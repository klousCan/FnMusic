use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use eframe::egui::emath::GuiRounding as _;
use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, RichText, TextureHandle, Vec2,
};
use fnmusic::{
    api::{self, Api, Credentials, Error, Session},
    library::{self, Collection, Kind, Listing, Page, Track},
    playback::{self, LyricLine, Queue, Repeat},
    relay::Relay,
    storage::random_id,
};
use tokio::{runtime::Runtime, sync::Semaphore};

use super::{
    artwork,
    desktop::{Command, Desktop},
    experience::{LyricFollow, SleepTimer},
    icons::{self, Icon},
    media::{Media, MediaEvent},
    oauth::{Broker, Hooks},
    track_table::{Column, Table},
    vault::{self, Accent, ColorMode, Preferences, Vault},
    widgets,
};

pub(super) const ACCENT: Color32 = Color32::from_rgb(235, 54, 87);
const COVER_CAPACITY: usize = 240;
const SIDEBAR_BREAKPOINT: f32 = 980.0;
const PANEL_BREAKPOINT: f32 = 1240.0;
const HISTORY_LIMIT: usize = 40;

struct HistoryEntry {
    page: Page,
    listing: Listing,
    number: u32,
    scroll: f32,
    table: Table,
}

struct Connection {
    credentials: Credentials,
    session: Arc<Session>,
    queue_path: PathBuf,
    queue: Queue,
}

enum Event {
    Connected(api::Result<Box<Connection>>),
    LoggedOut(api::Result<()>),
    Page(u64, u32, api::Result<Listing>),
    Lyrics(u64, api::Result<String>),
    Cover(u64, String, Option<(egui::ColorImage, Color32)>),
    Favorite(u64, String, bool, api::Result<()>),
    SidebarPlaylists(u64, api::Result<Listing>),
    PlaylistUpdated(u64, Collection, api::Result<()>),
}

#[derive(Clone)]
struct Events {
    send: mpsc::Sender<Event>,
    ctx: egui::Context,
}

impl Events {
    fn send(&self, event: Event) {
        let _ = self.send.send(event);
        self.ctx.request_repaint();
    }
}

enum Cover {
    Ready(TextureHandle, u64, Color32),
    Pending,
    Missing(Instant),
}

#[derive(PartialEq, Eq)]
enum Panel {
    None,
    Queue,
    #[allow(dead_code)]
    Lyrics,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PlaybackQuality {
    #[default]
    Auto,
    Standard,
    Lossless,
    HiRes,
}

impl PlaybackQuality {
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动",
            Self::Standard => "标准",
            Self::Lossless => "无损",
            Self::HiRes => "Hi-Res",
        }
    }

    fn parameter(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Standard => Some("standard"),
            Self::Lossless => Some("lossless"),
            Self::HiRes => Some("hires"),
        }
    }
}

#[derive(Clone, Copy)]
enum TrackAction {
    Queue,
    Next,
    Favorite(bool),
}

struct TrackDrag {
    session: u64,
    tracks: Vec<Track>,
}

pub struct MusicApp {
    desktop: Option<Desktop>,
    close_to_tray: bool,
    preferences: Preferences,
    last_system_theme: Option<egui::Theme>,
    appearance_open: bool,
    exiting: bool,
    window_title: String,
    runtime: Runtime,
    relay: Relay,
    api: Api,
    vault: Vault,
    browser: Broker,
    events: Events,
    receive: mpsc::Receiver<Event>,
    session: Option<Arc<Session>>,
    saved: Option<Credentials>,
    server_input: String,
    auth_busy: bool,
    logging_out: bool,
    close_after_logout: bool,
    session_epoch: u64,
    page: Page,
    previous: Vec<HistoryEntry>,
    forward: Vec<HistoryEntry>,
    library_scroll: f32,
    restore_scroll: Option<f32>,
    listing: Listing,
    track_table: Table,
    page_number: u32,
    page_epoch: u64,
    loading: bool,
    page_error: Option<String>,
    notice: Option<String>,
    search: String,
    search_due: Option<Instant>,
    search_focus: bool,
    sidebar_collapsed: bool,
    sidebar_playlists: Vec<Collection>,
    playlist_pending: HashSet<String>,
    panel: Panel,
    lyrics_detail: bool,
    detail_lyrics_enabled: bool,
    covers: HashMap<String, Cover>,
    cover_limit: Arc<Semaphore>,
    cover_clock: u64,
    favorite_pending: HashSet<String>,
    favorite_confirmed: HashMap<String, bool>,
    queue: Queue,
    queue_path: Option<PathBuf>,
    saved_at: Instant,
    dirty: bool,
    media: Option<Media>,
    media_open: bool,
    playing: bool,
    playback_quality: PlaybackQuality,
    position_hold: Instant,
    seek_preview: Option<u64>,
    volume_before_mute: f32,
    track_epoch: u64,
    lyrics: Vec<LyricLine>,
    lyrics_loading: bool,
    lyric_error: Option<String>,
    last_lyric: Option<usize>,
    detail_follow: LyricFollow,
    sidebar_follow: LyricFollow,
    sleep_timer: SleepTimer,
    locate_queue: bool,
}

impl MusicApp {
    pub fn install_desktop(&mut self, hwnd: windows::Win32::Foundation::HWND) {
        let (desktop, errors) = Desktop::new(hwnd, self.events.ctx.clone(), self.close_to_tray);
        self.desktop = Some(desktop);
        if !errors.is_empty() {
            self.notice = Some(errors.join("；"));
        }
    }

    fn set_close_preference(&mut self, enabled: bool) {
        self.preferences.close_to_tray = enabled;
        match self.vault.save_preferences(&self.preferences) {
            Ok(()) => self.close_to_tray = enabled,
            Err(_) => self.notice = Some("关闭窗口的设置保存失败".into()),
        }
    }

    fn accent(&self) -> Color32 {
        match self.preferences.accent {
            Accent::Red => ACCENT,
            Accent::Green => Color32::from_rgb(42, 133, 104),
            Accent::Blue => Color32::from_rgb(48, 119, 201),
        }
    }

    fn apply_appearance(&self, ctx: &egui::Context) {
        let use_dark = match self.preferences.color_mode {
            ColorMode::Light => false,
            ColorMode::Dark => true,
            ColorMode::System => ctx
                .system_theme()
                .is_some_and(|theme| theme == egui::Theme::Dark),
        };
        let mut visuals = if use_dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let accent = self.accent();
        visuals.hyperlink_color = if use_dark {
            accent
        } else {
            accent.lerp_to_gamma(Color32::BLACK, 0.18)
        };
        visuals.selection.bg_fill = accent.gamma_multiply(if use_dark { 0.28 } else { 0.14 });
        visuals.selection.stroke.color = visuals.text_color();
        visuals.widgets.active.bg_fill = accent.gamma_multiply(0.84);
        visuals.widgets.hovered.bg_stroke.color = accent.gamma_multiply(0.65);
        visuals.menu_corner_radius = CornerRadius::same(12);
        super::motion::set_theme(ctx, visuals);
        ctx.style_mut(|style| {
            style
                .text_styles
                .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
            // Keep scrolling understated: the default wide, opaque handle is
            // especially distracting in narrow queue and lyrics popovers.
            let scroll = &mut style.spacing.scroll;
            scroll.floating = true;
            scroll.bar_width = 6.0;
            scroll.floating_width = 3.0;
            scroll.floating_allocated_width = 0.0;
            scroll.handle_min_length = 30.0;
            scroll.foreground_color = false;
            scroll.dormant_background_opacity = 0.0;
            scroll.active_background_opacity = 0.0;
            scroll.interact_background_opacity = 0.16;
            scroll.dormant_handle_opacity = 0.0;
            scroll.active_handle_opacity = 0.38;
            scroll.interact_handle_opacity = 0.72;
        });
    }

    fn refresh_system_appearance(&mut self, ctx: &egui::Context) {
        if self.preferences.color_mode != ColorMode::System {
            return;
        }
        let system_theme = ctx.system_theme();
        if self.last_system_theme != system_theme {
            self.last_system_theme = system_theme;
            self.apply_appearance(ctx);
        }
    }

    fn save_appearance(&mut self, ctx: &egui::Context) {
        if self.vault.save_preferences(&self.preferences).is_err() {
            self.notice = Some("外观设置保存失败".into());
        }
        self.close_to_tray = self.preferences.close_to_tray;
        self.apply_appearance(ctx);
    }

