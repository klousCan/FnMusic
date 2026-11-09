use eframe::egui::emath::GuiRounding as _;
use eframe::egui::{
    self, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
#[repr(u8)]
pub enum Icon {
    Home,
    Music,
    Albums,
    Artist,
    Queue,
    Heart,
    Search,
    Sidebar,
    Logout,
    Power,
    Back,
    Forward,
    Refresh,
    Play,
    Pause,
    Previous,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    Mute,
    More,
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
    Locate,
    Timer,
    Moon,
    Check,
    Trash,
    ArrowUp,
    ArrowDown,
    HeartOff,
    Quality,
    Sparkles,
    Monitor,
    Tray,
    Sun,
    Palette,
}

pub fn small(ui: &mut Ui, icon: Icon, label: &str, selected: bool) -> Response {
    button(ui, icon, label, None, selected, vec2(32.0, 30.0))
}

pub fn glyph(ui: &Ui, center: Pos2, icon: Icon, color: Color32) {
    paint(ui, center, icon, color, false);
}

pub fn transport(ui: &mut Ui, playing: bool) -> Response {
    let label = if playing { "暂停" } else { "播放" };
    let (rect, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
    let response = ui.interact(rect, egui::Id::new("transport-playback"), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let fill = if !ui.is_enabled() {
            visuals.widgets.inactive.weak_bg_fill
        } else if response.is_pointer_button_down_on() {
            super::motion::themed_color(ui.ctx(), Color32::from_gray(71), Color32::from_gray(185))
        } else if response.hovered() {
            super::motion::themed_color(ui.ctx(), Color32::from_gray(45), Color32::WHITE)
        } else {
            super::motion::themed_color(ui.ctx(), Color32::from_gray(28), Color32::from_gray(231))
        };
        let glyph = if !ui.is_enabled() {
            visuals.weak_text_color()
        } else {
            super::motion::themed_color(ui.ctx(), Color32::WHITE, Color32::from_gray(26))
        };
        let center = rect.center();
        ui.painter().circle_filled(
            center,
            if response.is_pointer_button_down_on() {
                18.0
            } else {
                19.0
            },
            fill,
        );
        if response.has_focus() {
            ui.painter()
                .circle_stroke(center, 21.0, Stroke::new(1.6_f32, visuals.hyperlink_color));
        }
        let transition = ui.ctx().animate_bool(response.id.with("playing"), playing);
        paint(
            ui,
            if playing {
                center
            } else {
                center + vec2(1.0, 0.0)
            },
            Icon::Pause,
            glyph.gamma_multiply(transition),
            false,
        );
        paint(
            ui,
            center + vec2(1.0, 0.0),
            Icon::Play,
            glyph.gamma_multiply(1.0 - transition),
            false,
        );
    }
    response.on_hover_text(format!("{label}（空格）"))
}

pub fn button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    caption: Option<&str>,
    selected: bool,
    size: Vec2,
) -> Response {
    button_impl(ui, icon, label, caption, selected, size, true)
}

pub fn navigation(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    caption: Option<&str>,
    selected: bool,
    size: Vec2,
) -> Response {
    button_impl(ui, icon, label, caption, selected, size, false)
}

fn button_impl(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    caption: Option<&str>,
    selected: bool,
    size: Vec2,
    paint_selection: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        if selected {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                ui.is_enabled(),
                true,
                label,
            )
        } else {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
        }
    });
    if ui.is_rect_visible(rect) {
        let style = ui.style().interact_selectable(&response, selected);
        let hover = ui.ctx().animate_bool(
            response.id.with("hover"),
            response.hovered() || response.has_focus(),
        );
        let pressed = ui.ctx().animate_bool(
            response.id.with("press"),
            response.is_pointer_button_down_on(),
        );
        if (selected && paint_selection) || (!selected && hover > 0.0) {
            ui.painter().rect(
                rect.shrink(1.0 + pressed),
                7.0,
                style
                    .weak_bg_fill
                    .gamma_multiply(if selected { 1.0 } else { hover }),
                style.bg_stroke,
                StrokeKind::Inside,
            );
        }
        let color = if selected && (!paint_selection || caption.is_some()) && ui.is_enabled() {
            ui.visuals().selection.stroke.color
        } else if selected && ui.is_enabled() {
            ui.visuals().hyperlink_color
        } else {
            style.fg_stroke.color
        };
        let center = if caption.is_some() {
            pos2(rect.left() + 17.0, rect.center().y)
        } else {
            rect.center()
        };
        let selected_t = ui
            .ctx()
            .animate_bool(response.id.with("selected"), selected);
        if icon == Icon::Heart
            && ui.style().animation_time > 0.0
            && selected_t > 0.0
            && selected_t < 1.0
        {
            ui.painter().circle_stroke(
                center,
                10.0 + selected_t * 8.0,
                Stroke::new(1.2_f32, color.gamma_multiply(1.0 - selected_t)),
            );
        }
        paint(ui, center + vec2(0.0, pressed), icon, color, selected);
        if let Some(caption) = caption {
            let mut job = egui::text::LayoutJob::simple_singleline(
                caption.into(),
                egui::TextStyle::Button.resolve(ui.style()),
                color,
            );
            job.wrap.max_width = (rect.width() - 38.0).max(0.0);
            job.wrap.max_rows = 1;
            let galley = ui.painter().layout_job(job);
            ui.painter().galley(
                pos2(rect.left() + 34.0, center.y - galley.size().y * 0.5),
                galley,
                color,
            );
        }
    }
    response.on_hover_text(label)
}

