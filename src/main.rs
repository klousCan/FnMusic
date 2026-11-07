#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod windows_app;

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_app::run() {
        use windows::{
            Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
            core::{HSTRING, w},
        };
        let message = HSTRING::from(format!("无法启动飞牛音乐。\n\n{error}"));
        unsafe {
            MessageBoxW(None, &message, w!("飞牛音乐"), MB_OK | MB_ICONERROR);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("FnMusic 桌面客户端需要 Windows。");
}
