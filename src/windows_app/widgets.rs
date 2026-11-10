use super::icons::{self, Icon};
use eframe::egui::emath::GuiRounding as _;
use eframe::egui::{
    self, Align2, Color32, FontId, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

#[derive(Clone, Copy)]
pub enum Row {
    Action,
    Selected(bool),
    Toggle(bool),
    Destructive,
}

pub fn menu(ui: &mut Ui, title: &str) {
    super::motion::popup(ui);
    ui.set_width(248.0);
    ui.spacing_mut().item_spacing.y = 3.0;
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), title));
    ui.painter().text(
        rect.left_center() + vec2(12.0, 0.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(12.0),
        ui.visuals().weak_text_color(),
    );
}

pub fn secondary_text(ui: &Ui) -> Color32 {
    super::motion::themed_color(ui.ctx(), Color32::from_gray(105), Color32::from_gray(160))
}

fn text(ui: &Ui, position: egui::Pos2, width: f32, value: &str, size: f32, color: Color32) {
    let color = if color == ui.visuals().weak_text_color() {
        secondary_text(ui)
    } else {
        color
    };
    let mut job = egui::text::LayoutJob::simple_singleline(
        value.into(),
        FontId::proportional(size.max(12.0)),
        color,
    );
    job.wrap.max_width = width.max(0.0);
    job.wrap.max_rows = 1;
    let galley = ui.painter().layout_job(job);
    let position =
        (position - vec2(0.0, galley.size().y * 0.5)).round_to_pixels(ui.ctx().pixels_per_point());
    ui.painter().galley(position, galley, color);
}

pub fn action(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    description: Option<&str>,
    kind: Row,
) -> Response {
    let selected = matches!(kind, Row::Selected(true) | Row::Toggle(true));
    let height = if description.is_some() { 52.0 } else { 36.0 };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| match kind {
        Row::Selected(on) => egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            on,
            label,
        ),
        Row::Toggle(on) => {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), on, label)
        }
        _ => egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label),
    });
    if ui.is_rect_visible(rect) {
        let hover = ui.ctx().animate_bool(
            response.id.with("row-hover"),
            response.hovered() || response.has_focus(),
        );
        let accent = ui.visuals().hyperlink_color;
        let fill = if selected {
            accent.gamma_multiply(0.09)
        } else {
            ui.visuals()
                .widgets
                .hovered
                .weak_bg_fill
                .gamma_multiply(hover)
        };
        ui.painter().rect_filled(rect.shrink(1.0), 9, fill);
        let color = if !ui.is_enabled() {
            ui.visuals().weak_text_color()
        } else if matches!(kind, Row::Destructive) {
            super::motion::themed_color(
                ui.ctx(),
                Color32::from_rgb(183, 41, 52),
                Color32::from_rgb(247, 113, 126),
            )
        } else {
            ui.visuals().text_color()
        };
        icons::glyph(
            ui,
            pos2(rect.left() + 20.0, rect.center().y),
            icon,
            if selected { accent } else { color },
        );
        let reserve = if matches!(kind, Row::Toggle(_)) {
            48.0
        } else {
            34.0
        };
        text(
            ui,
            pos2(
                rect.left() + 40.0,
                rect.center().y - if description.is_some() { 8.0 } else { 0.0 },
            ),
            rect.width() - 40.0 - reserve,
            label,
            13.0,
            color,
        );
        if let Some(description) = description {
            text(
                ui,
                pos2(rect.left() + 40.0, rect.center().y + 12.0),
                rect.width() - 50.0,
                description,
                10.5,
                ui.visuals().weak_text_color(),
            );
        }
        if let Row::Toggle(on) = kind {
            let t = ui.ctx().animate_bool(response.id.with("switch"), on);
            let track = egui::Rect::from_center_size(
                pos2(rect.right() - 24.0, rect.center().y),
                vec2(28.0, 17.0),
            );
            let fill = ui
                .visuals()
                .widgets
                .inactive
                .bg_fill
                .lerp_to_gamma(accent, t);
            ui.painter().rect_filled(track, 9, fill);
            ui.painter().circle_filled(
                pos2(track.left() + 8.5 + 11.0 * t, track.center().y),
                6.5,
                Color32::WHITE,
            );
        } else if selected {
            icons::glyph(
                ui,
                pos2(rect.right() - 18.0, rect.center().y),
                Icon::Check,
                accent,
            );
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.shrink(1.5),
                9,
                Stroke::new(1.0_f32, accent),
                StrokeKind::Inside,
            );
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn icon_menu<R>(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let result = egui::menu::menu_custom_button(
        ui,
        egui::Button::new("")
            .min_size(vec2(30.0, 28.0))
            .corner_radius(8)
            .frame(false),
        contents,
    );
    result.response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if result.response.hovered() || result.response.has_focus() {
        ui.painter().rect_filled(
            result.response.rect.shrink(1.0),
            8,
            ui.visuals().widgets.hovered.weak_bg_fill,
        );
    }
    icons::glyph(
        ui,
        result.response.rect.center(),
        icon,
        ui.visuals().text_color(),
    );
    result.response.clone().on_hover_text(label);
    result
}

pub fn submenu<R>(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = vec2(10.0, 8.0);
        ui.spacing_mut().interact_size.y = 38.0;
        ui.visuals_mut().widgets.hovered.corner_radius = egui::CornerRadius::same(9);
        ui.visuals_mut().widgets.open.corner_radius = egui::CornerRadius::same(9);
        let result = ui.menu_button(
            egui::RichText::new(format!("\u{2003}\u{2003} {label}")).size(13.0),
            contents,
        );
        icons::glyph(
            ui,
            pos2(
                result.response.rect.left() + 20.0,
                result.response.rect.center().y,
            ),
            icon,
            ui.visuals().text_color(),
        );
        result.response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
        });
        result
    })
    .inner
}