    fn appearance_window(&mut self, ctx: &egui::Context) {
        if !self.appearance_open {
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.appearance_open = false;
            return;
        }
        let mut close = false;
        egui::Window::new("外观")
            .id(egui::Id::new("appearance-settings"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .movable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size(Vec2::new(536.0, 352.0))
            .frame(
                Frame::window(&ctx.style())
                    .corner_radius(18)
                    .inner_margin(22),
            )
            .show(ctx, |ui| {
                super::motion::popup(ui);
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    let badge = ui
                        .allocate_exact_size(Vec2::splat(38.0), egui::Sense::hover())
                        .0;
                    ui.painter().rect_filled(
                        badge,
                        12,
                        ui.visuals().hyperlink_color.gamma_multiply(0.09),
                    );
                    icons::glyph(
                        ui,
                        badge.center(),
                        Icon::Palette,
                        ui.visuals().hyperlink_color,
                    );
                    ui.add_space(5.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("外观").size(20.0).strong());
                        ui.label(RichText::new("选择显示模式与主题色").size(12.0).weak());
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        close = icons::small(ui, Icon::WindowClose, "关闭外观设置（Esc）", false)
                            .clicked();
                    });
                });
                ui.add_space(14.0);
                ui.label(RichText::new("显示模式").size(13.0).strong());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let width = (ui.available_width() - 20.0) / 3.0;
                    for (mode, icon, label, dark) in [
                        (ColorMode::System, Icon::Monitor, "跟随系统", None),
                        (ColorMode::Light, Icon::Sun, "浅色", Some(false)),
                        (ColorMode::Dark, Icon::Moon, "深色", Some(true)),
                    ] {
                        if widgets::mode_card(
                            ui,
                            icon,
                            label,
                            dark,
                            self.preferences.color_mode == mode,
                            width,
                        )
                        .clicked()
                            && self.preferences.color_mode != mode
                        {
                            self.preferences.color_mode = mode;
                            self.save_appearance(ctx);
                        }
                    }
                });
                ui.add_space(14.0);
                ui.label(RichText::new("主题色").size(13.0).strong());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let width = (ui.available_width() - 20.0) / 3.0;
                    for (accent, label, color) in [
                        (Accent::Red, "经典红", ACCENT),
                        (Accent::Green, "墨绿", Color32::from_rgb(42, 133, 104)),
                        (Accent::Blue, "海蓝", Color32::from_rgb(48, 119, 201)),
                    ] {
                        if widgets::accent_card(
                            ui,
                            label,
                            color,
                            self.preferences.accent == accent,
                            width,
                        )
                        .clicked()
                            && self.preferences.accent != accent
                        {
                            self.preferences.accent = accent;
                            self.save_appearance(ctx);
                        }
                    }
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let rect = ui
                        .allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover())
                        .0;
                    icons::glyph(
                        ui,
                        rect.center(),
                        Icon::Check,
                        ui.visuals().weak_text_color(),
                    );
                    ui.label(
                        RichText::new("修改立即生效，并自动保存到下次启动")
                            .size(12.0)
                            .weak(),
                    );
                });
            });
        if close {
            self.appearance_open = false;
        }
    }

    fn system_command(&mut self, command: Command, ctx: &egui::Context) {
        match command {
            Command::Show => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            Command::Exit => {
                self.exiting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Command::ClosePreference => self.set_close_preference(!self.close_to_tray),
            _ if self.session.is_none() => {}
            Command::Toggle => self.toggle_playing(),
            Command::Play if !self.playing => self.toggle_playing(),
            Command::Pause if self.playing => self.toggle_playing(),
            Command::Stop => {
                self.stop_media();
                self.seek(0);
            }
            Command::Previous => self.previous_track(),
            Command::Next => self.next_track(false),
            Command::Seek(position) => self.seek(position.min(self.duration())),
            _ => {}
        }
    }

    fn update_desktop(&mut self, ctx: &egui::Context) {
        if self.vault.activation.take() {
            self.system_command(Command::Show, ctx);
        }
        if let Some(desktop) = &self.desktop {
            for command in desktop.commands() {
                self.system_command(command, ctx);
            }
        }
        let title = self
            .queue
            .current()
            .map(|track| format!("{} · {} — 飞牛音乐", track.title, track.artist))
            .unwrap_or_else(|| "飞牛音乐 · FnMusic".into());
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
        let duration = self.duration();
        if let Some(desktop) = &mut self.desktop {
            desktop.sync(
                self.queue.current(),
                self.playing,
                self.queue.position_ms,
                duration,
                self.close_to_tray,
            );
        }
        // Also wakes a hidden idle window when another process signals activation.
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn account_menu(&mut self, ui: &mut egui::Ui, compact: bool) {
        let name = self
            .saved
            .as_ref()
            .map(|a| a.username.as_str())
            .unwrap_or("账户");
        let initial = name.chars().next().unwrap_or('用');
        let name: String = name.chars().take(12).collect();
        let button = egui::Button::new("")
            .min_size(Vec2::new(ui.available_width(), 44.0))
            .corner_radius(10.0)
            .fill(ui.visuals().faint_bg_color);
        let response = egui::menu::menu_custom_button(ui, button, |ui| {
            super::motion::popup(ui);
            ui.set_width(248.0);
            ui.spacing_mut().item_spacing.y = 3.0;
            if let Some(account) = &self.saved {
                ui.add_space(5.0);
                ui.label(RichText::new(&account.username).strong().size(16.0));
                ui.label(
                    RichText::new(account.base.origin().ascii_serialization())
                        .small()
                        .weak(),
                );
                ui.add_space(5.0);
            }
            ui.separator();
            let mode = match self.preferences.color_mode {
                ColorMode::System => "跟随系统",
                ColorMode::Light => "浅色",
                ColorMode::Dark => "深色",
            };
            let color = match self.preferences.accent {
                Accent::Red => "经典红",
                Accent::Green => "墨绿",
                Accent::Blue => "海蓝",
            };
            if widgets::action(
                ui,
                Icon::Palette,
                "外观与主题色…",
                Some(&format!("{mode} · {color}")),
                widgets::Row::Action,
            )
            .clicked()
            {
                ui.close_menu();
                self.appearance_open = true;
            }
            ui.separator();
            widgets::submenu(ui, Icon::Sparkles, "动画效果", |ui| {
                widgets::menu(ui, "动画效果");
                use super::vault::AnimationMode;
                for (mode, icon, label, description) in [
                    (
                        AnimationMode::On,
                        Icon::Sparkles,
                        "开启",
                        "显示完整的界面动效",
                    ),
                    (
                        AnimationMode::System,
                        Icon::Monitor,
                        "跟随系统",
                        "遵循 Windows 动画设置",
                    ),
                    (AnimationMode::Off, Icon::Power, "关闭", "界面变化立即生效"),
                ] {
                    if widgets::action(
                        ui,
                        icon,
                        label,
                        Some(description),
                        widgets::Row::Selected(self.preferences.animation_mode == mode),
                    )
                    .clicked()
                    {
                        self.preferences.animation_mode = mode;
                        self.save_appearance(ui.ctx());
                        super::motion::configure(ui.ctx(), mode);
                        ui.close_menu();
                    }
                }
            });
            widgets::submenu(ui, Icon::WindowClose, "关闭窗口时", |ui| {
                widgets::menu(ui, "关闭窗口时");
                if widgets::action(
                    ui,
                    Icon::Tray,
                    "留在托盘，继续播放",
                    Some("隐藏窗口，音乐继续播放"),
                    widgets::Row::Selected(self.close_to_tray),
                )
                .clicked()
                {
                    self.set_close_preference(true);
                    ui.close_menu();
                }
                if widgets::action(
                    ui,
                    Icon::Power,
                    "直接退出应用",
                    Some("关闭窗口时停止播放并退出"),
                    widgets::Row::Selected(!self.close_to_tray),
                )
                .clicked()
                {
                    self.set_close_preference(false);
                    ui.close_menu();
                }
            });
            self.sleep_menu(ui);
            ui.separator();
            if widgets::action(
                ui,
                Icon::Logout,
                "退出登录",
                Some("退出当前 NAS 账号"),
                widgets::Row::Destructive,
            )
            .clicked()
            {
                ui.close_menu();
                self.disconnect(true);
            }
            if widgets::action(
                ui,
                Icon::Power,
                "退出飞牛音乐",
                Some("停止播放并关闭应用"),
                widgets::Row::Action,
            )
            .clicked()
            {
                ui.close_menu();
                self.system_command(Command::Exit, ui.ctx());
            }
        })
        .response
        .on_hover_text("账户与应用设置");
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "账户与应用设置")
        });
        let rect = response.rect;
        let center = egui::pos2(
            if compact {
                rect.center().x
            } else {
                rect.left() + 22.0
            },
            rect.center().y,
        );
        let accent = ui.visuals().hyperlink_color;
        ui.painter()
            .circle_filled(center, 14.0, accent.gamma_multiply(0.14));
        ui.painter().text(
            center,
            egui::Align2::CENTER_CENTER,
            initial,
            egui::FontId::proportional(14.0),
            accent,
        );
        if !compact {
            let mut job = egui::text::LayoutJob::simple_singleline(
                name,
                egui::FontId::proportional(13.0),
                ui.visuals().text_color(),
            );
            job.wrap.max_width = (rect.width() - 63.0).max(0.0);
            job.wrap.max_rows = 1;
            let galley = ui.painter().layout_job(job);
            ui.painter().galley(
                egui::pos2(rect.left() + 43.0, center.y - galley.size().y * 0.5),
                galley,
                ui.visuals().text_color(),
            );
            let point = egui::pos2(rect.right() - 13.0, center.y);
            ui.painter().line_segment(
                [point + Vec2::new(-3.0, 1.5), point + Vec2::new(0.0, -1.5)],
                (1.2, ui.visuals().weak_text_color()),
            );
            ui.painter().line_segment(
                [point + Vec2::new(0.0, -1.5), point + Vec2::new(3.0, 1.5)],
                (1.2, ui.visuals().weak_text_color()),
            );
        }
    }

    fn primary_action(&self, ui: &mut egui::Ui, label: &str) -> egui::Response {
        let size = Vec2::new(116.0, 34.0);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        let accent = ui.visuals().hyperlink_color;
        let fill = if response.is_pointer_button_down_on() {
            accent.gamma_multiply(0.74)
        } else if response.hovered() {
            accent.gamma_multiply(1.10)
        } else {
            accent
        };
        ui.painter().rect_filled(rect, 17.0, fill);
        icons::glyph(
            ui,
            egui::pos2(rect.left() + 17.0, rect.center().y),
            Icon::Play,
            Color32::WHITE,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 30.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(14.0),
            Color32::WHITE,
        );
        response.on_hover_text(label)
    }

    #[cfg(test)]
    pub fn new(ctx: egui::Context) -> api::Result<Self> {
        Self::with_vault(ctx, Vault::open()?)
    }

    pub fn with_vault(ctx: egui::Context, vault: Vault) -> api::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::Invalid("无法启动后台任务".into()))?;
        let relay =
            Relay::start(&runtime).map_err(|_| Error::Invalid("无法启动本地播放服务".into()))?;
        let browser = Broker::start(vault.profiles())?;
        let (send, receive) = mpsc::channel();
        let now = Instant::now();
        let preferences = vault.preferences();
        let mut app = Self {
            desktop: None,
            close_to_tray: preferences.close_to_tray,
            preferences: preferences.clone(),
            last_system_theme: None,
            appearance_open: false,
            exiting: false,
            window_title: String::new(),
            runtime,
            relay,
            api: Api::new()?,
            vault,
            browser,
            events: Events { send, ctx },
            receive,
            session: None,
            saved: None,
            server_input: String::new(),
            auth_busy: false,
            logging_out: false,
            close_after_logout: false,
            session_epoch: 0,
            page: Page::Home,
            previous: Vec::new(),
            forward: Vec::new(),
            library_scroll: 0.0,
            restore_scroll: None,
            listing: Listing::default(),
            track_table: Table::default(),
            page_number: 0,
            page_epoch: 0,
            loading: false,
            page_error: None,
            notice: None,
            search: String::new(),
            search_due: None,
            search_focus: false,
            sidebar_collapsed: false,
            sidebar_playlists: Vec::new(),
            playlist_pending: HashSet::new(),
            panel: Panel::None,
            lyrics_detail: false,
            detail_lyrics_enabled: true,
            covers: HashMap::new(),
            cover_limit: Arc::new(Semaphore::new(3)),
            cover_clock: 0,
            favorite_pending: HashSet::new(),
            favorite_confirmed: HashMap::new(),
            queue: Queue::default(),
            queue_path: None,
            saved_at: now,
            dirty: false,
            media: None,
            media_open: false,
            playing: false,
            playback_quality: PlaybackQuality::Auto,
            position_hold: now,
            seek_preview: None,
            volume_before_mute: 0.8,
            track_epoch: 0,
            lyrics: Vec::new(),
            lyrics_loading: false,
            lyric_error: None,
            last_lyric: None,
            detail_follow: LyricFollow::default(),
            sidebar_follow: LyricFollow::default(),
            sleep_timer: SleepTimer::Off,
            locate_queue: false,
        };
        app.apply_appearance(&app.events.ctx);
        match app.vault.load() {
            Ok(Some(credentials)) => {
                app.server_input = credentials.base.to_string();
                app.saved = Some(credentials);
                app.connect(true);
            }
            Ok(None) => {}
            Err(_) => app.notice = Some("无法读取已保存的会话，请重新授权".into()),
        }
        Ok(app)
    }

    fn connect(&mut self, restore: bool) {
        if self.auth_busy {
            return;
        }
        let base = match api::normalize_base(&self.server_input) {
            Ok(base) => base,
            Err(err) => {
                self.notice = Some(err.to_string());
                return;
            }
        };
        let api = self.api.clone();
        let vault = self.vault.clone();
        let browser = self.browser.clone();
        let events = self.events.clone();
        let saved = self.saved.clone();
        self.auth_busy = true;
        self.notice = None;
        self.runtime.spawn(async move {
            let result = async {
                let credentials = if restore {
                    vault.load()?.ok_or(Error::Expired)?
                } else {
                    let request = api.authorization(&base).await?;
                    let same = saved.as_ref().filter(|old| old.base == base);
                    let profile = same
                        .map(|old| old.profile.clone())
                        .unwrap_or_else(random_id);
                    let device = same
                        .map(|old| old.device_id.clone())
                        .unwrap_or_else(random_id);
                    let attempt = async {
                        let code = browser.authorize(request, profile.clone(), false).await?;
                        let credentials = api.exchange(base, device, profile.clone(), code).await?;
                        vault.save(&credentials)?;
                        Ok::<_, Error>(credentials)
                    }
                    .await;
                    match &attempt {
                        Ok(_) => {
                            if let Some(old) = saved.as_ref().filter(|old| old.profile != profile) {
                                let _ = browser.clear(old.profile.clone()).await;
                            }
                        }
                        Err(_) if same.is_none() => {
                            let _ = browser.clear(profile).await;
                        }
                        Err(_) => {}
                    }
                    attempt?
                };
                let session = Arc::new(Session::new(
                    api,
                    credentials.clone(),
                    Box::new(Hooks {
                        browser,
                        vault: vault.clone(),
                    }),
                ));
                if restore {
                    session.validate().await?;
                }
                let queue_path = vault.queue_file(&credentials);
                let queue = vault::read_queue(&queue_path);
                Ok(Box::new(Connection {
                    credentials,
                    session,
                    queue_path,
                    queue,
                }))
            }
            .await;
            events.send(Event::Connected(result));
        });
    }

    fn disconnect(&mut self, explicit: bool) {
        self.persist();
        let session = self.session.take();
        self.stop_media();
        self.session_epoch += 1;
        self.page_epoch += 1;
        self.track_epoch += 1;
        self.previous.clear();
        self.forward.clear();
        self.search.clear();
        self.search_due = None;
        self.search_focus = false;
        self.sidebar_playlists.clear();
        self.playlist_pending.clear();
        self.panel = Panel::None;
        self.lyrics_detail = false;
        self.listing = Listing::default();
        self.track_table = Table::default();
        self.covers.clear();
        self.sleep_timer = SleepTimer::Off;
        self.detail_follow.resume();
        self.sidebar_follow.resume();
        self.locate_queue = false;
        self.favorite_pending.clear();
        self.favorite_confirmed.clear();
        self.lyrics.clear();
        self.queue = Queue::default();
        self.volume_before_mute = self.queue.volume;
        self.queue_path = None;
        self.dirty = false;

        if explicit {
            self.auth_busy = true;
            self.logging_out = true;
            let saved = self.saved.clone();
            let vault = self.vault.clone();
            let browser = self.browser.clone();
            let events = self.events.clone();
            self.runtime.spawn(async move {
                let revoke = async {
                    if let Some(session) = session {
                        session.logout().await;
                    }
                };
                let clear = async {
                    if let Some(credentials) = saved {
                        return browser.clear(credentials.profile).await;
                    }
                    Ok(())
                };
                let (_, clear) = tokio::join!(revoke, clear);
                let local = vault.forget();
                events.send(Event::LoggedOut(local.and(clear)));
            });
        } else {
            self.notice = Some(Error::Expired.to_string());
        }
    }

    fn open_page(&mut self, page: Page, remember: bool) {
        let changed = self.page != page;
        if remember && self.page != page {
            let entry = self.take_history_entry();
            if self.previous.len() == HISTORY_LIMIT {
                self.previous.remove(0);
            }
            self.previous.push(entry);
            self.forward.clear();
        }
        if changed {
            self.track_table = Table::default();
        }
        self.page = page;
        self.page_epoch += 1;
        self.listing = Listing::default();
        self.page_number = 0;
        self.library_scroll = 0.0;
        self.restore_scroll = None;
        self.load_page(1);
    }

    fn take_history_entry(&mut self) -> HistoryEntry {
        HistoryEntry {
            page: self.page.clone(),
            listing: std::mem::take(&mut self.listing),
            number: self.page_number,
            scroll: self.restore_scroll.unwrap_or(self.library_scroll),
            table: std::mem::take(&mut self.track_table),
        }
    }

    fn travel(&mut self, forward: bool) {
        let destination = if forward {
            self.forward.pop()
        } else {
            self.previous.pop()
        };
        let Some(destination) = destination else {
            return;
        };
        let current = self.take_history_entry();
        if forward {
            self.previous.push(current);
        } else {
            self.forward.push(current);
        }
        self.page = destination.page;
        self.search = match &self.page {
            Page::Search(query) => query.clone(),
            _ => String::new(),
        };
        self.search_due = None;
        self.page_epoch += 1;
        self.loading = false;
        self.page_error = None;
        self.page_number = destination.number;
        self.listing = destination.listing;
        self.track_table = destination.table;
        self.library_scroll = destination.scroll;
        self.restore_scroll = Some(destination.scroll);
        if self.page_number == 0 {
            self.load_page(1);
        }
    }

    fn load_page(&mut self, number: u32) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.loading = true;
        self.page_error = None;
        let page = self.page.clone();
        let epoch = self.page_epoch;
        let events = self.events.clone();
        self.runtime.spawn(async move {
            let result = library::load(&session, &page, number).await;
            events.send(Event::Page(epoch, number, result));
        });
    }

    fn request_lyrics(&mut self) {
        self.track_epoch += 1;
        self.lyrics.clear();
        self.lyric_error = None;
        self.last_lyric = None;
        self.detail_follow.resume();
        self.sidebar_follow.resume();
        self.lyrics_loading = false;
        let (Some(session), Some(track)) = (self.session.clone(), self.queue.current()) else {
            return;
        };
        let id = track.id.clone();
        let events = self.events.clone();
        let epoch = self.track_epoch;
        self.lyrics_loading = true;
        self.runtime.spawn(async move {
            events.send(Event::Lyrics(epoch, library::lyric(&session, &id).await));
        });
    }

    fn load_sidebar_playlists(&self) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let events = self.events.clone();
        let epoch = self.session_epoch;
        self.runtime.spawn(async move {
            let mut all = Listing::default();
            let result = async {
                for number in 1..=40 {
                    let listing = library::load(&session, &Page::Playlists, number).await?;
                    all.append(listing);
                    if !all.has_more {
                        break;
                    }
                }
                Ok(all)
            }
            .await;
            events.send(Event::SidebarPlaylists(epoch, result));
        });
    }

    fn add_to_playlist(&mut self, playlist: Collection, tracks: &[Track]) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if tracks.is_empty() || !self.playlist_pending.insert(playlist.id.clone()) {
            return;
        }
        let ids: Vec<_> = tracks.iter().map(|track| track.id.clone()).collect();
        let events = self.events.clone();
        let epoch = self.session_epoch;
        self.runtime.spawn(async move {
            let result = library::add_playlist_tracks(&session, &playlist.id, &ids).await;
            events.send(Event::PlaylistUpdated(epoch, playlist, result));
        });
    }

    fn favorite(&mut self, track: &Track) {
        self.set_tracks_favorite(std::slice::from_ref(track), !self.is_favorite(track));
    }

    fn is_favorite(&self, track: &Track) -> bool {
        self.favorite_confirmed
            .get(&track.id)
            .copied()
            .unwrap_or(track.favorite)
    }

    fn set_tracks_favorite(&mut self, tracks: &[Track], on: bool) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let mut ids = Vec::new();
        for track in tracks {
            if self.is_favorite(track) != on && self.favorite_pending.insert(track.id.clone()) {
                ids.push(track.id.clone());
            }
        }
        if ids.is_empty() {
            return;
        }
        let events = self.events.clone();
        let epoch = self.session_epoch;
        self.runtime.spawn(async move {
            for id in ids {
                let result = library::favorite(&session, &id, on).await;
                events.send(Event::Favorite(epoch, id, on, result));
            }
        });
    }

    fn cover(&mut self, id: &str) -> Option<&TextureHandle> {
        if id.is_empty() {
            return None;
        }
        self.cover_clock += 1;
        let now = Instant::now();
        let request = self
            .covers
            .get(id)
            .is_none_or(|cover| matches!(cover, Cover::Missing(until) if now >= *until));
        if request
            && let Some(session) = self.session.clone()
            && let Ok(permit) = self.cover_limit.clone().try_acquire_owned()
        {
            if self.covers.len() >= COVER_CAPACITY {
                let old = self
                    .covers
                    .iter()
                    .filter(|(_, cover)| !matches!(cover, Cover::Pending))
                    .min_by_key(|(_, cover)| match cover {
                        Cover::Ready(_, used, _) => *used,
                        _ => 0,
                    })
                    .map(|(key, _)| key.clone());
                if let Some(old) = old {
                    self.covers.remove(&old);
                }
            }
            let id = id.to_owned();
            self.covers.insert(id.clone(), Cover::Pending);
            let events = self.events.clone();
            let epoch = self.session_epoch;
            self.runtime.spawn(async move {
                let cover = match session.cover(&id, "600").await {
                    Ok(data) => tokio::task::spawn_blocking(move || {
                        artwork::decode(&data).map(|image| {
                            let color = artwork::dominant_color(&image);
                            (image, color)
                        })
                    })
                    .await
                    .ok()
                    .flatten(),
                    Err(_) => None,
                };
                drop(permit);
                events.send(Event::Cover(epoch, id, cover));
            });
        }
        match self.covers.get_mut(id) {
            Some(Cover::Ready(texture, used, _)) => {
                *used = self.cover_clock;
                Some(texture)
            }
            _ => None,
        }
    }

    fn process_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.receive.try_recv() {
            match event {
                Event::Connected(result) => {
                    self.auth_busy = false;
                    match result {
                        Ok(connection) => {
                            let Connection {
                                credentials,
                                session,
                                queue_path,
                                queue,
                            } = *connection;
                            self.session_epoch += 1;
                            self.saved = Some(credentials);
                            self.session = Some(session);
                            self.queue_path = Some(queue_path);
                            self.queue = queue;
                            if self.queue.volume > 0.0 {
                                self.volume_before_mute = self.queue.volume;
                            }
                            self.saved_at = Instant::now();
                            self.open_page(Page::Home, false);
                            self.load_sidebar_playlists();
                            self.request_lyrics();
                        }
                        Err(error) => self.notice = Some(error.to_string()),
                    }
                }
                Event::LoggedOut(result) => {
                    self.auth_busy = false;
                    self.logging_out = false;
                    self.notice = Some(match result {
                        Ok(()) => {
                            self.saved = None;
                            "已退出登录".into()
                        }
                        Err(error) => error.to_string(),
                    });
                    if self.close_after_logout {
                        self.exiting = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                Event::Page(epoch, number, result) if epoch == self.page_epoch => {
                    self.loading = false;
                    match result {
                        Ok(mut listing) => {
                            for track in &mut listing.tracks {
                                if let Some(&confirmed) = self.favorite_confirmed.get(&track.id) {
                                    track.favorite = confirmed;
                                } else {
                                    self.queue.set_favorite(&track.id, track.favorite);
                                }
                            }
                            if number == 1 {
                                self.listing = listing;
                            } else {
                                self.listing.append(listing);
                            }
                            self.page_number = number;
                            self.track_table.sort_tracks(&mut self.listing.tracks);
                            if number == 1 {
                                self.track_table.retain(&self.listing.tracks);
                            }
                        }
                        Err(error) => self.page_error = Some(error.to_string()),
                    }
                }
                Event::Lyrics(epoch, result) if epoch == self.track_epoch => {
                    self.lyrics_loading = false;
                    match result {
                        Ok(text) => self.lyrics = playback::parse_lrc(&text),
                        Err(error) => self.lyric_error = Some(error.to_string()),
                    }
                }
                Event::Cover(epoch, id, cover) if epoch == self.session_epoch => {
                    let cover = if let Some((image, color)) = cover {
                        Cover::Ready(
                            ctx.load_texture(&id, image, egui::TextureOptions::LINEAR),
                            self.cover_clock,
                            color,
                        )
                    } else {
                        Cover::Missing(Instant::now() + Duration::from_secs(30))
                    };
                    self.covers.insert(id, cover);
                }
                Event::Favorite(epoch, id, on, result) if epoch == self.session_epoch => {
                    self.favorite_pending.remove(&id);
                    match result {
                        Ok(()) => {
                            self.favorite_confirmed.insert(id.clone(), on);
                            self.queue.set_favorite(&id, on);
                            self.dirty = true;
                            for entry in self.previous.iter_mut().chain(&mut self.forward) {
                                if entry.page == Page::Favorites {
                                    // Membership changes invalidate the server's pagination.
                                    entry.listing = Listing::default();
                                    entry.number = 0;
                                } else {
                                    for track in entry
                                        .listing
                                        .tracks
                                        .iter_mut()
                                        .filter(|track| track.id == id)
                                    {
                                        track.favorite = on;
                                    }
                                }
                            }
                            for track in self
                                .listing
                                .tracks
                                .iter_mut()
                                .filter(|track| track.id == id)
                            {
                                track.favorite = on;
                            }
                        }
                        Err(error) => self.notice = Some(error.to_string()),
                    }
                    if self.page == Page::Favorites && self.favorite_pending.is_empty() {
                        self.open_page(Page::Favorites, false);
                    }
                }
                Event::SidebarPlaylists(epoch, Ok(listing)) if epoch == self.session_epoch => {
                    self.sidebar_playlists = listing.playlists;
                }
                Event::PlaylistUpdated(epoch, playlist, result) if epoch == self.session_epoch => {
                    self.playlist_pending.remove(&playlist.id);
                    match result {
                        Ok(()) => {
                            let matches_playlist = |page: &Page| matches!(page, Page::Detail(item) if item.kind == Kind::Playlist && item.id == playlist.id);
                            for entry in self.previous.iter_mut().chain(&mut self.forward) {
                                if matches_playlist(&entry.page) {
                                    entry.listing = Listing::default();
                                    entry.number = 0;
                                }
                            }
                            if matches_playlist(&self.page) {
                                self.open_page(self.page.clone(), false);
                            }
                            self.notice = Some(format!("已添加到「{}」", playlist.title));
                        }
                        Err(error) => self.notice = Some(error.to_string()),
                    }
                }
                _ => {}
            }
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.expired())
        {
            self.disconnect(false);
        }
    }

    fn play_track(&mut self, index: usize) {
        self.queue.replace(self.listing.tracks.clone(), index);
        self.track_changed(true);
    }

    fn track_changed(&mut self, playing: bool) {
        self.stop_media();
        self.request_lyrics();
        self.dirty = true;
        if playing {
            self.open_media();
        }
    }

    fn stop_media(&mut self) {
        self.relay.revoke();
        self.media = None;
        self.media_open = false;
        self.playing = false;
        self.seek_preview = None;
    }

    fn open_media(&mut self) {
        let (Some(session), Some(track)) = (self.session.clone(), self.queue.current()) else {
            return;
        };
        let address = self.relay.issue(
            session,
            track.id.clone(),
            &track.format,
            self.playback_quality.parameter(),
        );
        match Media::open(&address, self.queue.volume) {
            Ok(media) => {
                self.media = Some(media);
                self.media_open = false;
                self.playing = true;
            }
            Err(_) => self.media_failed(),
        }
    }

    fn media_failed(&mut self) {
        self.stop_media();
        self.notice = Some("无法播放这首歌曲，请检查网络或 Windows 音频格式支持".into());
    }

    fn set_playback_quality(&mut self, quality: PlaybackQuality) {
        if self.playback_quality == quality {
            return;
        }
        self.playback_quality = quality;
        let was_playing = self.playing;
        if self.queue.current().is_some() {
            self.stop_media();
            if was_playing {
                self.open_media();
            }
        }
        self.dirty = true;
    }

    fn toggle_playing(&mut self) {
        if self.media.is_none() {
            self.open_media();
            return;
        }
        self.playing = !self.playing;
        if self.media_open
            && let Some(media) = &self.media
            && media.set_playing(self.playing).is_err()
        {
            self.media_failed();
        }
    }

    fn previous_track(&mut self) {
        if self.queue.previous() {
            self.track_changed(self.playing);
        } else {
            self.seek(0);
        }
    }

    fn next_track(&mut self, natural: bool) {
        if self.queue.next(natural) {
            self.track_changed(self.playing || natural);
        } else if natural {
            self.stop_media();
            self.queue.position_ms = 0;
            if self.queue.clear_after_playback {
                self.request_lyrics();
            }
            self.dirty = true;
        }
    }

    fn pause_for_sleep(&mut self) {
        self.playing = false;
        if let Some(media) = &self.media
            && self.media_open
            && media.set_playing(false).is_err()
        {
            self.stop_media();
        }
        self.dirty = true;
        self.notice = Some("睡眠定时已暂停播放".into());
    }

    fn locate_current(&mut self, ctx: &egui::Context) {
        self.lyrics_detail = false;
        self.panel = Panel::Queue;
        self.locate_queue = true;
        self.last_lyric = None;
        ctx.request_repaint();
    }

    fn sleep_status(&self) -> Option<String> {
        if let Some(remaining) = self.sleep_timer.remaining(Instant::now()) {
            Some(format!("{} 后暂停", clock(remaining.as_millis() as u64)))
        } else if matches!(self.sleep_timer, SleepTimer::EndOfTrack) {
            Some("本首结束后暂停".into())
        } else {
            None
        }
    }

    fn sleep_menu(&mut self, ui: &mut egui::Ui) {
        widgets::submenu(ui, Icon::Moon, "睡眠定时", |ui| {
            widgets::menu(ui, "暂停播放");
            let status = self
                .sleep_status()
                .unwrap_or_else(|| "选择暂停播放的时间".into());
            widgets::status(ui, Icon::Timer, "睡眠定时", &status);
            ui.add_enabled_ui(self.queue.current().is_some(), |ui| {
                let width = (ui.available_width() - 8.0) * 0.5;
                for row in [[15, 30], [60, 90]] {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        for minutes in row {
                            if widgets::duration_tile(
                                ui,
                                minutes,
                                self.sleep_timer.preset() == Some(minutes),
                                width,
                            )
                            .clicked()
                            {
                                self.sleep_timer = SleepTimer::after(minutes);
                                ui.ctx().request_repaint();
                                ui.close_menu();
                            }
                        }
                    });
                    ui.add_space(3.0);
                }
                if widgets::action(
                    ui,
                    Icon::Music,
                    "本首结束后",
                    Some("当前歌曲播放完成时暂停"),
                    widgets::Row::Selected(matches!(self.sleep_timer, SleepTimer::EndOfTrack)),
                )
                .clicked()
                {
                    self.sleep_timer = SleepTimer::EndOfTrack;
                    ui.close_menu();
                }
            });
            ui.separator();
            if widgets::action(
                ui,
                Icon::Power,
                "关闭定时",
                None,
                widgets::Row::Selected(matches!(self.sleep_timer, SleepTimer::Off)),
            )
            .clicked()
            {
                self.sleep_timer = SleepTimer::Off;
                ui.close_menu();
            }
        });
    }

    fn playback_options(&mut self, ui: &mut egui::Ui) {
        widgets::menu(ui, "播放操作");
        if ui
            .add_enabled_ui(self.queue.current().is_some(), |ui| {
                widgets::action(
                    ui,
                    Icon::Locate,
                    "定位当前歌曲",
                    Some("在播放列表中显示"),
                    widgets::Row::Action,
                )
            })
            .inner
            .clicked()
        {
            self.locate_current(ui.ctx());
            ui.close_menu();
        }
        let enabled = self.queue.clear_after_playback;
        if widgets::action(
            ui,
            Icon::Trash,
            "播完列表后清空",
            Some("列表自然播放结束后清空"),
            widgets::Row::Toggle(enabled),
        )
        .clicked()
        {
            self.queue.clear_after_playback = !enabled;
            if self.queue.clear_after_playback {
                self.queue.repeat = Repeat::Off;
            }
            self.dirty = true;
        }
        ui.separator();
        self.sleep_menu(ui);
    }

    fn seek(&mut self, position: u64) {
        self.queue.position_ms = position;
        self.dirty = true;
        if self.media_open
            && let Some(media) = &self.media
        {
            if media.seek(position).is_err() {
                self.notice = Some("这首歌曲暂时无法跳转".into());
            }
            self.position_hold = Instant::now() + Duration::from_millis(450);
        }
    }

    fn duration(&self) -> u64 {
        self.media
            .as_ref()
            .and_then(Media::duration)
            .or_else(|| self.queue.current().map(|track| track.duration_ms))
            .unwrap_or_default()
    }

    fn update_media(&mut self) {
        let sleep_expired = self.sleep_timer.poll(Instant::now());
        if sleep_expired {
            self.pause_for_sleep();
        }
        while let Some(event) = self.media.as_ref().and_then(Media::event) {
            match event {
                MediaEvent::Opened => {
                    self.media_open = true;
                    if self.queue.position_ms > 0 {
                        self.seek(self.queue.position_ms);
                    }
                    if let Some(media) = &self.media
                        && media.set_playing(self.playing).is_err()
                    {
                        self.media_failed();
                    }
                }
                MediaEvent::Ended => {
                    if self.sleep_timer.track_ended() {
                        self.stop_media();
                        self.queue.position_ms = 0;
                        self.dirty = true;
                    } else if !sleep_expired {
                        self.next_track(true);
                    }
                }
                MediaEvent::Failed => self.media_failed(),
            }
        }
        if self.media_open
            && self.seek_preview.is_none()
            && Instant::now() >= self.position_hold
            && let Some(position) = self.media.as_ref().and_then(Media::position)
        {
            self.dirty |= self.queue.position_ms != position;
            self.queue.position_ms = position;
        }
    }

    fn persist(&mut self) {
        if self.dirty
            && let Some(path) = &self.queue_path
        {
            self.saved_at = Instant::now();
            if vault::save_queue(path, &self.queue).is_ok() {
                self.dirty = false;
            } else {
                self.notice = Some(Error::Storage.to_string());
            }
        }
    }

    fn search_now(&mut self) {
        self.search_due = None;
        let query = self.search.trim().to_owned();
        if query.is_empty() {
            if matches!(self.page, Page::Search(_)) {
                if self.previous.is_empty() {
                    self.open_page(Page::Home, false);
                } else {
                    self.travel(false);
                    self.forward.clear();
                }
            }
        } else {
            let page = Page::Search(query);
            if self.page != page {
                self.forward.clear();
                self.open_page(page, !matches!(self.page, Page::Search(_)));
            }
        }
    }

    fn login_view(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.22).max(20.0));
                let (rect, _) = ui.allocate_exact_size(artwork::square(72.0), egui::Sense::hover());
                artwork::paint(ui, rect, None, "FnMusic");
                ui.add_space(16.0);
                ui.label(RichText::new("飞牛音乐").size(36.0).strong());
                ui.label(RichText::new("连接你的 NAS，播放自己的音乐资料库").weak());
                ui.add_space(25.0);
                ui.add_enabled_ui(!self.auth_busy, |ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.server_input)
                            .hint_text("NAS 地址，例如 192.168.1.10:5666")
                            .desired_width(340.0),
                    );
                    ui.add_space(12.0);
                    let login = ui.add(
                        egui::Button::new(RichText::new("使用 NAS 账号登录").color(Color32::WHITE))
                            .fill(ui.visuals().hyperlink_color)
                            .min_size(Vec2::new(340.0, 36.0)),
                    );
                    if login.clicked()
                        || (field.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                    {
                        self.connect(false);
                    }
                    if self.saved.is_some() {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            // Center the pair within the login form.
                            ui.add_space((ui.available_width() - 260.0).max(0.0) * 0.5);
                            if ui.button("重试已保存的会话").clicked() {
                                self.connect(true);
                            }
                            if ui.button("清除会话").clicked() {
                                self.disconnect(true);
                            }
                        });
                    }
                });
                if self.auth_busy {
                    ui.add_space(12.0);
                    ui.spinner();
                    ui.label("正在连接或等待 NAS 授权…");
                }
                if let Some(notice) = &self.notice {
                    ui.add_space(14.0);
                    ui.label(RichText::new(notice).color(ACCENT));
                }
            });
        });
    }

    fn sidebar_width(&self, ctx: &egui::Context) -> f32 {
        let compact = self.sidebar_collapsed || ctx.screen_rect().width() < SIDEBAR_BREAKPOINT;
        let width = if compact { 64.0 } else { 190.0 };
        ctx.animate_value_with_time(
            egui::Id::new("sidebar-width"),
            width,
            ctx.style().animation_time,
        )
    }

    fn sidebar_background(ctx: &egui::Context) -> Color32 {
        super::motion::themed_color(
            ctx,
            Color32::from_rgb(244, 243, 245),
            Color32::from_rgb(31, 31, 36),
        )
    }

    fn detail_background(&self, ctx: &egui::Context) -> Color32 {
        let color = self
            .queue
            .current()
            .and_then(|track| self.covers.get(&track.cover))
            .and_then(|cover| match cover {
                Cover::Ready(_, _, color) => Some(*color),
                _ => None,
            })
            .unwrap_or_else(|| self.accent());
        let background = super::motion::themed_color(
            ctx,
            Color32::from_rgb(250, 249, 248).lerp_to_gamma(color, 0.08),
            Color32::from_rgb(21, 22, 25).lerp_to_gamma(color, 0.24),
        );
        super::motion::smooth_color(ctx, egui::Id::new("cover-background"), background)
    }

    fn sidebar(&mut self, ctx: &egui::Context, compact: bool) {
        let width = self.sidebar_width(ctx);
        // Delay expanded content until it fits; do not let text force the
        // animated panel beyond its requested width.
        let compact = compact || width < 182.0;
        let fill = Self::sidebar_background(ctx);
        egui::SidePanel::left("sidebar")
            .exact_width(width)
            .resizable(false)
            .frame(Frame::new().fill(fill).inner_margin(12))
            .show(ctx, |ui| {
                ui.add_space(9.0);
                ui.add_space(3.0);
                if compact {
                    if icons::button(
                        ui,
                        Icon::Search,
                        "搜索（Ctrl+F）",
                        None,
                        false,
                        Vec2::new(ui.available_width(), 32.0),
                    )
                    .clicked()
                    {
                        self.search_focus = true;
                        ctx.request_repaint();
                    }
                } else {
                    self.search_field(ui, ui.available_width());
                    ui.add_space(10.0);
                    ui.label(RichText::new("资料库").small().weak().strong());
                }
                ui.add_space(4.0);
                let highlight = ui.painter().add(egui::Shape::Noop);
                for (page, icon) in [
                    (Page::Home, Icon::Home),
                    (Page::Tracks, Icon::Music),
                    (Page::Albums, Icon::Albums),
                    (Page::Artists, Icon::Artist),
                    (Page::Playlists, Icon::Queue),
                    (Page::Favorites, Icon::Heart),
                ] {
                    let label = page.title();
                    let selected = self.page == page;
                    let response = icons::navigation(
                        ui,
                        icon,
                        label,
                        (!compact).then_some(label),
                        selected,
                        Vec2::new(ui.available_width(), 32.0),
                    );
                    if selected {
                        let origin = ui.min_rect().top();
                        let y = ctx.animate_value_with_time(
                            egui::Id::new("sidebar-selection-y"),
                            response.rect.top() - origin,
                            ctx.style().animation_time,
                        );
                        let rect = response
                            .rect
                            .translate(egui::vec2(0.0, origin + y - response.rect.top()))
                            .shrink(1.0);
                        ui.painter().set(
                            highlight,
                            egui::Shape::rect_filled(rect, 7.0, ui.visuals().selection.bg_fill),
                        );
                    }
                    if page == Page::Favorites
                        && let Some(payload) = track_drop(ui, &response, self.session_epoch)
                    {
                        self.set_tracks_favorite(&payload.tracks, true);
                    }
                    if response.clicked() && !selected {
                        self.search.clear();
                        self.search_due = None;
                        self.open_page(page, true);
                    }
                }
                if !compact && !self.sidebar_playlists.is_empty() && ui.available_height() > 92.0 {
                    ui.add_space(10.0);
                    ui.label(RichText::new("我的歌单").small().weak().strong());
                    egui::ScrollArea::vertical()
                        .id_salt("sidebar-playlists")
                        .max_height(ui.available_height() - 72.0)
                        .show_rows(ui, 29.0, self.sidebar_playlists.len(), |ui, rows| {
                            for index in rows {
                                let playlist = self.sidebar_playlists[index].clone();
                                let selected =
                                    matches!(&self.page, Page::Detail(item) if item == &playlist);
                                let response = icons::button(
                                    ui,
                                    Icon::Queue,
                                    &playlist.title,
                                    Some(&playlist.title),
                                    selected,
                                    Vec2::new(ui.available_width(), 29.0),
                                );
                                if let Some(payload) = track_drop(ui, &response, self.session_epoch)
                                {
                                    self.add_to_playlist(playlist.clone(), &payload.tracks);
                                }
                                if response.clicked() && !selected {
                                    self.search.clear();
                                    self.search_due = None;
                                    self.open_page(Page::Detail(playlist), true);
                                }
                            }
                        });
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    self.account_menu(ui, compact);
                    ui.separator();
                });
            });
    }

    fn search_field(&mut self, ui: &mut egui::Ui, width: f32) {
        let id = egui::Id::new("library-search");
        let (rect, hit) = ui.allocate_exact_size(Vec2::new(width, 34.0), egui::Sense::click());
        let focused = ui.memory(|memory| memory.has_focus(id));
        let fill = super::motion::themed_color(
            ui.ctx(),
            Color32::from_rgb(235, 235, 240),
            Color32::from_rgb(35, 36, 41),
        );
        ui.painter().rect(
            rect,
            10.0,
            fill,
            egui::Stroke::new(
                if focused { 1.5_f32 } else { 1.0_f32 },
                if focused {
                    self.accent()
                } else {
                    Color32::TRANSPARENT
                },
            ),
            egui::StrokeKind::Inside,
        );
        icons::glyph(
            ui,
            egui::pos2(rect.left() + 16.0, rect.center().y),
            Icon::Search,
            if focused {
                self.accent()
            } else {
                ui.visuals().weak_text_color()
            },
        );
        let mut edit = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("library-search-field")
                .max_rect(egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 30.0, rect.top()),
                    rect.max,
                ))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let field = edit.add(
            egui::TextEdit::singleline(&mut self.search)
                .id(id)
                .frame(false)
                .desired_width((rect.width() - 54.0).max(18.0))
                .hint_text("搜索歌曲、专辑或艺人"),
        );
        if hit.clicked() {
            field.request_focus();
        }
        if std::mem::take(&mut self.search_focus) {
            field.request_focus();
        }
        if field.changed() {
            self.search_due = Some(Instant::now() + Duration::from_millis(300));
            ui.ctx().request_repaint_after(Duration::from_millis(300));
        }
        if (field.has_focus() || field.lost_focus())
            && ui.input(|input| input.key_pressed(egui::Key::Enter))
        {
            self.search_now();
        }
    }

    fn library_view(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(ctx.style().visuals.panel_fill)
                    .inner_margin(18),
            )
            .show(ctx, |ui| {
                ui.set_opacity(super::motion::reveal(
                    ctx,
                    egui::Id::new("library-reveal"),
                    egui::Id::new((self.page_epoch, self.lyrics_detail)),
                ));
                let mut open = None;
                let mut play = None;
                let mut load_more = false;
                let mut area = egui::ScrollArea::vertical().id_salt(("library", self.page_epoch));
                if self.page_number != 0
                    && let Some(offset) = self.restore_scroll.take()
                {
                    area = area.vertical_scroll_offset(offset);
                }
                let scroll = area.show(ui, |ui| {
                    let title = self.page.title().to_owned();
                    ui.label(RichText::new(title).size(32.0).strong());
                    let subtitle = match &self.page {
                        Page::Home => Some("来自 NAS 的音乐资料库".to_owned()),
                        Page::Detail(collection) => Some(collection.subtitle.clone()),
                        Page::Search(query) => Some(format!("“{query}”")),
                        _ => None,
                    };
                    if let Some(subtitle) = subtitle.filter(|text| !text.is_empty()) {
                        ui.label(RichText::new(subtitle).weak());
                    }
                    if let Some(message) = self.notice.clone() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(message).color(ACCENT));
                            if ui.small_button("关闭").clicked() {
                                self.notice = None;
                            }
                        });
                    }
                    ui.add_space(9.0);

                    if let Page::Detail(collection) = self.page.clone() {
                        ui.horizontal(|ui| {
                            self.art(ui, &collection.cover, &collection.title, 84.0);
                            if !self.listing.tracks.is_empty()
                                && self.primary_action(ui, "播放全部").clicked()
                            {
                                play = Some(0);
                            }
                        });
                    }
                    for (heading, cards) in [
                        (
                            if self.page == Page::Home {
                                "最近添加"
                            } else {
                                "专辑"
                            },
                            self.listing.albums.clone(),
                        ),
                        ("艺人", self.listing.artists.clone()),
                        ("歌单", self.listing.playlists.clone()),
                    ] {
                        if !cards.is_empty() {
                            ui.add_space(6.0);
                            ui.label(RichText::new(heading).size(19.0).strong());
                            ui.add_space(8.0);
                            if let Some(card) = self.cards(ui, &cards) {
                                open = Some(card);
                            }
                        }
                    }
                    if !self.listing.tracks.is_empty() {
                        ui.add_space(8.0);
                        ui.label(RichText::new("歌曲").size(19.0).strong());
                        ui.add_space(4.0);
                        play = self.track_list(ui).or(play);
                    }
                    if self.loading {
                        ui.add_space(12.0);
                        ui.label(RichText::new("正在读取资料库…").small().weak());
                        self.loading_skeleton(ui);
                    } else if let Some(error) = &self.page_error {
                        ui.add_space(12.0);
                        ui.label(RichText::new(error).color(ACCENT));
                        load_more = ui.button("重试").clicked();
                    } else if self.listing.has_more {
                        ui.add_space(12.0);
                        load_more = ui.button("加载更多").clicked();
                    } else if self.page_number == 1
                        && self.listing.tracks.is_empty()
                        && self.listing.albums.is_empty()
                        && self.listing.artists.is_empty()
                        && self.listing.playlists.is_empty()
                    {
                        ui.label(
                            RichText::new(if matches!(self.page, Page::Search(_)) {
                                "没有匹配的结果"
                            } else {
                                "这里还没有内容"
                            })
                            .weak(),
                        );
                    }
                });
                if (self.library_scroll >= 48.0) != (scroll.state.offset.y >= 48.0) {
                    ctx.request_repaint();
                }
                self.library_scroll = scroll.state.offset.y;
                if load_more {
                    self.load_page(self.page_number + 1);
                }
                if let Some(index) = play {
                    self.play_track(index);
                }
                if let Some(collection) = open {
                    self.open_page(Page::Detail(collection), true);
                }
            });
    }

    fn art(&mut self, ui: &mut egui::Ui, id: &str, seed: &str, size: f32) {
        let (rect, _) = ui.allocate_exact_size(artwork::square(size), egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let cache_id = ui.id().with(("previous-art", self.session_epoch));
            let texture = self.cover(id).cloned();
            let texture = if let Some(texture) = texture {
                ui.ctx()
                    .data_mut(|data| data.insert_temp(cache_id, texture.clone()));
                Some(texture)
            } else if !id.is_empty() && !matches!(self.covers.get(id), Some(Cover::Missing(_))) {
                ui.ctx()
                    .data(|data| data.get_temp::<TextureHandle>(cache_id))
            } else {
                None
            };
            let alpha = super::motion::reveal(
                ui.ctx(),
                ui.id().with("art-reveal"),
                egui::Id::new((id, seed)),
            );
            ui.scope(|ui| {
                ui.multiply_opacity(alpha);
                if texture.is_none()
                    && !id.is_empty()
                    && !matches!(self.covers.get(id), Some(Cover::Missing(_)))
                {
                    super::motion::skeleton(ui, rect, 9);
                } else {
                    artwork::paint(ui, rect, texture.as_ref(), seed);
                }
            });
        }
    }

    fn paint_cover(&mut self, ui: &egui::Ui, rect: egui::Rect, id: &str, seed: &str) {
        self.cover(id);
        self.paint_cached_cover(ui, rect, id, seed);
    }

    fn paint_cached_cover(&self, ui: &egui::Ui, rect: egui::Rect, id: &str, seed: &str) {
        let texture = match self.covers.get(id) {
            Some(Cover::Ready(texture, _, _)) => Some(texture),
            _ => None,
        };
        if texture.is_none()
            && !id.is_empty()
            && !matches!(self.covers.get(id), Some(Cover::Missing(_)))
        {
            super::motion::skeleton(ui, rect, 9);
        } else {
            artwork::paint(ui, rect, texture, seed);
        }
    }

    fn loading_skeleton(&self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        let cards = self.page_number == 0
            && matches!(
                self.page,
                Page::Home | Page::Albums | Page::Artists | Page::Playlists
            );
        if cards {
            let count = ((ui.available_width() + 12.0) / 164.0).floor().max(2.0) as usize;
            let width = ((ui.available_width() - (count - 1) as f32 * 12.0) / count as f32)
                .clamp(80.0, 195.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                for _ in 0..count {
                    ui.allocate_ui_with_layout(
                        Vec2::new(width, width + 40.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            let rect = ui
                                .allocate_exact_size(Vec2::splat(width), egui::Sense::hover())
                                .0;
                            super::motion::skeleton(ui, rect, 9);
                            for length in [0.75, 0.45] {
                                let rect = ui
                                    .allocate_exact_size(
                                        Vec2::new(width * length, 12.0),
                                        egui::Sense::hover(),
                                    )
                                    .0;
                                super::motion::skeleton(ui, rect, 4);
                            }
                        },
                    );
                }
            });
        } else {
            for _ in 0..if self.page_number == 0 { 6 } else { 2 } {
                ui.horizontal(|ui| {
                    let rect = ui
                        .allocate_exact_size(Vec2::splat(34.0), egui::Sense::hover())
                        .0;
                    super::motion::skeleton(ui, rect, 7);
                    ui.vertical(|ui| {
                        for width in [ui.available_width() * 0.45, ui.available_width() * 0.25] {
                            let rect = ui
                                .allocate_exact_size(Vec2::new(width, 10.0), egui::Sense::hover())
                                .0;
                            super::motion::skeleton(ui, rect, 4);
                        }
                    });
                });
                ui.add_space(8.0);
            }
        }
    }

    fn cards(&mut self, ui: &mut egui::Ui, cards: &[Collection]) -> Option<Collection> {
        let columns = ((ui.available_width() + 12.0) / 152.0).floor().max(2.0) as usize;
        let width = ((ui.available_width() - (columns - 1) as f32 * 12.0) / columns as f32)
            .clamp(100.0, 195.0);
        let mut selected = None;
        for row in cards.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                for card in row {
                    ui.allocate_ui_with_layout(
                        Vec2::new(width, width + 52.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(width);
                            let (rect, hit) = ui
                                .allocate_exact_size(artwork::square(width), egui::Sense::click());
                            if ui.is_rect_visible(rect) {
                                let hover = ui
                                    .ctx()
                                    .animate_bool(hit.id.with("card-hover"), hit.hovered());
                                let press = ui.ctx().animate_bool(
                                    hit.id.with("card-press"),
                                    hit.is_pointer_button_down_on(),
                                );
                                let drawn = rect
                                    .shrink(press * 2.0)
                                    .translate(egui::vec2(0.0, -3.0 * hover));
                                self.paint_cover(ui, drawn, &card.cover, &card.title);
                                if hover > 0.0 {
                                    ui.painter().rect_filled(
                                        drawn,
                                        CornerRadius::same(9),
                                        Color32::from_black_alpha((30.0 * hover) as u8),
                                    );
                                    let center = drawn.right_bottom() - egui::vec2(24.0, 24.0);
                                    ui.painter().circle_filled(
                                        center,
                                        17.0,
                                        Color32::from_black_alpha((145.0 * hover) as u8),
                                    );
                                    icons::glyph(
                                        ui,
                                        center,
                                        Icon::Play,
                                        Color32::from_white_alpha((255.0 * hover) as u8),
                                    );
                                }
                            }
                            let title = ui.add(
                                egui::Button::new(RichText::new(&card.title).strong())
                                    .frame(false)
                                    .truncate(),
                            );
                            if hit.clicked() || title.clicked() {
                                selected = Some(card.clone());
                            }
                            ui.add(
                                egui::Label::new(RichText::new(&card.subtitle).small().weak())
                                    .truncate(),
                            );
                        },
                    );
                }
            });
            ui.add_space(8.0);
        }
        selected
    }

    fn track_list(&mut self, ui: &mut egui::Ui) -> Option<usize> {
        const ROW_HEIGHT: f32 = 46.0;
        const ART_COLUMN: f32 = 47.0;
        const ACTIONS: f32 = 102.0;

        let width = ui.available_width();
        let artist = if width >= 600.0 { width * 0.20 } else { 0.0 };
        let album = if width >= 760.0 { width * 0.21 } else { 0.0 };
        let columns = [
            (
                Column::Title,
                (width - ART_COLUMN - ACTIONS - artist - album - 58.0).max(40.0),
            ),
            (Column::Artist, artist),
            (Column::Album, album),
            (Column::Duration, 58.0),
        ];
        let list_id = ui.make_persistent_id("track-list");
        let header = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(ART_COLUMN);
            for (column, width) in columns.into_iter().filter(|(_, width)| *width > 0.0) {
                let sort = self
                    .track_table
                    .sort
                    .filter(|(current, _)| *current == column);
                let title = match sort {
                    Some((_, true)) => format!("{} ↓", column.title()),
                    Some((_, false)) => format!("{} ↑", column.title()),
                    None => column.title().into(),
                };
                let button = egui::Button::new(RichText::new(title).weak())
                    .frame(false)
                    .truncate();
                if ui.add_sized([width, 26.0], button).clicked() {
                    self.track_table
                        .toggle_sort(column, &mut self.listing.tracks);
                }
            }
        });
        ui.interact(
            header.response.rect,
            list_id,
            egui::Sense::focusable_noninteractive(),
        )
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "歌曲列表"));
        ui.separator();

        let rows = self.listing.tracks.len();
        let row_top = ui.next_widget_position();
        let mut play = None;
        if ui.memory(|memory| memory.has_focus(list_id)) {
            let (step, extend, enter) = ui.input_mut(|input| {
                let modifiers = input.modifiers;
                let up = input.consume_key(modifiers, egui::Key::ArrowUp);
                let down = input.consume_key(modifiers, egui::Key::ArrowDown);
                let enter = input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                (i32::from(down) - i32::from(up), modifiers.shift, enter)
            });
            let cursor = self.track_table.cursor(&self.listing.tracks);
            if step != 0 {
                let next = cursor.map_or(if step < 0 { rows - 1 } else { 0 }, |index| {
                    index.saturating_add_signed(step as isize).min(rows - 1)
                });
                self.track_table
                    .select(&self.listing.tracks, next, extend, false);
                let row = egui::Rect::from_min_size(
                    row_top + egui::vec2(0.0, ROW_HEIGHT * next as f32),
                    Vec2::new(width, ROW_HEIGHT),
                );
                ui.scroll_to_rect(row, None);
            }
            if enter {
                play = cursor.or(Some(0));
            }
        }

        let clip = ui.clip_rect();
        let first = (((clip.top() - row_top.y) / ROW_HEIGHT).floor() as usize)
            .saturating_sub(1)
            .min(rows);
        let end = (((clip.bottom() - row_top.y) / ROW_HEIGHT).ceil() as usize)
            .saturating_add(1)
            .clamp(first, rows);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(first as f32 * ROW_HEIGHT);
            for index in first..end {
                let track = self.listing.tracks[index].clone();
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), egui::Sense::hover());
                let row_id = list_id.with(index);
                let row = ui.interact(rect, row_id, egui::Sense::click_and_drag());
                if row.clicked()
                    || (row.secondary_clicked() && !self.track_table.selected.contains(&track.id))
                {
                    let modifiers = ui.input(|input| input.modifiers);
                    self.track_table.select(
                        &self.listing.tracks,
                        index,
                        row.clicked() && modifiers.shift,
                        row.clicked() && modifiers.ctrl,
                    );
                    ui.memory_mut(|memory| memory.request_focus(list_id));
                }
                let selected = self.track_table.selected.contains(&track.id);
                if row.drag_started_by(egui::PointerButton::Primary) {
                    let tracks = if selected {
                        self.selected_tracks()
                    } else {
                        vec![track.clone()]
                    };
                    row.dnd_set_drag_payload(TrackDrag {
                        session: self.session_epoch,
                        tracks,
                    });
                }
                row.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        true,
                        selected,
                        &track.title,
                    )
                });
                if row.double_clicked() && ui.input(|input| input.modifiers.is_none()) {
                    play = Some(index);
                }
                if selected || row.hovered() {
                    let fill = if selected {
                        ui.visuals().selection.bg_fill
                    } else {
                        ui.visuals().widgets.hovered.weak_bg_fill
                    };
                    ui.painter().rect_filled(rect.shrink(1.0), 6.0, fill);
                }
                let cover = egui::Rect::from_center_size(
                    egui::pos2(rect.left() + 22.0, rect.center().y),
                    Vec2::splat(34.0),
                );
                if ui.is_rect_visible(cover) {
                    self.paint_cover(ui, cover, &track.cover, &track.title);
                    if self
                        .queue
                        .current()
                        .is_some_and(|current| current.id == track.id)
                    {
                        let badge = egui::Rect::from_center_size(
                            cover.right_bottom() - egui::vec2(6.0, 6.0),
                            Vec2::splat(16.0),
                        );
                        ui.painter().rect_filled(badge, 4, ui.visuals().panel_fill);
                        super::motion::playing_indicator(ui, badge.shrink(3.0), self.playing);
                    }
                }
                let color = if selected {
                    ui.visuals().selection.stroke.color
                } else {
                    ui.visuals().text_color()
                };
                let mut x = rect.left() + ART_COLUMN;
                for (column, width) in columns.into_iter().filter(|(_, width)| *width > 0.0) {
                    let duration;
                    let text = match column {
                        Column::Title => &track.title,
                        Column::Artist => &track.artist,
                        Column::Album => &track.album,
                        Column::Duration => {
                            duration = clock(track.duration_ms);
                            &duration
                        }
                    };
                    if column == Column::Title && artist == 0.0 {
                        paint_track_text(ui, x, rect.center().y - 8.0, width, text, color, 14.0);
                        paint_track_text(
                            ui,
                            x,
                            rect.center().y + 9.0,
                            width,
                            &track.artist,
                            color,
                            12.0,
                        );
                    } else {
                        paint_track_text(ui, x, rect.center().y, width, text, color, 14.0);
                    }
                    x += width;
                }

                let mut controls = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(row_id.with("actions"))
                        .max_rect(egui::Rect::from_min_max(
                            egui::pos2(x, rect.top() + 8.0),
                            rect.right_bottom(),
                        ))
                        .layout(Layout::left_to_right(Align::Min)),
                );
                controls.spacing_mut().item_spacing.x = 1.0;
                if icons::small(&mut controls, Icon::Play, "播放", false).clicked() {
                    play = Some(index);
                }
                self.favorite_button(&mut controls, &track);
                let more = icons::small(&mut controls, Icon::More, "歌曲操作", false);
                let popup = row_id.with("menu");
                if more.clicked() {
                    if !selected {
                        self.track_table
                            .select(&self.listing.tracks, index, false, false);
                    }
                    ui.memory_mut(|memory| memory.toggle_popup(popup));
                }
                let count = self.track_table.selected.len();
                let mut menu = None;
                row.context_menu(|ui| menu = track_actions(ui, count));
                let popup_action = egui::popup_below_widget(
                    ui,
                    popup,
                    &more,
                    egui::PopupCloseBehavior::CloseOnClickOutside,
                    |ui| track_actions(ui, count),
                )
                .flatten();
                if let Some(action) = menu.or(popup_action) {
                    ui.memory_mut(|memory| memory.close_popup());
                    self.apply_track_action(action);
                }
            }
            ui.add_space((rows - end) as f32 * ROW_HEIGHT);
        });
        play
    }

    fn selected_tracks(&self) -> Vec<Track> {
        let mut seen = HashSet::new();
        self.listing
            .tracks
            .iter()
            .filter(|track| {
                self.track_table.selected.contains(&track.id) && seen.insert(track.id.as_str())
            })
            .cloned()
            .collect()
    }

    fn apply_track_action(&mut self, action: TrackAction) {
        let mut tracks = self.selected_tracks();
        match action {
            TrackAction::Favorite(on) => self.set_tracks_favorite(&tracks, on),
            TrackAction::Queue => {
                for track in tracks {
                    self.add_to_queue(track, false);
                }
            }
            TrackAction::Next => {
                if self.queue.current().is_none() && !tracks.is_empty() {
                    self.add_to_queue(tracks.remove(0), false);
                }
                for track in tracks.into_iter().rev() {
                    self.add_to_queue(track, true);
                }
            }
        }
    }

    fn favorite_button(&mut self, ui: &mut egui::Ui, track: &Track) {
        let on = self
            .favorite_confirmed
            .get(&track.id)
            .copied()
            .unwrap_or(track.favorite);
        let label = if on {
            "取消喜欢"
        } else {
            "喜欢这首歌"
        };
        let button = ui
            .add_enabled_ui(!self.favorite_pending.contains(&track.id), |ui| {
                icons::small(ui, Icon::Heart, label, on)
            })
            .inner;
        if button.clicked() {
            self.favorite(track);
        }
    }

    fn add_to_queue(&mut self, track: Track, next: bool) {
        let empty = self.queue.current().is_none();
        self.queue.add(track, next);
        self.dirty = true;
        if empty {
            self.request_lyrics();
        }
    }

    fn player_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("player")
            .exact_height(104.0)
            .frame(
                Frame::new()
                    .fill(super::motion::themed_color(
                        ctx,
                        Color32::from_rgb(250, 250, 252),
                        Color32::from_rgb(20, 20, 23),
                    ))
                    .inner_margin(12),
            )
            .show(ctx, |ui| {
                let track = self.queue.current().cloned();
                let enabled = track.is_some();
                const TRANSPORT_WIDTH: f32 = 4.0 * 32.0 + 44.0 + 4.0 * 6.0;
                let (header, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 48.0),
                    egui::Sense::hover(),
                );
                let center = header.center().x;
                let side_bottom = ui.max_rect().bottom();
                let left = egui::Rect::from_min_max(
                    header.min,
                    egui::pos2(center - TRANSPORT_WIDTH * 0.5, side_bottom),
                );
                let transport = egui::Rect::from_min_max(
                    egui::pos2(left.right(), header.top()),
                    egui::pos2(center + TRANSPORT_WIDTH * 0.5, header.bottom()),
                );
                let right = egui::Rect::from_min_max(
                    egui::pos2(transport.right(), header.top()),
                    egui::pos2(header.right(), side_bottom),
                );
                let info = egui::Rect::from_min_max(
                    left.min,
                    egui::pos2(
                        (left.left() + 300.0).min(left.right() - if enabled { 36.0 } else { 0.0 }),
                        left.bottom(),
                    ),
                );
                let mut info_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("now-playing")
                        .max_rect(info)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                info_ui.set_clip_rect(info.intersect(ui.clip_rect()));
                self.player_song_info(&mut info_ui, track.as_ref());
                if let Some(track) = &track {
                    let favorite = egui::Rect::from_min_max(
                        egui::pos2(info.right() + 4.0, left.top()),
                        egui::pos2(info.right() + 36.0, left.bottom()),
                    );
                    let mut favorite_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt("player-favorite")
                            .max_rect(favorite)
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    self.favorite_button(&mut favorite_ui, track);
                }
                let mut controls = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("transport-controls")
                        .max_rect(transport)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                controls.spacing_mut().item_spacing.x = 6.0;
                controls.add_enabled_ui(enabled, |ui| self.transport_controls(ui));
                let mut accessories = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("player-accessories")
                        .max_rect(right)
                        .layout(Layout::right_to_left(Align::Center)),
                );
                accessories.set_clip_rect(right.intersect(ui.clip_rect()));
                accessories.spacing_mut().item_spacing.x = 4.0;
                self.player_accessories(&mut accessories);

                ui.add_space(4.0);
                if track.is_some() {
                    let available = ui.available_width();
                    let width = (available * 0.38).clamp(240.0, 560.0).min(available);
                    ui.horizontal(|ui| {
                        ui.add_space((available - width) * 0.5);
                        ui.allocate_ui_with_layout(
                            Vec2::new(width, 22.0),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_width(width);
                                self.progress_bar(ui, enabled);
                            },
                        );
                    });
                } else {
                    self.progress_bar(ui, enabled);
                }
            });
    }

    fn transport_controls(&mut self, ui: &mut egui::Ui) {
        if icons::small(ui, Icon::Shuffle, "随机播放", self.queue.shuffle).clicked() {
            self.queue.toggle_shuffle();
            self.dirty = true;
        }
        if icons::small(ui, Icon::Previous, "上一首", false).clicked() {
            self.previous_track();
        }
        if icons::transport(ui, self.playing).clicked() {
            self.toggle_playing();
        }
        if icons::small(ui, Icon::Next, "下一首", false).clicked() {
            self.next_track(false);
        }
        let (icon, label) = match self.queue.repeat {
            Repeat::Off => (Icon::Repeat, "循环已关闭"),
            Repeat::All => (Icon::Repeat, "全部循环"),
            Repeat::One => (Icon::RepeatOne, "单曲循环"),
        };
        if icons::small(ui, icon, label, self.queue.repeat != Repeat::Off).clicked() {
            self.queue.repeat = self.queue.repeat.cycle();
            if self.queue.repeat != Repeat::Off {
                self.queue.clear_after_playback = false;
            }
            self.dirty = true;
        }
    }

    fn player_accessories(&mut self, ui: &mut egui::Ui) {
        if icons::small(
            ui,
            Icon::Queue,
            "播放列表",
            !self.lyrics_detail && self.panel == Panel::Queue,
        )
        .clicked()
        {
            self.toggle_panel(Panel::Queue);
        }
        let volume_width = (ui.available_width() - 38.0).clamp(36.0, 92.0);
        let mut volume = self.queue.volume;
        let slider = playback_slider(ui, volume_width, false, |ui| {
            ui.add(
                egui::Slider::new(&mut volume, 0.0..=1.0)
                    .show_value(false)
                    .trailing_fill(true),
            )
        });
        slider.widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), f64::from(volume), "音量"));
        if slider
            .on_hover_text(format!("音量 {}%", (volume * 100.0).round()))
            .changed()
        {
            self.set_volume(volume);
        }
        let muted = self.queue.volume == 0.0;
        let (icon, label) = if muted {
            (Icon::Mute, "取消静音")
        } else {
            (Icon::Volume, "静音")
        };
        if icons::small(ui, icon, label, false).clicked() {
            if muted {
                self.set_volume(self.volume_before_mute);
            } else {
                self.volume_before_mute = self.queue.volume;
                self.set_volume(0.0);
            }
        }
        if let Some(track) = self.queue.current().cloned() {
            let label = match self.playback_quality {
                PlaybackQuality::Auto => track.quality_label().unwrap_or("自动"),
                selected => selected.label(),
            };
            let mut selected = None;
            egui::menu::menu_custom_button(
                ui,
                egui::Button::new(RichText::new(label).size(12.0).strong())
                    .corner_radius(7)
                    .min_size(Vec2::new(42.0, 26.0)),
                |ui| {
                    widgets::menu(ui, "播放质量");
                    for quality in [
                        PlaybackQuality::Auto,
                        PlaybackQuality::Standard,
                        PlaybackQuality::Lossless,
                        PlaybackQuality::HiRes,
                    ] {
                        let description = match quality {
                            PlaybackQuality::Auto => "跟随源文件",
                            PlaybackQuality::Standard => "标准音质",
                            PlaybackQuality::Lossless => "无损音质",
                            PlaybackQuality::HiRes => "高解析度音质",
                        };
                        if widgets::action(
                            ui,
                            Icon::Quality,
                            quality.label(),
                            Some(description),
                            widgets::Row::Selected(self.playback_quality == quality),
                        )
                        .clicked()
                        {
                            selected = Some(quality);
                            ui.close_menu();
                        }
                    }
                },
            )
            .response
            .on_hover_text("切换播放质量");
            if let Some(quality) = selected {
                self.set_playback_quality(quality);
            }
        }
    }

    fn set_volume(&mut self, volume: f32) {
        if volume > 0.0 {
            self.volume_before_mute = volume;
        }
        self.queue.volume = volume;
        if let Some(media) = &self.media {
            let _ = media.volume(volume);
        }
        self.dirty = true;
    }

    fn progress_bar(&mut self, ui: &mut egui::Ui, enabled: bool) {
        ui.horizontal(|ui| {
            let duration = self.duration();
            let mut position = self
                .seek_preview
                .unwrap_or(self.queue.position_ms)
                .min(duration);
            ui.add_sized(
                [51.0, 22.0],
                egui::Label::new(RichText::new(clock(position)).monospace().small().weak()),
            );
            let width = (ui.available_width() - 86.0).max(48.0);
            let slider = playback_slider(ui, width, true, |ui| {
                ui.add_enabled(
                    enabled && duration > 0,
                    egui::Slider::new(&mut position, 0..=duration.max(1))
                        .show_value(false)
                        .smart_aim(false)
                        .trailing_fill(true)
                        .handle_shape(egui::style::HandleShape::Circle),
                )
            });
            slider.widget_info(|| {
                egui::WidgetInfo::slider(enabled && duration > 0, position as f64, "播放进度")
            });
            if duration > 0
                && let Some(pointer) = slider.hover_pos()
            {
                let preview = scrub_position(slider.rect, pointer.x, duration);
                slider.clone().on_hover_ui_at_pointer(|ui| {
                    ui.label(RichText::new(clock(preview)).monospace());
                });
            }
            if slider.dragged() {
                self.seek_preview = Some(position);
            } else if slider.drag_stopped() || slider.changed() {
                self.seek_preview = None;
                self.seek(position);
            }
            let remaining = if duration > 0 {
                format!("-{}", clock(duration.saturating_sub(position)))
            } else {
                clock(0)
            };
            ui.add_sized(
                [55.0, 22.0],
                egui::Label::new(RichText::new(remaining).monospace().small().weak()),
            );
            if self.playing
                && (!self.media_open || self.media.as_ref().is_some_and(Media::buffering))
            {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
    }

    fn player_song_info(&mut self, ui: &mut egui::Ui, track: Option<&Track>) {
        let sense = if track.is_some() {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let layout = *ui.layout();
        let response = ui
            .scope_builder(
                egui::UiBuilder::new()
                    .id_salt("player_song_info")
                    .layout(layout)
                    .sense(sense),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let content = egui::Id::new(track.map(|track| track.id.as_str()));
                    ui.set_opacity(super::motion::reveal(
                        ui.ctx(),
                        egui::Id::new("song-info-reveal"),
                        content,
                    ));
                    self.art(
                        ui,
                        track.map_or("", |track| &track.cover),
                        track.map_or("FnMusic", |track| track.title.as_str()),
                        48.0,
                    );
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width().clamp(40.0, 260.0), 48.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.add_space(5.0);
                            let title = track.map_or("尚未播放", |track| &track.title);
                            ui.add(
                                egui::Label::new(RichText::new(title).strong())
                                    .selectable(false)
                                    .truncate(),
                            );
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 5.0;
                                if let Some(track) = track {
                                    ui.add(
                                        egui::Label::new(RichText::new(&track.artist).weak())
                                            .selectable(false)
                                            .truncate(),
                                    );
                                } else {
                                    ui.add(
                                        egui::Label::new(RichText::new("从资料库选择歌曲").weak())
                                            .selectable(false)
                                            .truncate(),
                                    );
                                }
                            });
                        },
                    );
                },
            )
            .response;
        if track.is_some() {
            response.context_menu(|ui| self.playback_options(ui));
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "打开歌词详情")
            });
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("打开歌词详情")
                .clicked()
                && !self.lyrics_detail
            {
                self.lyrics_detail = true;
                self.last_lyric = None;
            }
        }
    }

    fn lyrics_detail_view(&mut self, ctx: &egui::Context) {
        let Some(track) = self.queue.current().cloned() else {
            return;
        };
        let background = self.detail_background(ctx);
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(background)
                    .inner_margin(egui::Margin::symmetric(64, 30)),
            )
            .show(ctx, |ui| {
                ui.set_opacity(super::motion::reveal(
                    ctx,
                    egui::Id::new("detail-reveal"),
                    egui::Id::new((self.track_epoch, self.detail_lyrics_enabled)),
                ));
                ui.add_space(18.0);
                let has_lyrics = self.lyrics.iter().any(|line| !line.text.trim().is_empty());
                let show_lyrics = self.detail_lyrics_enabled && (has_lyrics || self.lyrics_loading);
                ui.columns(if show_lyrics { 2 } else { 1 }, |columns| {
                    let available = columns[0].available_rect_before_wrap();
                    let size = (available.width() - 42.0)
                        .min(available.height() - 286.0)
                        .clamp(112.0, 480.0);
                    let left = if show_lyrics {
                        available.left()
                    } else {
                        available.center().x - size * 0.5
                    };
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(
                            ctx.animate_value_with_time(
                                egui::Id::new("detail-player-x"),
                                left,
                                ctx.style().animation_time,
                            ),
                            available.top(),
                        ),
                        Vec2::new(size, available.height()),
                    );
                    columns[0].scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(rect)
                            .layout(Layout::top_down(Align::Min)),
                        |ui| {
                            // Keep the entire transport group on screen. In a
                            // short window the old cover cap consumed the space
                            // reserved for progress and volume controls.
                            ui.set_width(size);
                            self.art(ui, &track.cover, &track.title, size);
                            ui.add_space(18.0);
                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    Vec2::new((size - 86.0).max(80.0), 32.0),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&track.title).size(24.0).strong(),
                                            )
                                            .truncate(),
                                        );
                                    },
                                );
                                self.favorite_button(ui, &track);
                                widgets::icon_menu(ui, Icon::More, "播放操作", |ui| {
                                    self.playback_options(ui);
                                    ui.separator();
                                    if widgets::action(
                                        ui,
                                        Icon::Queue,
                                        "添加到播放列表末尾",
                                        None,
                                        widgets::Row::Action,
                                    )
                                    .clicked()
                                    {
                                        self.add_to_queue(track.clone(), false);
                                        ui.close_menu();
                                    }
                                });
                            });
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&track.artist)
                                        .size(17.0)
                                        .color(ui.visuals().weak_text_color()),
                                )
                                .truncate(),
                            );
                            ui.add_space(12.0);
                            self.detail_progress(ui, &track, size);
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = ((size - 172.0) / 4.0).max(3.0);
                                self.transport_controls(ui);
                            });
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                let muted = self.queue.volume == 0.0;
                                let (icon, label) = if muted {
                                    (Icon::Mute, "取消静音")
                                } else {
                                    (Icon::Volume, "静音")
                                };
                                if icons::small(ui, icon, label, false).clicked() {
                                    if muted {
                                        self.set_volume(self.volume_before_mute);
                                    } else {
                                        self.volume_before_mute = self.queue.volume;
                                        self.set_volume(0.0);
                                    }
                                }
                                let mut volume = self.queue.volume;
                                if playback_slider(
                                    ui,
                                    (size * 0.48).clamp(72.0, 200.0),
                                    false,
                                    |ui| {
                                        ui.add(
                                            egui::Slider::new(&mut volume, 0.0..=1.0)
                                                .show_value(false)
                                                .trailing_fill(true),
                                        )
                                    },
                                )
                                .changed()
                                {
                                    self.set_volume(volume);
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui
                                        .add_enabled(
                                            has_lyrics,
                                            egui::Button::new("歌词")
                                                .selected(self.detail_lyrics_enabled && has_lyrics)
                                                .corner_radius(12),
                                        )
                                        .on_hover_text(if has_lyrics {
                                            "显示或隐藏歌词"
                                        } else {
                                            "这首歌曲暂无歌词"
                                        })
                                        .clicked()
                                    {
                                        self.detail_lyrics_enabled = !self.detail_lyrics_enabled;
                                        self.last_lyric = None;
                                        ctx.request_repaint();
                                    }
                                });
                            });
                        },
                    );
                    if show_lyrics {
                        let lyrics = &mut columns[1];
                        lyrics.add_space(28.0);
                        self.lyrics_detail_lines(lyrics);
                    }
                });
            });
    }

    fn detail_progress(&mut self, ui: &mut egui::Ui, track: &Track, width: f32) {
        let duration = self.duration();
        let mut position = self
            .seek_preview
            .unwrap_or(self.queue.position_ms)
            .min(duration);
        let response = playback_slider(ui, width, true, |ui| {
            ui.add_enabled(
                duration > 0,
                egui::Slider::new(&mut position, 0..=duration.max(1))
                    .show_value(false)
                    .smart_aim(false)
                    .trailing_fill(true),
            )
        });
        if response.dragged() {
            self.seek_preview = Some(position);
        } else if response.drag_stopped() || response.changed() {
            self.seek_preview = None;
            self.seek(position);
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(clock(position))
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            let remaining = format!("-{}", clock(duration.saturating_sub(position)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(remaining)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(track.audio_description())
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    )
                    .truncate(),
                );
            });
        });
    }

    fn lyrics_detail_lines(&mut self, ui: &mut egui::Ui) {
        if self.lyrics_loading {
            lyric_skeleton(ui);
            return;
        }
        if self.lyrics.is_empty() {
            ui.label(RichText::new("暂无同步歌词").size(22.0).weak());
            return;
        }
        let active = playback::active_lyric(&self.lyrics, self.queue.position_ms);
        let mut seek = None;
        let resumed = lyric_follow_ui(ui, &mut self.detail_follow);
        let follow = self.detail_follow.following();
        egui::ScrollArea::vertical()
            .id_salt(("detail_lyric_lines", self.track_epoch))
            .max_height(ui.available_height().max(100.0))
            .show(ui, |ui| {
                ui.set_width((ui.available_width() - 12.0).max(0.0));
                for (index, line) in self.lyrics.iter().enumerate() {
                    let current = active == Some(index);
                    let emphasis = ui.ctx().animate_bool(
                        ui.id().with((self.track_epoch, index, "lyric-active")),
                        current,
                    );
                    let text = RichText::new(&line.text)
                        .size(22.0 + 8.0 * emphasis)
                        .color(
                            ui.visuals()
                                .weak_text_color()
                                .lerp_to_gamma(ui.visuals().text_color(), emphasis),
                        )
                        .strong();
                    let row = ui.add(egui::Label::new(text).wrap().sense(
                        if line.position_ms.is_some() {
                            egui::Sense::click()
                        } else {
                            egui::Sense::hover()
                        },
                    ));
                    if row.clicked() {
                        seek = line.position_ms;
                    }
                    if follow && current && (resumed || active != self.last_lyric) {
                        row.scroll_to_me(Some(Align::Center));
                    }
                    ui.add_space(if current { 22.0 } else { 16.0 });
                }
            });
        self.last_lyric = active;
        if let Some(position) = seek {
            self.detail_follow.resume();
            self.last_lyric = None;
            let duration = self.duration();
            self.seek(if duration == 0 {
                position
            } else {
                position.min(duration)
            });
        }
    }

    fn toggle_panel(&mut self, panel: Panel) {
        self.panel = if !self.lyrics_detail && self.panel == panel {
            Panel::None
        } else {
            panel
        };
        self.lyrics_detail = false;
        self.last_lyric = None;
    }

    fn side_content(&mut self, ctx: &egui::Context) {
        if self.panel == Panel::None {
            return;
        }
        if ctx.screen_rect().width() >= PANEL_BREAKPOINT {
            egui::SidePanel::right("playing_details")
                .default_width(320.0)
                .width_range(280.0..=400.0)
                .frame(
                    Frame::new()
                        .fill(ctx.style().visuals.window_fill)
                        .inner_margin(14),
                )
                .show(ctx, |ui| self.playing_details(ui));
        } else {
            egui::Window::new(if self.panel == Panel::Queue {
                "播放列表"
            } else {
                "歌词"
            })
            .id(egui::Id::new("playing_details_popup"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .default_height(460.0)
            .max_height((ctx.screen_rect().height() - 178.0).max(220.0))
            .frame(
                Frame::window(&ctx.style())
                    .corner_radius(16)
                    .inner_margin(14),
            )
            .anchor(egui::Align2::RIGHT_BOTTOM, [-18.0, -120.0])
            .show(ctx, |ui| self.playing_details(ui));
        }
    }

    fn playing_details(&mut self, ui: &mut egui::Ui) {
        let queue = self.panel == Panel::Queue;
        let title = if queue {
            "播放列表"
        } else {
            "同步歌词"
        };
        let caption = if queue {
            format!("共 {} 首歌曲", self.queue.entries().count())
        } else {
            self.queue
                .current()
                .map(|track| track.title.clone())
                .unwrap_or_else(|| "正在播放".into())
        };
        ui.horizontal(|ui| {
            let icon_rect = ui
                .allocate_exact_size(Vec2::splat(26.0), egui::Sense::hover())
                .0;
            icons::glyph(
                ui,
                icon_rect.center(),
                if queue { Icon::Queue } else { Icon::Music },
                ui.visuals().hyperlink_color,
            );
            ui.vertical(|ui| {
                ui.label(RichText::new(title).size(18.0).strong());
                ui.label(RichText::new(caption).size(12.0).weak());
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::small(ui, Icon::WindowClose, &format!("关闭{title}"), false).clicked() {
                    self.panel = Panel::None;
                }
            });
        });
        ui.add_space(12.0);
        if queue {
            self.queue_view(ui);
        } else {
            self.lyrics_view(ui);
        }
    }

    fn queue_view(&mut self, ui: &mut egui::Ui) {
        const ROW_HEIGHT: f32 = 58.0;
        ui.horizontal(|ui| {
            if ui
                .add_enabled_ui(self.queue.current().is_some(), |ui| {
                    widgets::pill(ui, Icon::Locate, "定位当前歌曲")
                })
                .inner
                .clicked()
            {
                self.locate_current(ui.ctx());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                widgets::icon_menu(ui, Icon::More, "播放列表选项", |ui| {
                    self.playback_options(ui)
                });
            });
        });
        ui.add_space(8.0);
        if let Some(status) = self.sleep_status() {
            widgets::status(ui, Icon::Moon, "睡眠定时", &status);
        }
        if self.queue.current().is_none() {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                let rect = ui
                    .allocate_exact_size(Vec2::splat(40.0), egui::Sense::hover())
                    .0;
                ui.painter().rect_filled(
                    rect,
                    12,
                    ui.visuals().hyperlink_color.gamma_multiply(0.08),
                );
                icons::glyph(ui, rect.center(), Icon::Queue, ui.visuals().hyperlink_color);
                ui.add_space(12.0);
                ui.label(RichText::new("播放列表为空").size(14.0).strong());
                ui.label(RichText::new("从资料库选择歌曲开始播放").size(12.0).weak());
            });
            return;
        }
        enum Action {
            Play(usize),
            Remove(usize),
            Move(usize, isize),
        }
        let mut action = None;
        let mut area = egui::ScrollArea::vertical().id_salt("queue_rows");
        if std::mem::take(&mut self.locate_queue) {
            let cursor = self
                .queue
                .entries()
                .position(|(_, _, current)| current)
                .unwrap_or(0);
            area = area.vertical_scroll_offset(
                (cursor as f32 * ROW_HEIGHT - ui.available_height() * 0.4).max(0.0),
            );
        }
        area.show(ui, |ui| {
            ui.set_width((ui.available_width() - 10.0).max(0.0));
            let origin = ui.next_widget_position();
            let first = (((ui.clip_rect().top() - origin.y) / ROW_HEIGHT)
                .floor()
                .max(0.0) as usize)
                .saturating_sub(1);
            let visible = (ui.clip_rect().height() / ROW_HEIGHT).ceil() as usize + 3;
            let covers: Vec<_> = self
                .queue
                .entries()
                .skip(first)
                .take(visible)
                .map(|(_, track, _)| track.cover.clone())
                .collect();
            for id in covers {
                self.cover(&id);
            }
            let mut occurrences = HashMap::<&str, usize>::new();
            for (index, track, current) in self.queue.entries() {
                let occurrence = occurrences.entry(track.id.as_str()).or_default();
                let id = ui.id().with((self.session_epoch, &track.id, *occurrence));
                *occurrence += 1;
                let (target, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), ROW_HEIGHT),
                    egui::Sense::hover(),
                );
                if !ui.is_rect_visible(target) {
                    continue;
                }
                let y = ui.ctx().animate_value_with_time(
                    id.with("row-y"),
                    target.top() - origin.y,
                    ui.style().animation_time,
                );
                let rect = target
                    .translate(egui::vec2(0.0, origin.y + y - target.top()))
                    .shrink2(egui::vec2(0.0, 2.0));
                let row = ui.interact(rect, id.with("select"), egui::Sense::click());
                row.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        true,
                        current,
                        &track.title,
                    )
                });
                if row.clicked() {
                    action = Some(Action::Play(index));
                }
                let mut row_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(id)
                        .max_rect(rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                row_ui.set_opacity(super::motion::reveal(ui.ctx(), id.with("row-reveal"), id));
                let hover = ui
                    .ctx()
                    .animate_bool(id.with("hover"), row.hovered() || row.has_focus());
                let fill = if current {
                    ui.visuals()
                        .hyperlink_color
                        .gamma_multiply(0.09 + hover * 0.04)
                } else {
                    ui.visuals()
                        .widgets
                        .hovered
                        .weak_bg_fill
                        .gamma_multiply(hover * 0.65)
                };
                row_ui.painter().rect_filled(rect, 10, fill);
                let cover = egui::Rect::from_center_size(
                    egui::pos2(rect.left() + 25.0, rect.center().y),
                    Vec2::splat(34.0),
                );
                self.paint_cached_cover(&row_ui, cover, &track.cover, &track.title);
                if current {
                    let badge = egui::Rect::from_center_size(
                        cover.right_bottom() - egui::vec2(5.0, 5.0),
                        Vec2::splat(15.0),
                    );
                    row_ui
                        .painter()
                        .rect_filled(badge, 4, ui.visuals().window_fill);
                    super::motion::playing_indicator(&row_ui, badge.shrink(3.0), self.playing);
                }
                let text_color = if current {
                    ui.visuals().hyperlink_color
                } else {
                    ui.visuals().text_color()
                };
                let text_width = (rect.width() - 90.0).max(0.0);
                paint_track_text(
                    &row_ui,
                    rect.left() + 48.0,
                    rect.center().y - 9.0,
                    text_width,
                    &track.title,
                    text_color,
                    13.0,
                );
                paint_track_text(
                    &row_ui,
                    rect.left() + 48.0,
                    rect.center().y + 10.0,
                    text_width,
                    &track.artist,
                    ui.visuals().weak_text_color(),
                    11.0,
                );
                if row.has_focus() {
                    row_ui.painter().rect_stroke(
                        rect.shrink(1.0),
                        10,
                        egui::Stroke::new(1.0_f32, ui.visuals().hyperlink_color),
                        egui::StrokeKind::Inside,
                    );
                }
                let mut more_ui = row_ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("more")
                        .max_rect(egui::Rect::from_center_size(
                            egui::pos2(rect.right() - 18.0, rect.center().y),
                            Vec2::new(30.0, 28.0),
                        ))
                        .layout(Layout::left_to_right(Align::Center)),
                );
                widgets::icon_menu(&mut more_ui, Icon::More, "歌曲操作", |ui| {
                    widgets::menu(ui, "播放列表");
                    for (icon, label, candidate, kind) in [
                        (
                            Icon::ArrowUp,
                            "上移",
                            Action::Move(index, -1),
                            widgets::Row::Action,
                        ),
                        (
                            Icon::ArrowDown,
                            "下移",
                            Action::Move(index, 1),
                            widgets::Row::Action,
                        ),
                        (
                            Icon::Trash,
                            "从播放列表移除",
                            Action::Remove(index),
                            widgets::Row::Destructive,
                        ),
                    ] {
                        if widgets::action(ui, icon, label, None, kind).clicked() {
                            action = Some(candidate);
                            ui.close_menu();
                        }
                    }
                });
            }
        });
        match action {
            Some(Action::Play(index)) => {
                self.queue.select(index);
                self.track_changed(true);
            }
            Some(Action::Remove(index)) => {
                if self.queue.remove(index) {
                    self.track_changed(self.playing);
                }
                self.dirty = true;
            }
            Some(Action::Move(index, delta)) => {
                self.queue.move_entry(index, delta);
                self.dirty = true;
            }
            None => {}
        }
    }

    fn lyrics_view(&mut self, ui: &mut egui::Ui) {
        if self.lyrics_loading {
            lyric_skeleton(ui);
        } else if let Some(error) = &self.lyric_error {
            ui.label(RichText::new(error).weak());
            if ui.button("重试").clicked() {
                self.request_lyrics();
            }
        } else if self.lyrics.is_empty() {
            ui.label(RichText::new("暂无歌词").weak());
        } else {
            let active = playback::active_lyric(&self.lyrics, self.queue.position_ms);
            let mut seek = None;
            let resumed = lyric_follow_ui(ui, &mut self.sidebar_follow);
            let follow = self.sidebar_follow.following();
            egui::ScrollArea::vertical()
                .id_salt(("lyric_lines", self.track_epoch))
                .show(ui, |ui| {
                    for (index, line) in self.lyrics.iter().enumerate() {
                        let text = RichText::new(&line.text).size(if active == Some(index) {
                            20.0
                        } else {
                            16.0
                        });
                        let text = if active == Some(index) {
                            text.color(ui.visuals().hyperlink_color).strong()
                        } else {
                            text
                        };
                        let label =
                            egui::Label::new(text)
                                .wrap()
                                .sense(if line.position_ms.is_some() {
                                    egui::Sense::click()
                                } else {
                                    egui::Sense::hover()
                                });
                        let row = ui.add(label);
                        if row.clicked() {
                            seek = line.position_ms;
                        }
                        if follow && active == Some(index) && (resumed || active != self.last_lyric)
                        {
                            row.scroll_to_me(Some(Align::Center));
                        }
                        ui.add_space(10.0);
                    }
                });
            self.last_lyric = active;
            if let Some(position) = seek {
                self.sidebar_follow.resume();
                self.last_lyric = None;
                let duration = self.duration();
                self.seek(if duration == 0 {
                    position
                } else {
                    position.min(duration)
                });
            }
        }
    }
}

