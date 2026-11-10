use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::{ffi::OsStringExt, fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use reqwest::{Client, StatusCode};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES: &str = "https://github.com/klousCan/FnMusic/releases";
const LATEST: &str = "https://api.github.com/repos/klousCan/FnMusic/releases/latest";
const MAX_EXE: u64 = 256 * 1024 * 1024;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub page: String,
    exe: String,
    manifest: String,
    size: u64,
    digest: Option<String>,
}

#[derive(Clone)]
pub struct Staged {
    pub path: PathBuf,
    pub version: String,
    digest: String,
}

#[derive(Deserialize)]
struct RemoteRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    state: String,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    version: String,
    target: String,
    asset_name: String,
    size: u64,
    sha256: String,
}

fn client() -> Result<Client> {
    Client::builder()
        .https_only(true)
        .user_agent(format!("FnMusic/{VERSION}"))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let trusted = attempt.url().scheme() == "https"
                && matches!(
                    attempt.url().host_str(),
                    Some(
                        "api.github.com"
                            | "github.com"
                            | "release-assets.githubusercontent.com"
                            | "objects.githubusercontent.com"
                    )
                );
            if trusted && attempt.previous().len() < 8 {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|_| "无法初始化更新连接".into())
}

async fn limited_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if response.status() == StatusCode::FORBIDDEN
        || response.status() == StatusCode::TOO_MANY_REQUESTS
    {
        return Err("GitHub 请求受限，请稍后重试或前往发布页面下载".into());
    }
    if !response.status().is_success() {
        return Err(format!("无法获取更新信息（HTTP {}）", response.status()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "更新连接已中断")? {
        if bytes.len() + chunk.len() > limit {
            return Err("更新信息超出大小限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub async fn check() -> Result<Option<Release>> {
    let response = client()?
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|_| "无法连接 GitHub，请检查网络后重试")?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let remote: RemoteRelease = serde_json::from_slice(&limited_body(response, 256 * 1024).await?)
        .map_err(|_| "无法读取版本信息")?;
    if remote.draft || remote.prerelease {
        return Ok(None);
    }
    let tag = &remote.tag_name;
    let version = Version::parse(tag.strip_prefix('v').ok_or("发布版本号格式不正确")?)
        .map_err(|_| "发布版本号格式不正确")?;
    if !version.pre.is_empty()
        || !version.build.is_empty()
        || version <= Version::parse(VERSION).unwrap()
    {
        return Ok(None);
    }
    let base = format!("{RELEASES}/download/{tag}/");
    let find = |name: &str| {
        remote
            .assets
            .iter()
            .find(|asset| asset.name == name && asset.state == "uploaded")
            .filter(|asset| asset.browser_download_url == format!("{base}{name}"))
    };
    let exe = find("fnmusic.exe").ok_or("新版本尚未提供自动更新包，请前往发布页面下载")?;
    let manifest = find("update.json").ok_or("新版本缺少更新信息，请前往发布页面下载")?;
    if exe.size == 0 || exe.size > MAX_EXE || manifest.size > 32 * 1024 {
        return Err("更新包大小不正确，请前往发布页面下载".into());
    }
    Ok(Some(Release {
        version: version.to_string(),
        notes: remote
            .body
            .as_deref()
            .unwrap_or("此版本暂无更新说明。")
            .chars()
            .take(16_000)
            .collect(),
        page: format!("{RELEASES}/tag/{tag}"),
        exe: exe.browser_download_url.clone(),
        manifest: manifest.browser_download_url.clone(),
        size: exe.size,
        digest: exe.digest.clone(),
    }))
}

pub async fn download(
    release: Release,
    root: PathBuf,
    cancel: CancellationToken,
    progress: impl Fn(u64, u64),
) -> Result<Staged> {
    let client = client()?;
    let response = client
        .get(&release.manifest)
        .send()
        .await
        .map_err(|_| "无法获取更新校验信息")?;
    let manifest: Manifest = serde_json::from_slice(&limited_body(response, 32 * 1024).await?)
        .map_err(|_| "更新校验信息格式不正确")?;
    if manifest.schema_version != 1
        || manifest.version != release.version
        || manifest.target != "windows-x64"
        || manifest.asset_name != "fnmusic.exe"
        || manifest.size != release.size
        || !valid_hash(&manifest.sha256)
    {
        return Err("更新校验信息不匹配，已取消更新".into());
    }
    let digest = manifest.sha256.to_ascii_lowercase();
    if release
        .digest
        .as_ref()
        .is_some_and(|value| !value.eq_ignore_ascii_case(&format!("sha256:{digest}")))
    {
        return Err("GitHub 文件校验与更新信息不一致，已取消更新".into());
    }
    fs::create_dir_all(&root).map_err(|_| "无法创建更新缓存目录")?;
    let directory = tempfile::Builder::new()
        .prefix("stage-")
        .tempdir_in(&root)
        .map_err(|_| "无法创建下载目录")?;
    let path = directory.path().join("fnmusic.exe");
    let mut file = File::create(&path).map_err(|_| "无法保存更新包")?;
    let mut response = tokio::select! {
        _ = cancel.cancelled() => return Err("已取消下载".into()),
        result = client.get(&release.exe).timeout(Duration::from_secs(600)).send() =>
            result.map_err(|_| "无法下载更新包，请稍后重试")?,
    };
    if !response.status().is_success() {
        return Err(format!("下载失败（HTTP {}）", response.status()));
    }
    let mut hasher = Sha256::new();
    let mut received = 0;
    let mut notified = Instant::now();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err("已取消下载".into()),
            result = response.chunk() => result.map_err(|_| "更新下载中断，请重试")?,
        };
        let Some(chunk) = chunk else { break };
        received += chunk.len() as u64;
        if received > release.size || received > MAX_EXE {
            return Err("更新包大小异常，已取消下载".into());
        }
        file.write_all(&chunk)
            .map_err(|_| "无法保存更新包，请检查磁盘空间")?;
        hasher.update(&chunk);
        if notified.elapsed() >= Duration::from_millis(150) {
            progress(received, release.size);
            notified = Instant::now();
        }
    }
    if received != release.size || format!("{:x}", hasher.finalize()) != digest {
        return Err("更新包完整性校验失败，请重新下载".into());
    }
    file.sync_all().map_err(|_| "无法写入更新包")?;
    drop(file);
    validate_exe(&path)?;
    let _ = directory.keep();
    progress(received, release.size);
    Ok(Staged {
        path,
        version: release.version,
        digest,
    })
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|_| "无法读取更新文件")?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|_| "无法读取更新文件")?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn validate_exe(path: &Path) -> Result<()> {
    let invalid = || "下载的文件不是有效的 Windows x64 程序".to_owned();
    let mut file = File::open(path).map_err(|_| invalid())?;
    let mut dos = [0_u8; 64];
    file.read_exact(&mut dos).map_err(|_| invalid())?;
    let offset = u32::from_le_bytes(dos[60..64].try_into().unwrap());
    if &dos[..2] != b"MZ" || offset > 1024 * 1024 {
        return Err(invalid());
    }
    file.seek(SeekFrom::Start(offset.into()))
        .map_err(|_| invalid())?;
    let mut pe = [0_u8; 6];
    file.read_exact(&mut pe).map_err(|_| invalid())?;
    if &pe[..4] != b"PE\0\0" || u16::from_le_bytes(pe[4..6].try_into().unwrap()) != 0x8664 {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Plan {
    target: PathBuf,
    incoming: PathBuf,
    data_root: PathBuf,
    parent_pid: u32,
    sha256: String,
    version: String,
}

fn background(command: &mut Command) -> &mut Command {
    command
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
}

pub fn launch_installer(staged: Staged, root: PathBuf) -> Result<()> {
    let target = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|_| "无法定位当前程序")?;
    let stage_dir = staged.path.parent().ok_or("更新目录无效")?;
    if hash_file(&staged.path)? != staged.digest {
        return Err("更新文件发生变化，请重新下载".into());
    }
    let directory = target.parent().ok_or("程序目录无效")?;
    // Prepare the complete replacement before asking the running application to exit.
    let incoming = directory.join(format!(
        ".fnmusic-{}.incoming",
        fnmusic::storage::random_id()
    ));
    let prepare = || -> Result<()> {
        let mut output = File::options()
            .write(true)
            .create_new(true)
            .open(&incoming)
            .map_err(|_| "程序所在目录不可写，请前往发布页面手动更新")?;
        let mut input = File::open(&staged.path).map_err(|_| "无法读取更新包")?;
        std::io::copy(&mut input, &mut output).map_err(|_| "无法写入新版程序，请检查磁盘空间")?;
        output.sync_all().map_err(|_| "无法写入新版程序")?;
        drop(output);
        if hash_file(&incoming)? != staged.digest {
            return Err("程序写入校验失败".into());
        }
        let plan = Plan {
            target,
            incoming: incoming.clone(),
            data_root: root.parent().ok_or("更新目录无效")?.to_path_buf(),
            parent_pid: std::process::id(),
            sha256: staged.digest,
            version: staged.version,
        };
        let plan_path = stage_dir.join("plan.json");
        fs::write(
            &plan_path,
            serde_json::to_vec(&plan).map_err(|_| "无法创建更新计划")?,
        )
        .map_err(|_| "无法创建更新计划")?;
        let mut child = background(
            Command::new(&staged.path)
                .arg("--fnmusic-apply-update")
                .arg(&plan_path),
        )
        .spawn()
        .map_err(|_| "无法启动更新程序，请手动下载更新")?;
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            if child.try_wait().map_err(|_| "无法读取更新状态")?.is_some() {
                return Err(fs::read_to_string(stage_dir.join("error.txt"))
                    .unwrap_or_else(|_| "更新程序未能启动".into()));
            }
            if stage_dir.join("ready").is_file() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err("等待更新程序超时，请重试".into())
    };
    let result = prepare();
    if result.is_err() {
        let _ = fs::remove_file(&incoming);
    }
    result
}

fn start_app(target: &Path, data_root: &Path, ack: Option<&Path>) -> Result<std::process::Child> {
    let mut command = Command::new(target);
    command
        .env("FNMUSIC_DATA_DIR", data_root)
        .env_remove("FNMUSIC_UPDATE_ACK");
    if let Some(ack) = ack {
        command.env("FNMUSIC_UPDATE_ACK", ack);
    }
    background(&mut command)
        .spawn()
        .map_err(|_| "无法重新启动程序".into())
}

fn apply(plan_path: &Path) -> Result<()> {
    use windows::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
        },
    };
    if fs::metadata(plan_path)
        .map_err(|_| "无法读取更新计划")?
        .len()
        > 16 * 1024
    {
        return Err("更新计划无效".into());
    }
    let bytes = fs::read(plan_path).map_err(|_| "无法读取更新计划")?;
    if bytes.len() > 16 * 1024 {
        return Err("更新计划无效".into());
    }
    let plan: Plan = serde_json::from_slice(&bytes).map_err(|_| "更新计划无效")?;
    let own = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|_| "更新程序路径无效")?;
    let stage_dir = plan_path
        .parent()
        .ok_or("更新目录无效")?
        .canonicalize()
        .map_err(|_| "更新目录无效")?;
    let updates = plan
        .data_root
        .join("updates")
        .canonicalize()
        .map_err(|_| "更新目录无效")?;
    let target = plan.target.canonicalize().map_err(|_| "当前程序路径无效")?;
    if own.parent() != Some(stage_dir.as_path())
        || stage_dir.parent() != Some(updates.as_path())
        || target == own
        || plan.incoming.parent() != target.parent()
        || !valid_hash(&plan.sha256)
        || plan.version != VERSION
        || plan.parent_pid == std::process::id()
        || hash_file(&own)? != plan.sha256
        || hash_file(&plan.incoming)? != plan.sha256
    {
        return Err("更新计划校验失败".into());
    }
    validate_exe(&plan.incoming)?;
    let parent = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            plan.parent_pid,
        )
    }
    .map_err(|_| "无法等待原程序退出")?;
    let mut buffer = vec![0_u16; 32768];
    let mut length = buffer.len() as u32;
    let queried = unsafe {
        QueryFullProcessImageNameW(
            parent,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    let parent_image = PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..length.min(buffer.len() as u32) as usize],
    ));
    if queried.is_err() || parent_image.canonicalize().ok().as_ref() != Some(&target) {
        let _ = unsafe { CloseHandle(parent) };
        return Err("待更新程序与原进程不一致，已取消更新".into());
    }
    if fs::write(stage_dir.join("ready"), b"ready").is_err() {
        let _ = unsafe { CloseHandle(parent) };
        return Err("无法通知程序准备更新".into());
    }
    let exited = unsafe { WaitForSingleObject(parent, 120_000) };
    let _ = unsafe { CloseHandle(parent) };
    if exited != WAIT_OBJECT_0 {
        return Err("等待原程序退出超时，未替换文件".into());
    }
    let backup =
        target.with_file_name(format!(".fnmusic-{}.backup", fnmusic::storage::random_id()));
    let ack = stage_dir.join("started.txt");
    let install = || -> Result<()> {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if fs::rename(&target, &backup).is_ok() {
                break;
            }
            if Instant::now() >= until {
                return Err("无法替换原程序，可能被其他进程占用".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        fs::rename(&plan.incoming, &target).map_err(|_| "无法安装新版程序")?;
        let mut child = start_app(&target, &plan.data_root, Some(&ack))?;
        let until = Instant::now() + Duration::from_secs(45);
        while Instant::now() < until {
            if fs::read_to_string(&ack).is_ok_and(|text| text == VERSION) {
                return Ok(());
            }
            match child.try_wait() {
                Ok(Some(_)) => return Err("新版程序未能启动".into()),
                Ok(None) => {}
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("无法读取新版启动状态".into());
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err("新版程序启动超时".into())
    };
    if let Err(error) = install() {
        if backup.exists() {
            let _ = fs::remove_file(&target);
            if fs::rename(&backup, &target).is_err() {
                let error = format!("{error}。旧版备份保留于：{}", backup.display());
                recovery_notice(&error);
                return Err(error);
            }
        }
        let _ = fs::remove_file(&plan.incoming);
        let _ = fs::write(
            updates.join("last-error.txt"),
            format!("更新未完成，已恢复旧版：{error}"),
        );
        if start_app(&target, &plan.data_root, None).is_err() {
            recovery_notice(&format!(
                "{error}。请重新打开原程序，或从 {RELEASES} 手动下载。"
            ));
        }
        return Err(error);
    }
    let _ = fs::remove_file(&backup);
    Ok(())
}

fn recovery_notice(message: &str) {
    use windows::{
        Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
        core::{HSTRING, w},
    };
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            w!("FnMusic 更新未完成"),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// This mode runs before the application lock or any UI is initialized.
pub fn handle_command() -> Result<bool> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--fnmusic-apply-update")) {
        return Ok(false);
    }
    let plan = PathBuf::from(args.next().ok_or("缺少更新计划")?);
    if args.next().is_some() {
        return Err("更新参数无效".into());
    }
    if let Err(error) = apply(&plan) {
        if let Some(directory) = plan.parent() {
            let _ = fs::write(directory.join("error.txt"), &error);
        }
        return Ok(true);
    }
    Ok(true)
}

pub fn acknowledge_startup(root: &Path, ack: &Path) {
    if ack.file_name().is_some_and(|name| name == "started.txt")
        && let (Ok(directory), Ok(updates)) = (
            ack.parent().unwrap_or(Path::new("")).canonicalize(),
            root.canonicalize(),
        )
        && directory.parent() == Some(updates.as_path())
    {
        let _ = fs::write(ack, VERSION);
    }
}

pub fn take_error(root: &Path) -> Option<String> {
    let path = root.join("last-error.txt");
    let result = fs::read_to_string(&path).ok();
    if result.is_some() {
        let _ = fs::remove_file(path);
    }
    result
}

pub fn cleanup_old_stages(root: &Path) {
    let Ok(root) = root.canonicalize() else {
        return;
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("stage-") {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.file_attributes() & 0x400 != 0
        {
            continue;
        }
        if metadata
            .modified()
            .ok()
            .and_then(|time| time.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(7 * 86400))
            && let Ok(path) = entry.path().canonicalize()
            && path.parent() == Some(root.as_path())
        {
            let _ = fs::remove_dir_all(path);
        }
    }
}
