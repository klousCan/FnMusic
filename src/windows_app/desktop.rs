use std::{path::Path, sync::mpsc, time::Duration};

use eframe::egui;
use sha2::{Digest, Sha256};
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use windows::{
    Foundation::{TimeSpan, TypedEventHandler},
    Media::{
        MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
        SystemMediaTransportControls, SystemMediaTransportControlsButton as Button,
        SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsTimelineProperties,
    },
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, WAIT_OBJECT_0},
        System::{
            Threading::{
                CreateEventW, EVENT_MODIFY_STATE, OpenEventW, SetEvent, WaitForSingleObject,
            },
            WinRT::ISystemMediaTransportControlsInterop,
        },
        UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, IsIconic, IsWindowVisible, PostMessageW, WM_PAINT,
        },
    },
    core::{HSTRING, Result, factory},
};

use fnmusic::library::Track;

// A kernel event can be signalled and waited on from any thread. The Arc in Vault
// keeps it alive until all background users have released the data-directory lock.
pub struct Activation(HANDLE);
unsafe impl Send for Activation {}
unsafe impl Sync for Activation {}
impl Activation {
    fn name(root: &Path) -> HSTRING {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let hash = Sha256::digest(root.to_string_lossy().to_lowercase().as_bytes());
        HSTRING::from(format!("Local\\FnMusic-{:x}", hash))
    }
    pub fn new(root: &Path) -> Result<Self> {
        unsafe { CreateEventW(None, false, false, &Self::name(root)).map(Self) }
    }
    pub fn signal(root: &Path) -> bool {
        // A second launch grants the first instance permission to take focus.
        if let Some(pid) = std::fs::read_to_string(root.join("process.id"))
            .ok()
            .and_then(|text| text.parse::<u32>().ok())
        {
            unsafe {
                let _ = AllowSetForegroundWindow(pid);
            }
        }
        for _ in 0..10 {
            if let Ok(event) = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &Self::name(root)) } {
                let success = unsafe { SetEvent(event).is_ok() };
                unsafe {
                    let _ = CloseHandle(event);
                }
                return success;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        false
    }
    pub fn take(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }
}
impl Drop for Activation {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Show,
    Toggle,
    Play,
    Pause,
    Stop,
    Previous,
    Next,
    Seek(u64),
    ClosePreference,
    Exit,
}