impl eframe::App for MusicApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        super::motion::configure(ctx, self.preferences.animation_mode);
        self.refresh_system_appearance(ctx);
        super::motion::update_theme(ctx);
        if self.logging_out && ctx.input(|input| input.viewport().close_requested()) {
            self.close_after_logout = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.process_events(ctx);
        self.update_media();
        self.update_desktop(ctx);
        self.title_bar(ctx);
        self.appearance_window(ctx);
        if !self.exiting
            && !self.logging_out
            && self.close_to_tray
            && self.desktop.as_ref().is_some_and(Desktop::has_tray)
            && ctx.input(|input| input.viewport().close_requested())
        {
            self.persist();
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if self.session.is_none() {
            self.login_view(ctx);
            return;
        }
        if self
            .search_due
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.search_now();
        }
        if self.dirty && self.saved_at.elapsed() >= Duration::from_secs(5) {
            self.persist();
        }
        if !ctx.wants_keyboard_input()
            && self.queue.current().is_some()
            && ctx.input(|input| input.key_pressed(egui::Key::Space))
        {
            self.toggle_playing();
        }
        self.render_library_shell(ctx);
        if self.playing || self.media.is_some() || self.dirty {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }
}

impl MusicApp {
    fn title_bar(&mut self, ctx: &egui::Context) {
        let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
        let library = self.session.is_some() && !self.lyrics_detail;
        let compact = self.sidebar_collapsed || ctx.screen_rect().width() < SIDEBAR_BREAKPOINT;
        let sidebar_width = if library {
            self.sidebar_width(ctx)
        } else {
            0.0
        };
        let background = if self.lyrics_detail {
            self.detail_background(ctx)
        } else {
            ctx.style().visuals.panel_fill
        };
        egui::TopBottomPanel::top("custom-title-bar")
            .exact_height(40.0)
            .resizable(false)
            .frame(Frame::new().fill(background).inner_margin(0.0))
            .show(ctx, |ui| {
                if library {
                    let mut sidebar_rect = ui.max_rect();
                    sidebar_rect.set_width(sidebar_width);
                    ui.painter()
                        .rect_filled(sidebar_rect, 0.0, Self::sidebar_background(ctx));
                }
                ui.allocate_ui_with_layout(
                    ui.available_size(),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if library {
                            ui.allocate_ui_with_layout(
                                Vec2::new(sidebar_width, 40.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.set_width(sidebar_width);
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    if titlebar_button(
                                        ui,
                                        Icon::Sidebar,
                                        if compact {
                                            "显示侧边栏（Ctrl+B）"
                                        } else {
                                            "隐藏侧边栏（Ctrl+B）"
                                        },
                                        false,
                                        ctx.screen_rect().width() >= SIDEBAR_BREAKPOINT,
                                        64.0,
                                    ) {
                                        self.sidebar_collapsed = !self.sidebar_collapsed;
                                        ctx.request_repaint();
                                    }
                                    if sidebar_width >= 150.0 {
                                        ui.label(RichText::new("飞牛音乐").size(14.0).strong());
                                    }
                                },
                            );
                            ui.add_space(10.0);
                        } else {
                            ui.add_space(16.0);
                        }

                        if self.session.is_some() {
                            if titlebar_button(
                                ui,
                                Icon::Back,
                                if self.lyrics_detail {
                                    "退出播放详情（Esc）"
                                } else {
                                    "后退（Alt+←）"
                                },
                                false,
                                self.lyrics_detail || !self.previous.is_empty(),
                                34.0,
                            ) {
                                if self.lyrics_detail {
                                    self.lyrics_detail = false;
                                    self.last_lyric = None;
                                } else {
                                    self.travel(false);
                                }
                            }
                            if library {
                                if titlebar_button(
                                    ui,
                                    Icon::Forward,
                                    "前进（Alt+→）",
                                    false,
                                    !self.forward.is_empty(),
                                    34.0,
                                ) {
                                    self.travel(true);
                                }
                                if titlebar_button(
                                    ui,
                                    Icon::Refresh,
                                    "刷新",
                                    false,
                                    !self.loading,
                                    34.0,
                                ) {
                                    self.open_page(self.page.clone(), false);
                                }
                            }
                        }

                        let search_width = if library && compact { 193.0 } else { 0.0 };
                        let drag_width = (ui.available_width() - 140.0 - search_width).max(0.0);
                        let (rect, drag) = ui.allocate_exact_size(
                            Vec2::new(drag_width, 40.0),
                            egui::Sense::click_and_drag(),
                        );
                        let title = if self.lyrics_detail {
                            self.queue
                                .current()
                                .map(|track| format!("{} · {}", track.title, track.artist))
                                .unwrap_or_default()
                        } else if library {
                            if self.library_scroll >= 48.0 {
                                self.page.title().to_owned()
                            } else {
                                String::new()
                            }
                        } else {
                            "飞牛音乐".to_owned()
                        };
                        paint_track_text(
                            ui,
                            rect.left() + 4.0,
                            rect.center().y,
                            rect.width() - 8.0,
                            &title,
                            ui.visuals().weak_text_color(),
                            13.0,
                        );
                        if drag.drag_started() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                        }
                        if drag.double_clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        if search_width > 0.0 {
                            ui.allocate_ui_with_layout(
                                Vec2::new(185.0, 34.0),
                                Layout::left_to_right(Align::Center),
                                |ui| self.search_field(ui, 185.0),
                            );
                            ui.add_space(8.0);
                        }

                        if titlebar_button(ui, Icon::WindowMinimize, "最小化", false, true, 46.0)
                        {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        if titlebar_button(
                            ui,
                            if maximized {
                                Icon::WindowRestore
                            } else {
                                Icon::WindowMaximize
                            },
                            if maximized { "还原" } else { "最大化" },
                            false,
                            true,
                            46.0,
                        ) {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        if titlebar_button(ui, Icon::WindowClose, "关闭", true, true, 48.0) {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    },
                );
            });
    }

    fn render_library_shell(&mut self, ctx: &egui::Context) {
        let detail_changed = ctx.data_mut(|data| {
            let previous = data
                .get_temp_mut_or_insert_with(egui::Id::new("previous-detail-mode"), || {
                    self.lyrics_detail
                });
            let changed = *previous != self.lyrics_detail;
            *previous = self.lyrics_detail;
            changed
        });
        if detail_changed {
            ctx.data_mut(|data| {
                data.remove::<(egui::Id, f64)>(egui::Id::new("detail-reveal"));
                data.remove::<(egui::Id, f64)>(egui::Id::new("library-reveal"));
            });
        }
        if self.lyrics_detail
            && (self.queue.current().is_none()
                || ctx
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)))
        {
            self.lyrics_detail = false;
            self.last_lyric = None;
        }
        let was_detail = self.lyrics_detail;
        if !was_detail {
            let (back, forward, search, collapse) = ctx.input_mut(|input| {
                let back = input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft)
                    || input.pointer.button_clicked(egui::PointerButton::Extra1);
                let forward = input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight)
                    || input.pointer.button_clicked(egui::PointerButton::Extra2);
                let search = input.consume_key(egui::Modifiers::CTRL, egui::Key::F);
                let collapse = input.consume_key(egui::Modifiers::CTRL, egui::Key::B);
                (back, forward, search, collapse)
            });
            if back || forward {
                self.travel(!back);
            }
            self.search_focus |= search;
            if search {
                ctx.request_repaint();
            }
            if collapse && ctx.screen_rect().width() >= SIDEBAR_BREAKPOINT {
                self.sidebar_collapsed = !self.sidebar_collapsed;
            }
        }
        let compact = self.sidebar_collapsed || ctx.screen_rect().width() < SIDEBAR_BREAKPOINT;
        if !was_detail {
            self.sidebar(ctx, compact);
        }
        if self.session.is_none() {
            self.login_view(ctx);
            ctx.request_repaint();
            return;
        }
        if was_detail != self.lyrics_detail {
            ctx.request_repaint();
        }
        if self.lyrics_detail {
            self.lyrics_detail_view(ctx);
        } else {
            self.player_bar(ctx);
            self.side_content(ctx);
            self.library_view(ctx);
        }
    }
}

