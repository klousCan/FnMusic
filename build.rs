fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let sdk = std::path::PathBuf::from(
        std::env::var_os("ProgramFiles(x86)").expect("Windows SDK required"),
    )
    .join("Windows Kits/10/bin");
    let mut candidates: Vec<_> = std::fs::read_dir(sdk)
        .expect("Install Windows SDK")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("x64/rc.exe"))
        .filter(|path| path.is_file())
        .collect();
    candidates.sort();
    let compiler = candidates
        .last()
        .expect("Windows SDK resource compiler rc.exe required");
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let icon = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("assets/icon.ico");
    let rc = output.join("icon.rc");
    let res = output.join("icon.res");
    std::fs::write(
        &rc,
        format!(
            "1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    let status = std::process::Command::new(compiler)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(rc)
        .status()
        .unwrap();
    assert!(status.success(), "Failed to embed application icon");
    println!("cargo:rustc-link-arg-bin=fnmusic={}", res.display());
}