pub fn window_icon() -> Option<egui::IconData> {
    let image = super::artwork::decode(include_bytes!("../../assets/icon.png"))?;
    Some(egui::IconData {
        rgba: image
            .pixels
            .into_iter()
            .flat_map(|pixel| pixel.to_srgba_unmultiplied())
            .collect(),
        width: image.size[0] as u32,
        height: image.size[1] as u32,
    })
}

fn paint(ui: &Ui, center: Pos2, icon: Icon, color: Color32, selected: bool) {
    if paint_better_icon(ui, center, icon, color) {
        return;
    }
    let painter = ui.painter();
    let p = |x, y| center + vec2(x, y);
    let stroke = Stroke::new(1.65_f32, color);
    let line = |points: &[(f32, f32)]| {
        painter.add(Shape::line(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    let box_outline = |left, top, right, bottom| {
        painter.rect_stroke(
            Rect::from_min_max(p(left, top), p(right, bottom)),
            2.0,
            stroke,
            StrokeKind::Middle,
        );
    };
    let triangle = |x: f32, direction: f32| {
        painter.add(Shape::convex_polygon(
            vec![
                p(x - direction * 4.0, -5.5),
                p(x + direction * 5.0, 0.0),
                p(x - direction * 4.0, 5.5),
            ],
            color,
            Stroke::NONE,
        ));
    };
    match icon {
        Icon::Power => {
            let points = (0..=28)
                .map(|step| {
                    let angle = (-55.0_f32 + step as f32 * 290.0 / 28.0).to_radians();
                    p(7.0 * angle.cos(), 7.0 * angle.sin())
                })
                .collect();
            painter.add(Shape::line(points, stroke));
            line(&[(0.0, -9.0), (0.0, 0.0)]);
        }
        Icon::Home => {
            line(&[(-8.0, -1.0), (0.0, -8.0), (8.0, -1.0)]);
            line(&[
                (-6.0, -2.0),
                (-6.0, 7.0),
                (-2.0, 7.0),
                (-2.0, 2.0),
                (2.0, 2.0),
                (2.0, 7.0),
                (6.0, 7.0),
                (6.0, -2.0),
            ]);
        }
        Icon::Music => {
            line(&[(-2.0, 4.0), (-2.0, -6.0), (6.0, -8.0), (6.0, 2.0)]);
            painter.circle_filled(p(-4.6, 4.6), 2.5, color);
            painter.circle_filled(p(3.4, 2.6), 2.5, color);
        }
        Icon::Albums => {
            box_outline(-7.0, -5.0, 7.0, 7.0);
            line(&[(-4.0, -8.0), (4.0, -8.0)]);
            painter.circle_stroke(p(0.0, 1.0), 2.7, stroke);
        }
        Icon::Artist => {
            painter.circle_stroke(p(0.0, -4.0), 3.1, stroke);
            line(&[
                (-7.0, 7.0),
                (-6.0, 3.5),
                (-3.0, 2.0),
                (3.0, 2.0),
                (6.0, 3.5),
                (7.0, 7.0),
            ]);
        }
        Icon::Queue => {
            for y in [-5.0, 0.0, 5.0] {
                line(&[(-3.0, y), (7.0, y)]);
                painter.circle_filled(p(-7.0, y), 0.9, color);
            }
        }
        Icon::Heart => {
            let points = (0..=32)
                .map(|i| {
                    let t = i as f32 * std::f32::consts::TAU / 32.0;
                    p(
                        t.sin().powi(3) * 7.2,
                        -(13.0 * t.cos()
                            - 5.0 * (2.0 * t).cos()
                            - 2.0 * (3.0 * t).cos()
                            - (4.0 * t).cos())
                            * 0.45,
                    )
                })
                .collect::<Vec<_>>();
            painter.add(Shape::line(
                points,
                Stroke::new(if selected { 2.4_f32 } else { 1.65 }, color),
            ));
        }
        Icon::Search => {
            painter.circle_stroke(p(-1.5, -1.5), 5.0, stroke);
            line(&[(2.0, 2.0), (7.0, 7.0)]);
        }
        Icon::Sidebar => {
            box_outline(-8.0, -6.5, 8.0, 6.5);
            line(&[(-3.0, -6.0), (-3.0, 6.0)]);
        }
        Icon::Logout => {
            line(&[(-1.0, -7.0), (-7.0, -7.0), (-7.0, 7.0), (-1.0, 7.0)]);
            line(&[(-3.0, 0.0), (8.0, 0.0), (4.0, -4.0)]);
            line(&[(8.0, 0.0), (4.0, 4.0)]);
        }
        Icon::Back | Icon::Forward => {
            let d = if matches!(icon, Icon::Back) {
                -1.0
            } else {
                1.0
            };
            line(&[(-d * 6.0, 0.0), (d * 6.0, 0.0), (d * 1.0, -5.0)]);
            line(&[(d * 6.0, 0.0), (d * 1.0, 5.0)]);
        }
        Icon::Refresh => {
            let points = (0..=16)
                .map(|i| {
                    let a = (i as f32 / 16.0) * 5.1 + 0.7;
                    p(a.cos() * 6.5, a.sin() * 6.5)
                })
                .collect();
            painter.add(Shape::line(points, stroke));
            line(&[(7.0, -8.0), (7.0, -2.0), (1.0, -2.0)]);
        }
        Icon::Play => triangle(0.0, 1.0),
        Icon::Pause => {
            for x in [-4.5, 1.5] {
                painter.rect_filled(Rect::from_min_max(p(x, -5.5), p(x + 3.0, 5.5)), 0.8, color);
            }
        }
        Icon::Previous | Icon::Next => {
            let d = if matches!(icon, Icon::Previous) {
                -1.0
            } else {
                1.0
            };
            triangle(-d * 2.0, d);
            line(&[(d * 6.0, -5.5), (d * 6.0, 5.5)]);
        }
        Icon::Shuffle => {
            line(&[
                (-8.0, -5.0),
                (-4.0, -5.0),
                (4.0, 5.0),
                (8.0, 5.0),
                (5.0, 2.0),
            ]);
            line(&[(8.0, 5.0), (5.0, 8.0)]);
            line(&[(-8.0, 5.0), (-4.0, 5.0), (-1.0, 1.0)]);
            line(&[(1.0, -1.0), (4.0, -5.0), (8.0, -5.0), (5.0, -8.0)]);
            line(&[(8.0, -5.0), (5.0, -2.0)]);
        }
        Icon::Repeat | Icon::RepeatOne => {
            line(&[(-7.0, 1.0), (-7.0, -5.0), (7.0, -5.0), (4.0, -8.0)]);
            line(&[(7.0, -5.0), (4.0, -2.0)]);
            line(&[(7.0, -1.0), (7.0, 5.0), (-7.0, 5.0), (-4.0, 2.0)]);
            line(&[(-7.0, 5.0), (-4.0, 8.0)]);
            if matches!(icon, Icon::RepeatOne) {
                line(&[(-1.0, -0.6), (0.5, -2.0), (0.5, 2.0)]);
            }
        }
        Icon::More => {
            for x in [-5.0, 0.0, 5.0] {
                painter.circle_filled(p(x, 0.0), 1.2, color);
            }
        }
        Icon::WindowMinimize => line(&[(-6.0, 3.0), (6.0, 3.0)]),
        Icon::WindowMaximize => box_outline(-6.0, -6.0, 6.0, 6.0),
        Icon::WindowRestore => {
            line(&[(-2.0, -6.0), (6.0, -6.0), (6.0, 2.0)]);
            line(&[
                (-6.0, -2.0),
                (-6.0, 6.0),
                (2.0, 6.0),
                (2.0, -2.0),
                (-6.0, -2.0),
            ]);
        }
        Icon::WindowClose => {
            line(&[(-5.0, -5.0), (5.0, 5.0)]);
            line(&[(5.0, -5.0), (-5.0, 5.0)]);
        }
        Icon::Check => line(&[(-6.0, 0.0), (-2.0, 4.0), (6.0, -5.0)]),
        Icon::ArrowUp | Icon::ArrowDown => {
            let d = if icon == Icon::ArrowUp { -1.0 } else { 1.0 };
            line(&[(0.0, -d * 7.0), (0.0, d * 7.0), (-5.0, d * 2.0)]);
            line(&[(0.0, d * 7.0), (5.0, d * 2.0)]);
        }
        Icon::Locate | Icon::Timer | Icon::Moon | Icon::Sun => {
            painter.circle_stroke(center, 7.0, stroke);
        }
        Icon::Trash
        | Icon::HeartOff
        | Icon::Quality
        | Icon::Sparkles
        | Icon::Monitor
        | Icon::Tray
        | Icon::Palette => {
            box_outline(-6.0, -6.0, 6.0, 6.0);
        }
        Icon::Volume | Icon::Mute => {
            line(&[
                (-8.0, -3.0),
                (-5.0, -3.0),
                (-1.0, -6.5),
                (-1.0, 6.5),
                (-5.0, 3.0),
                (-8.0, 3.0),
                (-8.0, -3.0),
            ]);
            if matches!(icon, Icon::Volume) {
                line(&[(2.0, -3.0), (3.0, -1.6), (3.5, 0.0), (3.0, 1.6), (2.0, 3.0)]);
                line(&[(4.5, -5.5), (6.1, -3.0), (6.7, 0.0), (6.1, 3.0), (4.5, 5.5)]);
            } else {
                line(&[(2.0, -2.5), (7.0, 2.5)]);
                line(&[(2.0, 2.5), (7.0, -2.5)]);
            }
        }
    }
}

// SVG paths are Tabler-style icons selected through Better Icons. Rasterizing a
// small themed texture keeps the native egui interactions while replacing the
// prior hand-drawn glyphs everywhere in the desktop UI.
fn paint_better_icon(ui: &Ui, center: Pos2, icon: Icon, color: Color32) -> bool {
    // Match the actual framebuffer size. Downsampling 320px cover thumbnails
    // into 18pt icons skips edge coverage and softens thin strokes.
    let pixels_per_point = ui.ctx().pixels_per_point();
    let side_pixels = (18.0 * pixels_per_point).round().clamp(1.0, 144.0) as u32;
    let key = egui::Id::new(("better-icons-native", icon as u8, side_pixels));
    let cached = ui
        .ctx()
        .data(|data| data.get_temp::<egui::TextureHandle>(key));
    let texture = cached.or_else(|| {
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#{:02x}{:02x}{:02x}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</svg>"##,
            255, 255, 255, tabler_body(icon)
        );
        let image = super::artwork::rasterize_svg(svg.as_bytes(), side_pixels)?;
        let texture = ui.ctx().load_texture(
            format!("better-icons-{}-{side_pixels}px", icon as u8),
            image, egui::TextureOptions::NEAREST,
        );
        ui.ctx().data_mut(|data| data.insert_temp(key, texture.clone()));
        Some(texture)
    });
    let Some(texture) = texture else {
        return false;
    };
    let size = Vec2::splat(side_pixels as f32 / pixels_per_point);
    let min = (center - size * 0.5).round_to_pixels(pixels_per_point);
    let rect = Rect::from_min_size(min, size);
    ui.painter().image(
        texture.id(),
        rect,
        Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
        color,
    );
    true
}

fn tabler_body(icon: Icon) -> &'static str {
    match icon {
        Icon::Home => {
            r#"<path d="M5 12l7 -7l7 7v8a2 2 0 0 1 -2 2h-10a2 2 0 0 1 -2 -2z"/><path d="M9 22v-6h6v6"/>"#
        }
        Icon::Music => {
            r#"<path d="M9 18v-13l12 -2v13"/><path d="M9 9l12 -2"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"#
        }
        Icon::Albums => {
            r#"<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="12" cy="12" r="3"/><path d="M12 3v6"/>"#
        }
        Icon::Artist => r#"<circle cx="12" cy="7" r="4"/><path d="M5.5 21a7 7 0 0 1 13 0"/>"#,
        Icon::Queue => r#"<path d="M4 6h12M4 12h12M4 18h8"/><path d="M19 16v6M16 19h6"/>"#,
        Icon::Heart => {
            r#"<path d="M12 20l-7 -7a4 4 0 0 1 5.5 -5.8l1.5 1.4l1.5 -1.4a4 4 0 0 1 5.5 5.8z"/>"#
        }
        Icon::Search => r#"<circle cx="11" cy="11" r="6"/><path d="M20 20l-4 -4"/>"#,
        Icon::Sidebar => r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/>"#,
        Icon::Logout => {
            r#"<path d="M14 8v-3a1 1 0 0 0 -1 -1h-8a1 1 0 0 0 -1 1v14a1 1 0 0 0 1 1h8a1 1 0 0 0 1 -1v-3"/><path d="M9 12h12l-3 -3M18 15l3 -3"/>"#
        }
        Icon::Power => r#"<path d="M7 6.5a8 8 0 1 0 10 0"/><path d="M12 4v8"/>"#,
        Icon::Back => r#"<path d="M15 18l-6 -6l6 -6"/>"#,
        Icon::Forward => r#"<path d="M9 18l6 -6l-6 -6"/>"#,
        Icon::Refresh => {
            r#"<path d="M20 11a8.1 8.1 0 0 0 -15.5 -2M4 5v4h4"/><path d="M4 13a8.1 8.1 0 0 0 15.5 2M20 19v-4h-4"/>"#
        }
        Icon::Play => r#"<path d="M7 4v16l13 -8z"/>"#,
        Icon::Pause => r#"<path d="M6 5h4v14h-4zM14 5h4v14h-4z"/>"#,
        Icon::Previous => r#"<path d="M20 5v14l-11 -7zM4 5v14"/>"#,
        Icon::Next => r#"<path d="M4 5v14l11 -7zM20 5v14"/>"#,
        Icon::Shuffle => r#"<path d="M17 3l4 4l-4 4M3 7h3l11 10h4M17 13l4 4l-4 4M3 17h3l3 -3"/>"#,
        Icon::Repeat => {
            r#"<path d="M4 12v-3a3 3 0 0 1 3 -3h10l-3 -3M20 12v3a3 3 0 0 1 -3 3h-10l3 3"/>"#
        }
        Icon::RepeatOne => {
            r#"<path d="M4 12v-3a3 3 0 0 1 3 -3h10l-3 -3M20 12v3a3 3 0 0 1 -3 3h-10l3 3"/><path d="M12 11l1 -1v5"/>"#
        }
        Icon::Volume => {
            r#"<path d="M15 8a5 5 0 0 1 0 8M17.7 5.3a9 9 0 0 1 0 13.4M4 10h4l5 -4v12l-5 -4h-4z"/>"#
        }
        Icon::Mute => r#"<path d="M4 10h4l5 -4v12l-5 -4h-4zM16 10l4 4M20 10l-4 4"/>"#,
        Icon::More => {
            r#"<circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/>"#
        }
        Icon::WindowMinimize => r#"<path d="M5 12h14"/>"#,
        Icon::WindowMaximize => r#"<rect x="5" y="5" width="14" height="14" rx="1"/>"#,
        Icon::WindowRestore => {
            r#"<path d="M8 8V5a1 1 0 0 1 1 -1h10a1 1 0 0 1 1 1v10a1 1 0 0 1 -1 1h-3M5 8h10a1 1 0 0 1 1 1v10a1 1 0 0 1 -1 1H5a1 1 0 0 1 -1 -1V9a1 1 0 0 1 1 -1z"/>"#
        }
        Icon::WindowClose => r#"<path d="M18 6L6 18M6 6l12 12"/>"#,
        Icon::Locate => {
            r#"<circle cx="12" cy="12" r="7"/><circle cx="12" cy="12" r="2"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3"/>"#
        }
        Icon::Timer => {
            r#"<circle cx="12" cy="13" r="8"/><path d="M12 9v4l2 2M9 2h6M12 2v3M19 5l1 -1"/>"#
        }
        Icon::Moon => r#"<path d="M20.8 13.1A9 9 0 0 1 10.9 3.2a9 9 0 1 0 9.9 9.9z"/>"#,
        Icon::Check => r#"<path d="M5 12l4 4l10 -10"/>"#,
        Icon::Trash => r#"<path d="M4 6h16M10 6V4h4v2M6 6l1 14h10l1 -14M10 10v6M14 10v6"/>"#,
        Icon::ArrowUp => r#"<path d="M12 20V4M6 10l6 -6l6 6"/>"#,
        Icon::ArrowDown => r#"<path d="M12 4v16M6 14l6 6l6 -6"/>"#,
        Icon::HeartOff => {
            r#"<path d="M3 3l18 18M6.5 6.5a4 4 0 0 0 -1.5 6.5l7 7l3.5 -3.5M9.7 6.5L12 8.6l1.5 -1.4a4 4 0 0 1 5.5 5.8l-1 1"/>"#
        }
        Icon::Quality => r#"<path d="M3 10v4M7 6v12M12 3v18M17 6v12M21 10v4"/>"#,
        Icon::Sparkles => {
            r#"<path d="M12 3l2.5 6.5L21 12l-6.5 2.5L12 21l-2.5 -6.5L3 12l6.5 -2.5zM4 3v4M2 5h4M20 17v4M18 19h4"/>"#
        }
        Icon::Monitor => {
            r#"<rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8M12 17v4"/>"#
        }
        Icon::Tray => r#"<path d="M3 13l3 -8h12l3 8v6H3zM3 13h5l2 3h4l2 -3h5"/>"#,
        Icon::Sun => {
            r#"<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M2 12h2M20 12h2M5 5l1.5 1.5M17.5 17.5L19 19M5 19l1.5 -1.5M17.5 6.5L19 5"/>"#
        }
        Icon::Palette => {
            r#"<path d="M12 3a9 9 0 0 0 0 18h1a2 2 0 0 0 0 -4h-.5a1.5 1.5 0 0 1 0 -3H15a6 6 0 0 0 0 -12z"/><circle cx="7.5" cy="10" r=".7"/><circle cx="10" cy="6.5" r=".7"/><circle cx="14.5" cy="6.5" r=".7"/><circle cx="17" cy="10" r=".7"/>"#
        }
    }
}