impl Drop for MusicApp {
    fn drop(&mut self) {
        self.persist();
        self.stop_media();
        self.browser.stop();
    }
}

fn lyric_follow_ui(ui: &mut egui::Ui, follow: &mut LyricFollow) -> bool {
    let mut resumed = false;
    // Keep the header height fixed so entering browse mode doesn't shift lines.
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), 30.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_height(30.0);
            if !follow.following() && widgets::pill(ui, Icon::Locate, "回到当前歌词").clicked()
            {
                follow.resume();
                resumed = true;
            }
        },
    );
    let region = ui.available_rect_before_wrap();
    let manual = ui.input(|input| {
        input.pointer.hover_pos().is_some_and(|pointer| {
            region.contains(pointer)
                && (input.raw_scroll_delta.y != 0.0
                    || (input.pointer.primary_down() && input.pointer.delta().y != 0.0))
        })
    });
    if manual {
        follow.pause();
        resumed = false;
        ui.ctx().request_repaint();
    }
    resumed
}

fn lyric_skeleton(ui: &mut egui::Ui) {
    for ratio in [0.7, 0.9, 0.6, 0.8, 0.65, 0.85] {
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width() * ratio, 20.0),
            egui::Sense::hover(),
        );
        super::motion::skeleton(ui, rect, 6);
        ui.add_space(20.0);
    }
}

