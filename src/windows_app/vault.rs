use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    slice,
    sync::Arc,
};

use fnmusic::{
    api::{Credentials, Error, Result},
    playback::Queue,
    storage,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    },
    core::PCWSTR,
};

#[derive(Clone)]
pub struct Vault {
    root: PathBuf,
    _lock: Arc<File>,
    pub activation: Arc<super::desktop::Activation>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    Light,
    Dark,
    #[default]
    System,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    #[default]
    Red,
    Green,
    Blue,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnimationMode {
    On,
    System,
    Off,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Preferences {
    pub animation_mode: AnimationMode,
    pub close_to_tray: bool,
    pub color_mode: ColorMode,
    pub accent: Accent,
    pub wallpaper_path: Option<PathBuf>,
    pub wallpaper_opacity: f32,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            animation_mode: AnimationMode::On,
            close_to_tray: true,
            color_mode: ColorMode::System,
            accent: Accent::Red,
            wallpaper_path: None,
            wallpaper_opacity: 0.18,
        }
    }
}

impl Vault {
    #[cfg(test)]
    pub fn open() -> Result<Self> {
        Self::open_or_activate()?.ok_or_else(|| Error::Invalid("已显示正在运行的飞牛音乐".into()))
    }

    pub fn open_or_activate() -> Result<Option<Self>> {
        let root = if let Some(path) = std::env::var_os("FNMUSIC_DATA_DIR") {
            PathBuf::from(path)
        } else {
            PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or(Error::Storage)?).join("FnMusic")
        };
        std::fs::create_dir_all(&root).map_err(|_| Error::Storage)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("instance.lock"))
            .map_err(|_| Error::Storage)?;
        if lock.try_lock().is_err() {
            return if super::desktop::Activation::signal(&root) {
                Ok(None)
            } else {
                Err(Error::Invalid("已有实例正在启动，请稍后再试".into()))
            };
        }
        let activation = super::desktop::Activation::new(&root)
            .map_err(|_| Error::Invalid("无法创建实例唤醒事件".into()))?;
        let _ = storage::replace(
            &root.join("process.id"),
            std::process::id().to_string().as_bytes(),
        );
        Ok(Some(Self {
            root,
            _lock: Arc::new(lock),
            activation: Arc::new(activation),
        }))
    }

    pub fn preferences(&self) -> Preferences {
        std::fs::read(self.root.join("preferences.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save_preferences(&self, preferences: &Preferences) -> Result<()> {
        storage::replace(
            &self.root.join("preferences.json"),
            &serde_json::to_vec(preferences).map_err(|_| Error::Storage)?,
        )
        .map_err(|_| Error::Storage)
    }

    pub fn profiles(&self) -> PathBuf {
        self.root.join("webview")
    }

    pub fn load(&self) -> Result<Option<Credentials>> {
        let ciphertext = match std::fs::read(self.root.join("session.bin")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::Storage),
        };
        let plaintext = crypt(&ciphertext, false)?;
        let credentials: Credentials =
            serde_json::from_slice(&plaintext).map_err(|_| Error::Storage)?;
        if credentials.profile.len() != 32
            || !credentials.profile.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Storage);
        }
        Ok(Some(credentials))
    }

    pub fn save(&self, session: &Credentials) -> Result<()> {
        let plaintext = serde_json::to_vec(session).map_err(|_| Error::Storage)?;
        let ciphertext = crypt(&plaintext, true)?;
        storage::replace(&self.root.join("session.bin"), &ciphertext).map_err(|_| Error::Storage)
    }

    pub fn forget(&self) -> Result<()> {
        match std::fs::remove_file(self.root.join("session.bin")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Error::Storage),
        }
    }

    pub fn queue_file(&self, session: &Credentials) -> PathBuf {
        let mut digest = Sha256::new();
        digest.update(session.base.as_str());
        digest.update([0]);
        digest.update(&session.account_id);
        self.root
            .join("queues")
            .join(format!("{:x}.json", digest.finalize()))
    }
}

pub fn read_queue(path: &Path) -> Queue {
    let mut queue: Queue = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    queue.restore();
    queue
}

pub fn save_queue(path: &Path, queue: &Queue) -> Result<()> {
    let bytes = serde_json::to_vec(queue).map_err(|_| Error::Storage)?;
    storage::replace(path, &bytes).map_err(|_| Error::Storage)
}

fn crypt(bytes: &[u8], protect: bool) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into().map_err(|_| Error::Storage)?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // DPAPI allocates the output with LocalAlloc. Neither call mutates the input.
    unsafe {
        if protect {
            CryptProtectData(
                &input,
                PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .map_err(|_| Error::Storage)?;
        if output.pbData.is_null() {
            return Err(Error::Storage);
        }
        let result = slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(result)
    }
}

#[cfg(test)]
pub fn acceptance_credentials(root: &Path) -> Result<Credentials> {
    let ciphertext = std::fs::read(root.join("session.bin")).map_err(|_| Error::Storage)?;
    serde_json::from_slice(&crypt(&ciphertext, false)?).map_err(|_| Error::Storage)
}

#[cfg(test)]
mod tests {
    #[test]
    fn old_close_only_preferences_keep_new_appearance_defaults() {
        let preferences: super::Preferences =
            serde_json::from_str(r#"{"close_to_tray":false}"#).unwrap();
        assert!(!preferences.close_to_tray);
        assert_eq!(preferences.color_mode, super::ColorMode::System);
        assert_eq!(preferences.accent, super::Accent::Red);
        assert!(preferences.wallpaper_path.is_none());
    }

    #[test]
    fn dpapi_round_trip() {
        let secret = b"demo credential";
        let encrypted = super::crypt(secret, true).unwrap();
        assert!(
            !encrypted
                .windows(secret.len())
                .any(|window| window == secret)
        );
        assert_eq!(super::crypt(&encrypted, false).unwrap(), secret);
    }
}