pub fn pill(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(
        label.into(),
        FontId::proportional(12.0),
        ui.visuals().text_color(),
    );
    let width = (galley.size().x + 42.0).min(ui.available_width());
    let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let hover = ui.ctx().animate_bool(
        response.id.with("pill-hover"),
        response.hovered() || response.has_focus(),
    );
    let accent = ui.visuals().hyperlink_color;
    ui.painter()
        .rect_filled(rect, 14, accent.gamma_multiply(0.07 + hover * 0.07));
    icons::glyph(ui, pos2(rect.left() + 16.0, rect.center().y), icon, accent);
    text(
        ui,
        pos2(rect.left() + 29.0, rect.center().y),
        rect.width() - 36.0,
        label,
        12.0,
        accent,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn status(ui: &mut Ui, icon: Icon, title: &str, detail: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Label,
            ui.is_enabled(),
            format!("{title}，{detail}"),
        )
    });
    let accent = ui.visuals().hyperlink_color;
    ui.painter()
        .rect_filled(rect, 11, accent.gamma_multiply(0.07));
    icons::glyph(ui, pos2(rect.left() + 22.0, rect.center().y), icon, accent);
    text(
        ui,
        pos2(rect.left() + 43.0, rect.center().y - 9.0),
        rect.width() - 53.0,
        title,
        13.0,
        ui.visuals().text_color(),
    );
    text(
        ui,
        pos2(rect.left() + 43.0, rect.center().y + 11.0),
        rect.width() - 53.0,
        detail,
        11.0,
        ui.visuals().weak_text_color(),
    );
    ui.add_space(5.0);
}

fn choice_frame(
    ui: &mut Ui,
    label: &str,
    selected: bool,
    size: egui::Vec2,
    accent: Color32,
) -> (egui::Rect, Response) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            label,
        )
    });
    let hover = ui.ctx().animate_bool(
        response.id.with("choice-hover"),
        response.hovered() || response.has_focus(),
    );
    let press = ui.ctx().animate_bool(
        response.id.with("choice-press"),
        response.is_pointer_button_down_on(),
    );
    let fill = if selected {
        accent.gamma_multiply(0.08 + hover * 0.04)
    } else {
        ui.visuals()
            .widgets
            .inactive
            .weak_bg_fill
            .lerp_to_gamma(ui.visuals().widgets.hovered.weak_bg_fill, hover * 0.6)
    };
    ui.painter().rect(
        rect.shrink(press * 0.7),
        12,
        fill,
        Stroke::new(
            if selected || response.has_focus() {
                1.4_f32
            } else {
                0.6
            },
            if selected || response.has_focus() {
                accent
            } else {
                ui.visuals()
                    .widgets
                    .noninteractive
                    .bg_stroke
                    .color
                    .gamma_multiply(0.5)
            },
        ),
        StrokeKind::Inside,
    );
    (
        rect,
        response.on_hover_cursor(egui::CursorIcon::PointingHand),
    )
}