struct BackgroundWake {
    stop: mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl BackgroundWake {
    fn new(hwnd: HWND) -> std::io::Result<Self> {
        let address = hwnd.0 as isize;
        let (stop, receive) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("fnmusic-background-wake".into())
            .spawn(move || {
                let hwnd = HWND(address as *mut _);
                while matches!(
                    receive.recv_timeout(Duration::from_millis(200)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    // Windows does not generate WM_PAINT for hidden windows, so egui's
                    // redraw request alone cannot drive queue/media processing in the tray.
                    // Post onto the owning UI thread; never call its window procedure here.
                    unsafe {
                        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                            let _ = PostMessageW(
                                Some(hwnd),
                                WM_PAINT,
                                Default::default(),
                                Default::default(),
                            );
                        }
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for BackgroundWake {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct Desktop {
    wake: Option<BackgroundWake>,
    tray: Option<TrayIcon>,
    play: MenuItem,
    previous: MenuItem,
    next: MenuItem,
    background: CheckMenuItem,
    controls: Option<Controls>,
    receiver: mpsc::Receiver<Command>,
    last: Option<(Option<Track>, bool, bool)>,
}

impl Desktop {
    pub fn new(hwnd: HWND, ctx: egui::Context, close_to_tray: bool) -> (Self, Vec<String>) {
        let (send, receiver) = mpsc::channel();
        let menu = Menu::new();
        let show = MenuItem::new("显示飞牛音乐", true, None);
        let play = MenuItem::new("播放", false, None);
        let previous = MenuItem::new("上一首", false, None);
        let next = MenuItem::new("下一首", false, None);
        let background = CheckMenuItem::new("关闭窗口后继续播放", true, close_to_tray, None);
        let exit = MenuItem::new("退出飞牛音乐", true, None);
        let mut errors = Vec::new();
        let background_wake = match BackgroundWake::new(hwnd) {
            Ok(wake) => Some(wake),
            Err(_) => {
                errors.push("无法启动后台事件唤醒，关闭窗口将退出应用".into());
                None
            }
        };
        let result = menu.append_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &play,
            &previous,
            &next,
            &PredefinedMenuItem::separator(),
            &background,
            &exit,
        ]);
        let tray = result.map_err(|e| e.to_string()).and_then(|_| {
            let image = super::icons::window_icon().ok_or("无法读取托盘图标")?;
            let icon = tray_icon::Icon::from_rgba(image.rgba, image.width, image.height)
                .map_err(|e| e.to_string())?;
            TrayIconBuilder::new()
                .with_icon(icon)
                .with_tooltip("飞牛音乐")
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .build()
                .map_err(|e| e.to_string())
        });
        let tray = match tray {
            Ok(tray) => Some(tray),
            Err(err) => {
                errors.push(format!("系统托盘不可用：{err}，关闭窗口将退出应用"));
                None
            }
        };
        let ids = [
            show.id().clone(),
            play.id().clone(),
            previous.id().clone(),
            next.id().clone(),
            background.id().clone(),
            exit.id().clone(),
        ];
        let sender = send.clone();
        let wake = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let command = if event.id == ids[0] {
                Command::Show
            } else if event.id == ids[1] {
                Command::Toggle
            } else if event.id == ids[2] {
                Command::Previous
            } else if event.id == ids[3] {
                Command::Next
            } else if event.id == ids[4] {
                Command::ClosePreference
            } else if event.id == ids[5] {
                Command::Exit
            } else {
                return;
            };
            let _ = sender.send(command);
            wake.request_repaint();
        }));
        let sender = send.clone();
        let wake = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            ) {
                let _ = sender.send(Command::Show);
                wake.request_repaint();
            }
        }));
        let controls = match Controls::new(hwnd, send, ctx) {
            Ok(controls) => Some(controls),
            Err(err) => {
                errors.push(format!("Windows 媒体控制不可用：{err}"));
                None
            }
        };
        (
            Self {
                wake: background_wake,
                tray,
                play,
                previous,
                next,
                background,
                controls,
                receiver,
                last: None,
            },
            errors,
        )
    }

    pub fn commands(&self) -> Vec<Command> {
        self.receiver.try_iter().collect()
    }
    pub fn has_tray(&self) -> bool {
        self.tray.is_some() && self.wake.is_some()
    }
    pub fn sync(
        &mut self,
        track: Option<&Track>,
        playing: bool,
        position: u64,
        duration: u64,
        background: bool,
    ) {
        let state = (track.cloned(), playing, background);
        if self.last.as_ref() != Some(&state) {
            self.play.set_text(if playing { "暂停" } else { "播放" });
            self.play.set_enabled(track.is_some());
            self.previous.set_enabled(track.is_some());
            self.next.set_enabled(track.is_some());
            self.background.set_checked(background);
            if let Some(tray) = &self.tray {
                let title = track
                    .map(|t| format!("{} · {} — 飞牛音乐", t.title, t.artist))
                    .unwrap_or_else(|| "飞牛音乐".into());
                // Shell tooltip limit is 127 UTF-16 code units, including the terminator.
                let title =
                    String::from_utf16_lossy(&title.encode_utf16().take(126).collect::<Vec<_>>());
                let _ = tray.set_tooltip(Some(title));
            }
            if let Some(controls) = &self.controls {
                let _ = controls.metadata(track, playing);
            }
            self.last = Some(state);
        }
        if let Some(controls) = &self.controls {
            let _ = controls.timeline(position, duration);
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
        TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
    }
}

