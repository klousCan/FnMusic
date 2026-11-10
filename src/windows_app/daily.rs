use std::path::Path;

use fnmusic::{
    api::{Error, Result, Session},
    library,
    recommendation::{self, Cache, DAILY_SIZE, Day},
    storage,
};

pub fn today() -> Day {
    let time = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    Day {
        year: time.wYear,
        month: time.wMonth,
        day: time.wDay,
    }
}

pub async fn load(session: &Session, path: &Path, day: Day) -> Result<Cache> {
    let cache = std::fs::metadata(path)
        .ok()
        .filter(|file| file.len() <= 1024 * 1024)
        .and_then(|_| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Cache>(&bytes).ok())
        .filter(|cache| cache.current.tracks.len() <= DAILY_SIZE)
        .unwrap_or_default();
    if cache.ready(day) {
        return Ok(cache);
    }
    let account = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or(Error::Storage)?;
    let mut candidates =
        library::daily_candidates(session, recommendation::seed(account, day)).await?;
    let tracks = std::mem::take(&mut candidates.tracks);
    let mut cache = cache.generate(day, account, tracks);
    library::daily_covers(session, &mut cache.current.tracks, &candidates).await;
    Ok(cache)
}

pub fn save(path: &Path, cache: &Cache) -> Result<()> {
    storage::replace(
        path,
        &serde_json::to_vec(cache).map_err(|_| Error::Storage)?,
    )
    .map_err(|_| Error::Storage)
}