pub fn mode_card(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    dark: Option<bool>,
    selected: bool,
    width: f32,
) -> Response {
    let accent = ui.visuals().hyperlink_color;
    let (rect, response) = choice_frame(ui, label, selected, vec2(width, 94.0), accent);
    let preview = egui::Rect::from_min_size(rect.min + vec2(12.0, 10.0), vec2(width - 24.0, 44.0));
    let miniature = |rect: egui::Rect, dark: bool| {
        let background = if dark {
            Color32::from_rgb(30, 31, 35)
        } else {
            Color32::from_rgb(252, 252, 254)
        };
        let foreground = if dark {
            Color32::from_gray(115)
        } else {
            Color32::from_gray(188)
        };
        ui.painter().rect(
            rect,
            5,
            background,
            Stroke::new(0.6_f32, foreground.gamma_multiply(0.5)),
            StrokeKind::Inside,
        );
        let sidebar = egui::Rect::from_min_max(
            rect.min + vec2(4.0, 7.0),
            pos2(rect.left() + rect.width() * 0.25, rect.bottom() - 5.0),
        );
        ui.painter()
            .rect_filled(sidebar, 2, foreground.gamma_multiply(0.18));
        let left = sidebar.right() + 5.0;
        for (index, length) in [0.8, 0.6, 0.7].into_iter().enumerate() {
            let bar = egui::Rect::from_min_size(
                pos2(left, rect.top() + 10.0 + index as f32 * 9.0),
                vec2((rect.right() - left - 7.0) * length, 3.0),
            );
            ui.painter().rect_filled(
                bar,
                2,
                if index == 0 {
                    accent.gamma_multiply(0.85)
                } else {
                    foreground.gamma_multiply(0.5)
                },
            );
        }
    };
    match dark {
        Some(dark) => miniature(preview, dark),
        None => {
            let half = (preview.width() - 3.0) * 0.5;
            miniature(
                egui::Rect::from_min_size(preview.min, vec2(half, preview.height())),
                false,
            );
            miniature(
                egui::Rect::from_min_size(
                    pos2(preview.left() + half + 3.0, preview.top()),
                    vec2(half, preview.height()),
                ),
                true,
            );
        }
    }
    let center_y = rect.bottom() - 20.0;
    icons::glyph(
        ui,
        pos2(rect.left() + 20.0, center_y),
        icon,
        if selected {
            accent
        } else {
            ui.visuals().weak_text_color()
        },
    );
    text(
        ui,
        pos2(rect.left() + 36.0, center_y),
        rect.width() - 60.0,
        label,
        13.0,
        ui.visuals().text_color(),
    );
    if selected {
        icons::glyph(ui, pos2(rect.right() - 16.0, center_y), Icon::Check, accent);
    }
    response
}

pub fn connection_choice(ui: &mut Ui, label: &str, selected: bool, width: f32) -> Response {
    let accent = ui.visuals().hyperlink_color;
    let (rect, response) = choice_frame(ui, label, selected, vec2(width, 36.0), accent);
    ui.painter().text(
        rect.center().round_to_pixels(ui.ctx().pixels_per_point()),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        if selected {
            accent
        } else {
            ui.visuals().text_color()
        },
    );
    response
}

pub fn accent_card(
    ui: &mut Ui,
    label: &str,
    color: Color32,
    selected: bool,
    width: f32,
) -> Response {
    let border = if ui.visuals().dark_mode {
        color
    } else {
        color.lerp_to_gamma(Color32::BLACK, 0.14)
    };
    let (rect, response) = choice_frame(ui, label, selected, vec2(width, 64.0), border);
    let center = pos2(rect.left() + 28.0, rect.center().y);
    if selected {
        ui.painter().circle_stroke(
            center,
            18.0,
            Stroke::new(1.0_f32, color.gamma_multiply(0.38)),
        );
    }
    ui.painter().circle_filled(center, 14.0, color);
    if selected {
        icons::glyph(ui, center, Icon::Check, Color32::WHITE);
    }
    text(
        ui,
        pos2(rect.left() + 51.0, rect.center().y),
        rect.width() - 60.0,
        label,
        13.0,
        ui.visuals().text_color(),
    );
    response
}

pub fn duration_tile(ui: &mut Ui, minutes: u64, selected: bool, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 52.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            format!("{minutes} 分钟后"),
        )
    });
    let accent = ui.visuals().hyperlink_color;
    let hover = ui.ctx().animate_bool(
        response.id.with("tile-hover"),
        response.hovered() || response.has_focus(),
    );
    let fill = if selected {
        accent.gamma_multiply(0.12)
    } else {
        ui.visuals()
            .widgets
            .inactive
            .weak_bg_fill
            .lerp_to_gamma(ui.visuals().widgets.hovered.weak_bg_fill, hover)
    };
    ui.painter().rect(
        rect,
        10,
        fill,
        Stroke::new(
            if selected { 1.2_f32 } else { 0.5 },
            if selected {
                accent
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke.color
            },
        ),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.left_center() + vec2(14.0, 0.0),
        Align2::LEFT_CENTER,
        minutes,
        FontId::proportional(21.0),
        if selected {
            accent
        } else {
            ui.visuals().text_color()
        },
    );
    ui.painter().text(
        rect.left_center() + vec2(44.0, 2.0),
        Align2::LEFT_CENTER,
        "分钟",
        FontId::proportional(12.0),
        ui.visuals().weak_text_color(),
    );
    if selected {
        icons::glyph(
            ui,
            rect.right_top() + vec2(-14.0, 14.0),
            Icon::Check,
            accent,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