struct Controls {
    controls: SystemMediaTransportControls,
    button: i64,
    seek: i64,
}
impl Controls {
    fn new(hwnd: HWND, send: mpsc::Sender<Command>, ctx: egui::Context) -> Result<Self> {
        let interop: ISystemMediaTransportControlsInterop =
            factory::<SystemMediaTransportControls, _>()?;
        let controls: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd)? };
        let sender = send.clone();
        let wake = ctx.clone();
        let button = controls.ButtonPressed(&TypedEventHandler::<
            SystemMediaTransportControls,
            SystemMediaTransportControlsButtonPressedEventArgs,
        >::new(move |_, args| {
            if let Some(args) = args.as_ref() {
                let command = match args.Button()? {
                    Button::Play => Command::Play,
                    Button::Pause => Command::Pause,
                    Button::Stop => Command::Stop,
                    Button::Previous => Command::Previous,
                    Button::Next => Command::Next,
                    _ => return Ok(()),
                };
                let _ = sender.send(command);
                wake.request_repaint();
            }
            Ok(())
        }))?;
        let seek = controls.PlaybackPositionChangeRequested(&TypedEventHandler::<
            SystemMediaTransportControls,
            PlaybackPositionChangeRequestedEventArgs,
        >::new(move |_, args| {
            if let Some(args) = args.as_ref() {
                let _ = send.send(Command::Seek(
                    args.RequestedPlaybackPosition()?.Duration.max(0) as u64 / 10_000,
                ));
                ctx.request_repaint();
            }
            Ok(())
        }));
        let seek = match seek {
            Ok(token) => token,
            Err(err) => {
                let _ = controls.RemoveButtonPressed(button);
                return Err(err);
            }
        };
        let result = Self {
            controls,
            button,
            seek,
        };
        result.controls.SetIsPlayEnabled(true)?;
        result.controls.SetIsPauseEnabled(true)?;
        result.controls.SetIsStopEnabled(true)?;
        result.controls.SetIsPreviousEnabled(true)?;
        result.controls.SetIsNextEnabled(true)?;
        result.controls.SetIsEnabled(false)?;
        Ok(result)
    }
    fn metadata(&self, track: Option<&Track>, playing: bool) -> Result<()> {
        self.controls.SetIsEnabled(track.is_some())?;
        self.controls.SetPlaybackStatus(if track.is_none() {
            MediaPlaybackStatus::Closed
        } else if playing {
            MediaPlaybackStatus::Playing
        } else {
            MediaPlaybackStatus::Paused
        })?;
        let updater = self.controls.DisplayUpdater()?;
        updater.ClearAll()?;
        if let Some(track) = track {
            updater.SetType(MediaPlaybackType::Music)?;
            let music = updater.MusicProperties()?;
            music.SetTitle(&HSTRING::from(&track.title))?;
            music.SetArtist(&HSTRING::from(&track.artist))?;
            music.SetAlbumTitle(&HSTRING::from(&track.album))?;
        }
        updater.Update()
    }
    fn timeline(&self, position: u64, duration: u64) -> Result<()> {
        let span = |ms: u64| TimeSpan {
            Duration: ms.min(i64::MAX as u64 / 10_000) as i64 * 10_000,
        };
        let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
        timeline.SetStartTime(span(0))?;
        timeline.SetMinSeekTime(span(0))?;
        timeline.SetEndTime(span(duration))?;
        timeline.SetMaxSeekTime(span(duration))?;
        timeline.SetPosition(span(position.min(duration)))?;
        self.controls.UpdateTimelineProperties(&timeline)
    }
}
impl Drop for Controls {
    fn drop(&mut self) {
        let _ = self.controls.RemoveButtonPressed(self.button);
        let _ = self
            .controls
            .RemovePlaybackPositionChangeRequested(self.seek);
        let _ = self.controls.SetIsEnabled(false);
        if let Ok(updater) = self.controls.DisplayUpdater() {
            let _ = updater.ClearAll();
            let _ = updater.Update();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_activation_is_scoped_and_consumed_once() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let first = Activation::new(one.path()).unwrap();
        let second = Activation::new(two.path()).unwrap();
        assert!(Activation::signal(one.path()));
        assert!(first.take());
        assert!(!first.take());
        assert!(!second.take());
        drop(first);
        assert!(!Activation::signal(one.path()));
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop and native SMTC"]
    fn native_tray_and_system_media_commands() {
        use raw_window_handle::HasWindowHandle;
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        };
        use winit::platform::windows::EventLoopBuilderExtWindows as _;
        let _apartment = super::super::media::Apartment::new().unwrap();
        let event_loop = winit::event_loop::EventLoop::builder()
            .with_any_thread(true)
            .build()
            .unwrap();
        #[allow(deprecated)]
        let window = event_loop
            .create_window(winit::window::Window::default_attributes().with_visible(false))
            .unwrap();
        let raw_window_handle::RawWindowHandle::Win32(handle) =
            window.window_handle().unwrap().as_raw()
        else {
            panic!("Windows required")
        };
        let (mut desktop, errors) = Desktop::new(
            HWND(handle.hwnd.get() as *mut _),
            egui::Context::default(),
            true,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert!(desktop.has_tray());
        let title = format!("FnMusic system integration {}", std::process::id());
        let track = Track {
            title: title.clone(),
            artist: "测试艺人".into(),
            album: "测试专辑".into(),
            duration_ms: 30000,
            ..Default::default()
        };
        desktop.sync(Some(&track), true, 1000, 30000, true);
        let controls = &desktop.controls.as_ref().unwrap().controls;
        assert_eq!(
            controls.PlaybackStatus().unwrap(),
            MediaPlaybackStatus::Playing
        );
        assert_eq!(
            controls
                .DisplayUpdater()
                .unwrap()
                .MusicProperties()
                .unwrap()
                .Title()
                .unwrap(),
            title
        );
        let (done, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            unsafe {
                windows::Win32::System::WinRT::RoInitialize(
                    windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
                )
                .unwrap();
            }
            let _apartment = super::super::media::Apartment;
            let result = (|| -> Result<()> {
                let manager = windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()?;
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                loop {
                    for session in manager.GetSessions()? {
                        if session.TryGetMediaPropertiesAsync()?.get()?.Title()? == title {
                            assert!(session.TryPauseAsync()?.get()?);
                            assert!(session.TryChangePlaybackPositionAsync(50_000_000)?.get()?);
                            assert!(session.TrySkipNextAsync()?.get()?);
                            return Ok(());
                        }
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "SMTC session was not published"
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
            })();
            let _ = done.send(result);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut received = Vec::new();
        loop {
            unsafe {
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            received.extend(desktop.commands());
            if let Ok(result) = result.try_recv() {
                result.unwrap();
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "SMTC request timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        worker.join().unwrap();
        received.extend(desktop.commands());
        assert!(received.contains(&Command::Pause), "{received:?}");
        assert!(received.contains(&Command::Seek(5000)), "{received:?}");
        assert!(received.contains(&Command::Next), "{received:?}");
        desktop.sync(None, false, 0, 0, false);
        assert!(
            !desktop
                .controls
                .as_ref()
                .unwrap()
                .controls
                .IsEnabled()
                .unwrap()
        );
    }
}
