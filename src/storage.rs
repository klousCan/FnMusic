use std::{io, io::Write, path::Path};

pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub fn random_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}