fn titlebar_button(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    close: bool,
    enabled: bool,
    width: f32,
) -> bool {
    ui.add_enabled_ui(enabled, |ui| {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 40.0), egui::Sense::click());
        if ui.is_rect_visible(rect) {
            if response.hovered() || response.has_focus() || response.is_pointer_button_down_on() {
                let fill = if close {
                    Color32::from_rgb(196, 43, 52)
                } else if ui.visuals().dark_mode {
                    Color32::from_white_alpha(20)
                } else {
                    Color32::from_black_alpha(12)
                };
                ui.painter().rect_filled(rect.shrink(2.0), 5.0, fill);
            }
            let color = if !enabled {
                ui.visuals().weak_text_color()
            } else if close && (response.hovered() || response.is_pointer_button_down_on()) {
                Color32::WHITE
            } else {
                ui.visuals().text_color()
            };
            icons::glyph(ui, rect.center(), icon, color);
        }
        response.on_hover_text(label).clicked()
    })
    .inner
}

fn track_drop(ui: &egui::Ui, response: &egui::Response, session: u64) -> Option<Arc<TrackDrag>> {
    if response.dnd_hover_payload::<TrackDrag>().is_some() {
        ui.painter().rect_stroke(
            response.rect.shrink(1.0),
            7.0,
            egui::Stroke::new(1.5_f32, ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    response
        .dnd_release_payload::<TrackDrag>()
        .filter(|payload| payload.session == session)
}

fn track_actions(ui: &mut egui::Ui, count: usize) -> Option<TrackAction> {
    widgets::menu(
        ui,
        &if count > 1 {
            format!("已选 {count} 首歌曲")
        } else {
            "歌曲操作".to_owned()
        },
    );
    for (icon, label, action) in [
        (Icon::Next, "下一首播放", TrackAction::Next),
        (Icon::Queue, "添加到播放列表末尾", TrackAction::Queue),
        (Icon::Heart, "添加到喜欢的歌曲", TrackAction::Favorite(true)),
        (
            Icon::HeartOff,
            "从喜欢的歌曲移除",
            TrackAction::Favorite(false),
        ),
    ] {
        if widgets::action(ui, icon, label, None, widgets::Row::Action).clicked() {
            ui.close_menu();
            return Some(action);
        }
    }
    None
}

fn paint_track_text(
    ui: &egui::Ui,
    x: f32,
    y: f32,
    width: f32,
    text: &str,
    color: Color32,
    size: f32,
) {
    let color = if color == ui.visuals().weak_text_color() {
        widgets::secondary_text(ui)
    } else {
        color
    };
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.into(),
        egui::FontId::proportional(size.max(12.0)),
        color,
    );
    job.wrap.max_width = (width - 12.0).max(0.0);
    job.wrap.max_rows = 1;
    let galley = ui.painter().layout_job(job);
    let position =
        egui::pos2(x + 5.0, y - galley.size().y * 0.5).round_to_pixels(ui.ctx().pixels_per_point());
    ui.painter().galley(position, galley, color);
}

fn playback_slider(
    ui: &mut egui::Ui,
    width: f32,
    accent: bool,
    add: impl FnOnce(&mut egui::Ui) -> egui::Response,
) -> egui::Response {
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = width;
        ui.spacing_mut().slider_rail_height = 4.0;
        ui.spacing_mut().interact_size.y = 22.0;
        let visuals = ui.visuals_mut();
        visuals.widgets.inactive.corner_radius = CornerRadius::same(2);
        if !visuals.dark_mode {
            visuals.widgets.inactive.bg_fill = Color32::from_gray(215);
            visuals.widgets.inactive.fg_stroke.color = Color32::from_gray(100);
        }
        visuals.selection.bg_fill = if accent {
            visuals.hyperlink_color
        } else {
            visuals.weak_text_color()
        };
        add(ui)
    })
    .inner
}

fn scrub_position(rect: egui::Rect, pointer: f32, duration: u64) -> u64 {
    // egui's circular handle is inset by its radius at both ends of the slider.
    let inset = rect.height() / 2.5;
    let ratio = egui::remap_clamp(
        pointer,
        (rect.left() + inset)..=(rect.right() - inset),
        0.0..=1.0,
    );
    (f64::from(ratio) * duration as f64).round() as u64
}

fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lyric_browsing_pauses_only_for_scroll_inside_the_lyrics() {
        let ctx = egui::Context::default();
        let mut follow = LyricFollow::default();
        for (pointer, expected) in [
            (egui::pos2(650.0, 120.0), true),
            (egui::pos2(100.0, 120.0), false),
        ] {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(500.0, 500.0),
                    )),
                    events: vec![
                        egui::Event::PointerMoved(pointer),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, -80.0),
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        lyric_follow_ui(ui, &mut follow);
                    });
                },
            );
            assert_eq!(follow.following(), expected);
        }
        follow.resume();
        assert!(follow.following());
    }

    #[test]
    #[ignore = "interactive mock-only preview; requires 127.0.0.1:5667 and an empty FNMUSIC_DATA_DIR"]
    fn mock_experience_preview() {
        use winit::platform::windows::EventLoopBuilderExtWindows as _;
        let root = PathBuf::from(std::env::var_os("FNMUSIC_DATA_DIR").unwrap());
        assert!(
            !root.join("session.bin").exists(),
            "use an empty preview directory"
        );
        let _apartment = super::super::media::Apartment::new().unwrap();
        struct Preview {
            app: MusicApp,
            started: Instant,
            stage: u8,
        }
        impl eframe::App for Preview {
            fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
                self.app.update(ctx, frame);
                if self.stage == 0 && self.app.session.is_some() {
                    self.app.open_page(Page::Tracks, false);
                    self.stage = 1;
                } else if self.stage == 1 && !self.app.listing.tracks.is_empty() {
                    self.app.queue.volume = 0.0;
                    self.app.play_track(0);
                    self.app.lyrics_detail = true;
                    self.stage = 2;
                }
                if self.started.elapsed() >= Duration::from_secs(180) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        eframe::run_native(
            "FnMusic 动效预览",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_inner_size([1200.0, 800.0])
                    .with_decorations(false),
                event_loop_builder: Some(Box::new(|builder| {
                    builder.with_any_thread(true);
                })),
                ..Default::default()
            },
            Box::new(|cc| {
                super::super::install_chinese_fonts(&cc.egui_ctx);
                let mut app = MusicApp::new(cc.egui_ctx.clone())?;
                let credentials = app.runtime.block_on(async {
                    let base = api::normalize_base("127.0.0.1:5667")?;
                    let authorization = app.api.authorization(&base).await?;
                    let response = reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .unwrap()
                        .get(authorization.sign_in.clone())
                        .send()
                        .await
                        .unwrap();
                    let callback =
                        url::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
                    let code = authorization.callback_code(&callback).unwrap()?;
                    app.api.exchange(base, random_id(), random_id(), code).await
                })?;
                app.vault.save(&credentials)?;
                app.server_input = credentials.base.to_string();
                app.saved = Some(credentials);
                app.connect(true);
                Ok(Box::new(Preview {
                    app,
                    started: Instant::now(),
                    stage: 0,
                }))
            }),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop and an empty FNMUSIC_DATA_DIR"]
    fn native_window_hides_and_reactivates() {
        use raw_window_handle::HasWindowHandle;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IsWindowVisible};
        use winit::platform::windows::EventLoopBuilderExtWindows as _;
        let root = PathBuf::from(std::env::var_os("FNMUSIC_DATA_DIR").unwrap());
        assert!(
            !root.join("session.bin").exists(),
            "use an empty test directory"
        );
        let _apartment = super::super::media::Apartment::new().unwrap();
        struct Harness {
            app: MusicApp,
            hwnd: HWND,
            stage: Arc<AtomicUsize>,
            at: Instant,
        }
        impl eframe::App for Harness {
            fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
                self.app.update(ctx, frame);
                match self.stage.load(Ordering::SeqCst) {
                    0 if self.at.elapsed() > Duration::from_millis(300)
                        && unsafe { IsWindowVisible(self.hwnd) }.as_bool() =>
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        self.stage.store(1, Ordering::SeqCst);
                        self.at = Instant::now();
                    }
                    1 if self.at.elapsed() > Duration::from_millis(600) => {
                        assert!(
                            !unsafe { IsWindowVisible(self.hwnd) }.as_bool(),
                            "close should hide the window"
                        );
                        assert!(
                            Vault::open_or_activate().unwrap().is_none(),
                            "second launch should signal the existing instance"
                        );
                        self.stage.store(2, Ordering::SeqCst);
                        self.at = Instant::now();
                    }
                    2 if self.at.elapsed() > Duration::from_millis(900) => {
                        assert!(
                            unsafe { IsWindowVisible(self.hwnd) }.as_bool(),
                            "second launch should reveal the window"
                        );
                        self.app.set_close_preference(false);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        self.stage.store(3, Ordering::SeqCst);
                    }
                    _ => {}
                }
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        let stage = Arc::new(AtomicUsize::new(0));
        let result = stage.clone();
        eframe::run_native(
            "FnMusic 系统集成验证",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default().with_inner_size([800.0, 600.0]),
                event_loop_builder: Some(Box::new(|builder| {
                    builder.with_any_thread(true);
                })),
                ..Default::default()
            },
            Box::new(move |cc| {
                let mut app = MusicApp::new(cc.egui_ctx.clone())?;
                let raw_window_handle::RawWindowHandle::Win32(handle) =
                    cc.window_handle().unwrap().as_raw()
                else {
                    panic!("Windows required")
                };
                let hwnd = HWND(handle.hwnd.get() as *mut _);
                app.install_desktop(hwnd);
                assert!(app.desktop.as_ref().unwrap().has_tray());
                Ok(Box::new(Harness {
                    app,
                    hwnd,
                    stage,
                    at: Instant::now(),
                }))
            }),
        )
        .unwrap();
        assert_eq!(result.load(Ordering::SeqCst), 3);
    }

    #[test]
    #[ignore = "requires the automatic mock NAS on 127.0.0.1:5667 and an empty FNMUSIC_DATA_DIR"]
    fn mock_app_playback_restore_and_logout() {
        let root = PathBuf::from(std::env::var_os("FNMUSIC_DATA_DIR").unwrap());
        assert!(
            !root.join("session.bin").exists(),
            "use an empty test data directory"
        );
        use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED).unwrap();
        }
        let _apartment = super::super::media::Apartment;
        let ctx = egui::Context::default();
        let mut app = MusicApp::new(ctx.clone()).unwrap();
        let base = api::normalize_base("127.0.0.1:5667").unwrap();
        app.runtime.block_on(async {
            let profile = random_id();
            let request = app.api.authorization(&base).await.unwrap();
            let code = app
                .browser
                .authorize(request, profile.clone(), true)
                .await
                .unwrap();
            let credentials = app
                .api
                .exchange(base, random_id(), profile, code)
                .await
                .unwrap();
            app.vault.save(&credentials).unwrap();
        });

        app.server_input = "127.0.0.1:5667".into();
        app.connect(true);
        wait(&mut app, &ctx, "restore and load home", |app| {
            app.session.is_some() && !app.listing.albums.is_empty()
        });
        app.open_page(Page::Tracks, true);
        wait(&mut app, &ctx, "load tracks", |app| {
            !app.listing.tracks.is_empty()
        });
        verify_library_shell(&mut app, &ctx);
        verify_track_selection(&mut app, &ctx);
        app.search = app.listing.tracks[0].title.clone();
        app.search_now();
        wait(&mut app, &ctx, "search Chinese title", |app| {
            !app.listing.tracks.is_empty()
        });
        let track = app.listing.tracks[0].clone();
        app.favorite(&track);
        wait(&mut app, &ctx, "toggle favorite", |app| {
            app.listing.tracks[0].favorite != track.favorite
        });
        app.queue.volume = 0.0;
        app.play_track(0);
        wait(&mut app, &ctx, "play track and load lyrics", |app| {
            app.queue.position_ms > 200 && !app.lyrics.is_empty()
        });
        let lyric_position = app
            .lyrics
            .iter()
            .filter_map(|line| line.position_ms)
            .find(|&position| position > 1_000)
            .unwrap();
        app.seek(lyric_position);
        wait(&mut app, &ctx, "seek to a lyric", |app| {
            app.queue.position_ms >= lyric_position + 150
        });
        verify_player_controls(&mut app);
        verify_lyrics_detail_entry(&mut app);
        app.system_command(Command::Pause, &ctx);
        assert!(!app.playing);
        app.system_command(Command::Play, &ctx);
        assert!(app.playing);
        app.system_command(Command::Seek(lyric_position), &ctx);
        assert_eq!(app.queue.position_ms, lyric_position);
        app.update_desktop(&ctx);
        assert!(app.window_title.contains(&track.title));
        app.locate_current(&ctx);
        assert!(app.panel == Panel::Queue && !app.lyrics_detail && app.locate_queue);
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200.0, 800.0),
                )),
                ..Default::default()
            },
            |ctx| app.render_library_shell(ctx),
        );
        assert!(!app.locate_queue);
        app.sleep_timer = SleepTimer::Deadline(Instant::now(), 0);
        app.update_media();
        assert!(!app.playing);
        assert_eq!(app.queue.current().unwrap().id, track.id);
        assert!(app.queue.position_ms >= lyric_position.saturating_sub(100));
        assert!(matches!(app.sleep_timer, SleepTimer::Off));
        app.system_command(Command::Play, &ctx);
        app.queue.clear_after_playback = true;
        app.set_close_preference(false);
        assert!(!app.vault.preferences().close_to_tray);
        app.toggle_playing();
        app.persist();
        drop(app);

        let mut restarted = MusicApp::new(ctx.clone()).unwrap();
        wait(&mut restarted, &ctx, "restore after restart", |app| {
            app.session.is_some()
        });
        assert_eq!(restarted.queue.current().unwrap().id, track.id);
        assert!(restarted.queue.position_ms >= lyric_position.saturating_sub(100));
        assert!(!restarted.playing);
        assert!(!restarted.close_to_tray);
        assert!(restarted.queue.clear_after_playback);
        restarted.sleep_timer = SleepTimer::EndOfTrack;
        restarted.disconnect(true);
        assert!(matches!(restarted.sleep_timer, SleepTimer::Off));
        wait(&mut restarted, &ctx, "logout", |app| !app.auth_busy);
        assert_eq!(restarted.notice.as_deref(), Some("已退出登录"));
        assert!(!root.join("session.bin").exists());
    }

    fn verify_library_shell(app: &mut MusicApp, events_ctx: &egui::Context) {
        let draw = |app: &mut MusicApp, ctx: &egui::Context, width, events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(width, 640.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| app.render_library_shell(ctx),
            );
        };
        let panel = |ctx: &egui::Context, id: &str| {
            egui::panel::PanelState::load(ctx, egui::Id::new(id)).map(|state| state.rect)
        };
        app.panel = Panel::Lyrics;
        for (width, sidebar_width, docked) in [
            (979.0, 64.0, false),
            (980.0, 190.0, false),
            (1239.0, 190.0, false),
            (1240.0, 190.0, true),
        ] {
            let ctx = egui::Context::default();
            draw(app, &ctx, width, Vec::new());
            let sidebar = panel(&ctx, "sidebar").unwrap();
            let player = panel(&ctx, "player").unwrap();
            assert_eq!(sidebar.width(), sidebar_width);
            assert_eq!(sidebar.bottom(), 640.0);
            assert_eq!(player.left(), sidebar.right());
            assert_eq!(panel(&ctx, "playing_details").is_some(), docked);
        }
        app.panel = Panel::None;
        let ctx = egui::Context::default();
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        };
        draw(
            app,
            &ctx,
            1200.0,
            vec![key(egui::Key::B, egui::Modifiers::CTRL)],
        );
        assert_eq!(panel(&ctx, "sidebar").unwrap().width(), 64.0);
        app.sidebar_collapsed = false;

        app.load_page(2);
        wait(app, events_ctx, "load the second track page", |app| {
            app.page_number == 2
        });
        app.restore_scroll = Some(400.0);
        draw(app, &ctx, 1200.0, Vec::new());
        assert!((app.library_scroll - 400.0).abs() < 1.0);
        let track_count = app.listing.tracks.len();
        app.open_page(Page::Albums, true);
        wait(app, events_ctx, "open albums", |app| app.page_number == 1);

        draw(
            app,
            &ctx,
            1200.0,
            vec![key(egui::Key::ArrowLeft, egui::Modifiers::ALT)],
        );
        assert_eq!(app.page, Page::Tracks);
        assert_eq!(app.listing.tracks.len(), track_count);
        assert!((app.library_scroll - 400.0).abs() < 1.0);
        draw(
            app,
            &ctx,
            1200.0,
            vec![key(egui::Key::ArrowRight, egui::Modifiers::ALT)],
        );
        assert_eq!(app.page, Page::Albums);
        let pos = egui::pos2(350.0, 120.0);
        for pressed in [true, false] {
            draw(
                app,
                &ctx,
                1200.0,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Extra1,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert_eq!(app.page, Page::Tracks);
        assert!((app.library_scroll - 400.0).abs() < 1.0);
    }

    fn verify_track_selection(app: &mut MusicApp, events_ctx: &egui::Context) {
        let ctx = egui::Context::default();
        let mut list = egui::Id::NULL;
        let mut draw = |app: &mut MusicApp, modifiers, events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(720.0, 500.0),
                    )),
                    modifiers,
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        list = ui.make_persistent_id("track-list");
                        app.track_list(ui);
                    });
                },
            );
            list
        };
        let list_id = draw(app, egui::Modifiers::NONE, Vec::new());
        let center = |index: usize| {
            ctx.read_response(list_id.with(index))
                .unwrap()
                .rect
                .center()
        };
        for (index, modifiers) in [(0, egui::Modifiers::NONE), (2, egui::Modifiers::CTRL)] {
            let pos = center(index);
            for pressed in [true, false] {
                draw(
                    app,
                    modifiers,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers,
                        },
                    ],
                );
            }
        }
        assert_eq!(app.track_table.selected.len(), 2);
        let shift = egui::Modifiers::SHIFT;
        draw(
            app,
            shift,
            vec![egui::Event::Key {
                key: egui::Key::ArrowDown,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: shift,
            }],
        );
        assert_eq!(app.track_table.cursor(&app.listing.tracks), Some(3));
        let selected = app.selected_tracks();
        assert_eq!(selected.len(), 2);
        app.apply_track_action(TrackAction::Next);
        let queued: Vec<_> = app
            .queue
            .entries()
            .map(|(_, track, _)| track.id.as_str())
            .collect();
        assert_eq!(
            queued,
            selected
                .iter()
                .map(|track| track.id.as_str())
                .collect::<Vec<_>>()
        );

        wait(app, events_ctx, "load sidebar playlists", |app| {
            !app.sidebar_playlists.is_empty()
        });
        let playlist = app.sidebar_playlists[0].clone();
        app.add_to_playlist(playlist.clone(), &selected);
        wait(
            app,
            events_ctx,
            "add the selected tracks to the playlist",
            |app| app.playlist_pending.is_empty(),
        );
        let loaded = app
            .runtime
            .block_on(library::load(
                app.session.as_ref().unwrap(),
                &Page::Detail(playlist),
                1,
            ))
            .unwrap();
        assert!(
            selected
                .iter()
                .all(|track| loaded.tracks.iter().any(|added| added.id == track.id))
        );
    }

    #[test]
    fn scrub_preview_matches_the_slider_handle_range() {
        let rect = egui::Rect::from_min_size(egui::pos2(20.0, 10.0), Vec2::new(200.0, 22.0));
        assert_eq!(scrub_position(rect, 20.0, 120_000), 0);
        assert_eq!(scrub_position(rect, rect.center().x, 120_000), 60_000);
        assert_eq!(scrub_position(rect, 220.0, 120_000), 120_000);
    }

    fn verify_player_controls(app: &mut MusicApp) {
        let ctx = egui::Context::default();
        let draw = |app: &mut MusicApp, events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(760.0, 560.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| app.player_bar(ctx),
            );
        };
        draw(app, Vec::new());
        let bar = egui::panel::PanelState::load(&ctx, egui::Id::new("player"))
            .unwrap()
            .rect;
        let transport = ctx
            .read_response(egui::Id::new("transport-playback"))
            .unwrap()
            .rect;
        assert!(
            (transport.center().x - bar.center().x).abs() < 1.0,
            "transport should be centered: {transport:?}, player: {bar:?}"
        );
        let pos = transport.center();
        for expected in [false, true] {
            for pressed in [true, false] {
                draw(
                    app,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
            assert_eq!(
                app.playing, expected,
                "central transport click did not toggle playback"
            );
        }
        app.set_volume(0.37);
        app.set_volume(0.0);
        assert_eq!(app.volume_before_mute, 0.37);
        app.set_volume(0.0);
    }

    fn verify_lyrics_detail_entry(app: &mut MusicApp) {
        let ctx = egui::Context::default();
        let track = app.queue.current().cloned().unwrap();
        let mut bounds = egui::Rect::NOTHING;
        let mut draw = |app: &mut MusicApp, events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(760.0, 560.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        bounds = ui
                            .horizontal(|ui| app.player_song_info(ui, Some(&track)))
                            .response
                            .rect;
                    });
                },
            );
            bounds.left_top()
        };
        let origin = draw(app, Vec::new());
        for offset in [
            Vec2::new(24.0, 24.0),
            Vec2::new(65.0, 8.0),
            Vec2::new(65.0, 24.0),
        ] {
            app.lyrics_detail = false;
            let pos = origin + offset;
            for pressed in [true, false] {
                draw(
                    app,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
            assert!(
                app.lyrics_detail,
                "song info click at {offset:?} did not open lyrics"
            );
        }
    }

    fn wait(
        app: &mut MusicApp,
        ctx: &egui::Context,
        stage: &str,
        ready: impl Fn(&MusicApp) -> bool,
    ) {
        let start = Instant::now();
        loop {
            app.process_events(ctx);
            app.update_media();
            if ready(app) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(18),
                "mock app timed out at {stage}: {:?}, playback position {} ms",
                app.notice.as_ref().or(app.page_error.as_ref()),
                app.queue.position_ms
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}
