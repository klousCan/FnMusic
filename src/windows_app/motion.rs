use super::vault::AnimationMode;
use eframe::egui::{self, Id};

#[derive(Clone)]
struct ThemeTransition {
    from: egui::Visuals,
    to: egui::Visuals,
    start: f64,
    from_dark: f32,
}

pub fn set_theme(ctx: &egui::Context, target: egui::Visuals) {
    let key = Id::new("theme-initialized");
    let initialized = ctx.data(|data| data.get_temp::<bool>(key).unwrap_or(false));
    ctx.data_mut(|data| data.insert_temp(key, true));
    if initialized && ctx.style().animation_time > 0.0 {
        let transition = ThemeTransition {
            from_dark: f32::from(themed_color(ctx, egui::Color32::BLACK, egui::Color32::WHITE).r())
                / 255.0,
            from: ctx.style().visuals.clone(),
            to: target,
            start: ctx.input(|input| input.time),
        };
        ctx.data_mut(|data| data.insert_temp(Id::new("theme-transition"), transition));
        ctx.request_repaint();
        update_theme(ctx);
    } else {
        ctx.data_mut(|data| data.remove::<ThemeTransition>(Id::new("theme-transition")));
        ctx.set_visuals(target);
    }
}

pub fn update_theme(ctx: &egui::Context) {
    let key = Id::new("theme-transition");
    let Some(state) = ctx.data(|data| data.get_temp::<ThemeTransition>(key)) else {
        return;
    };
    let duration = ctx.style().animation_time as f64 * 1.5;
    let t = if duration <= 0.0 {
        1.0
    } else {
        ((ctx.input(|input| input.time) - state.start) / duration).clamp(0.0, 1.0) as f32
    };
    let t = t * t * (3.0 - 2.0 * t);
    let mut visual = state.to.clone();
    let mix = |a: egui::Color32, b| a.lerp_to_gamma(b, t);
    visual.panel_fill = mix(state.from.panel_fill, state.to.panel_fill);
    visual.window_fill = mix(state.from.window_fill, state.to.window_fill);
    visual.extreme_bg_color = mix(state.from.extreme_bg_color, state.to.extreme_bg_color);
    visual.faint_bg_color = mix(state.from.faint_bg_color, state.to.faint_bg_color);
    visual.code_bg_color = mix(state.from.code_bg_color, state.to.code_bg_color);
    visual.hyperlink_color = mix(state.from.hyperlink_color, state.to.hyperlink_color);
    visual.selection.bg_fill = mix(state.from.selection.bg_fill, state.to.selection.bg_fill);
    visual.selection.stroke.color = mix(
        state.from.selection.stroke.color,
        state.to.selection.stroke.color,
    );
    visual.window_stroke.color = mix(state.from.window_stroke.color, state.to.window_stroke.color);
    fn widget(from: &egui::style::WidgetVisuals, to: &mut egui::style::WidgetVisuals, t: f32) {
        to.bg_fill = from.bg_fill.lerp_to_gamma(to.bg_fill, t);
        to.weak_bg_fill = from.weak_bg_fill.lerp_to_gamma(to.weak_bg_fill, t);
        to.bg_stroke.color = from.bg_stroke.color.lerp_to_gamma(to.bg_stroke.color, t);
        to.fg_stroke.color = from.fg_stroke.color.lerp_to_gamma(to.fg_stroke.color, t);
    }
    widget(
        &state.from.widgets.noninteractive,
        &mut visual.widgets.noninteractive,
        t,
    );
    widget(
        &state.from.widgets.inactive,
        &mut visual.widgets.inactive,
        t,
    );
    widget(&state.from.widgets.hovered, &mut visual.widgets.hovered, t);
    widget(&state.from.widgets.active, &mut visual.widgets.active, t);
    widget(&state.from.widgets.open, &mut visual.widgets.open, t);
    ctx.set_visuals(visual);
    if t < 1.0 {
        ctx.request_repaint();
    } else {
        ctx.data_mut(|data| data.remove::<ThemeTransition>(key));
    }
}

pub fn themed_color(
    ctx: &egui::Context,
    light: egui::Color32,
    dark: egui::Color32,
) -> egui::Color32 {
    let key = Id::new("theme-transition");
    if let Some(state) = ctx.data(|data| data.get_temp::<ThemeTransition>(key)) {
        let duration = ctx.style().animation_time as f64 * 1.5;
        let t = if duration <= 0.0 {
            1.0
        } else {
            ((ctx.input(|input| input.time) - state.start) / duration).clamp(0.0, 1.0) as f32
        };
        let t = t * t * (3.0 - 2.0 * t);
        let target = if state.to.dark_mode { 1.0 } else { 0.0 };
        light.lerp_to_gamma(dark, state.from_dark * (1.0 - t) + target * t)
    } else if ctx.style().visuals.dark_mode {
        dark
    } else {
        light
    }
}

pub fn smooth_color(ctx: &egui::Context, id: Id, target: egui::Color32) -> egui::Color32 {
    let duration = ctx.style().animation_time * 3.0;
    let channel = |channel: u8, value| {
        ctx.animate_value_with_time(id.with(channel), value, duration)
            .round() as u8
    };
    egui::Color32::from_rgb(
        channel(0, target.r() as f32),
        channel(1, target.g() as f32),
        channel(2, target.b() as f32),
    )
}

pub fn skeleton(ui: &egui::Ui, rect: egui::Rect, radius: u8) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let animated = ui.style().animation_time > 0.0;
    let wave = if animated {
        (ui.input(|input| input.time) as f32 * 2.8).sin() * 0.5 + 0.5
    } else {
        0.3
    };
    let fill = ui
        .visuals()
        .widgets
        .inactive
        .weak_bg_fill
        .lerp_to_gamma(ui.visuals().weak_text_color(), 0.04 + wave * 0.07);
    ui.painter().rect_filled(rect, radius, fill);
    if animated {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(80));
    }
}

pub fn playing_indicator(ui: &egui::Ui, rect: egui::Rect, playing: bool) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let animated = playing && ui.style().animation_time > 0.0;
    let time = ui.input(|input| input.time) as f32;
    for index in 0..3 {
        let ratio = if animated {
            0.3 + 0.7
                * (time * (5.0 + index as f32) + index as f32 * 1.8)
                    .sin()
                    .abs()
        } else {
            [0.45, 0.8, 0.6][index]
        };
        let size = egui::vec2(rect.width() / 6.0, rect.height() * ratio);
        let center = egui::pos2(
            rect.left() + rect.width() * (index as f32 + 0.5) / 3.0,
            rect.center().y,
        );
        ui.painter().rect_filled(
            egui::Rect::from_center_size(center, size),
            1,
            ui.visuals().hyperlink_color,
        );
    }
    if animated {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(50));
    }
}

pub fn configure(ctx: &egui::Context, mode: AnimationMode) {
    let now = ctx.input(|input| input.time);
    let system = ctx.data_mut(|data| {
        let cached =
            data.get_temp_mut_or_insert_with(Id::new("system-motion"), || (-2.0_f64, true));
        if mode == AnimationMode::System && now - cached.0 >= 1.0 {
            use windows::Win32::UI::WindowsAndMessaging::{
                SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
                SystemParametersInfoW,
            };
            let mut enabled = 1_i32;
            let ok = unsafe {
                SystemParametersInfoW(
                    SPI_GETCLIENTAREAANIMATION,
                    0,
                    Some((&mut enabled as *mut i32).cast()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            }
            .is_ok();
            *cached = (now, !ok || enabled != 0);
        }
        cached.1
    });
    let enabled = mode == AnimationMode::On || (mode == AnimationMode::System && system);
    ctx.style_mut(|style| {
        style.animation_time = if enabled { 0.18 } else { 0.0 };
        style.scroll_animation = if enabled {
            egui::style::ScrollAnimation::duration(0.30)
        } else {
            egui::style::ScrollAnimation::none()
        };
    });
}

/// A bounded reveal keyed by actual content, without scheduling idle frames.
pub fn reveal(ctx: &egui::Context, slot: Id, content: Id) -> f32 {
    let duration = ctx.style().animation_time as f64;
    let now = ctx.input(|input| input.time);
    let start = ctx.data_mut(|data| {
        let state = data.get_temp_mut_or_insert_with(slot, || (content, now));
        if state.0 != content {
            *state = (content, now);
        }
        state.1
    });
    if duration <= 0.0 {
        return 1.0;
    }
    let t = ((now - start) / duration).clamp(0.0, 1.0) as f32;
    if t < 1.0 {
        ctx.request_repaint();
    }
    1.0 - (1.0 - t).powi(3)
}

/// Move the popup on the physical pixel grid without rescaling rasterized text.
pub fn popup(ui: &egui::Ui) {
    let ctx = ui.ctx();
    let frame = ctx.cumulative_pass_nr();
    let now = ctx.input(|input| input.time);
    let start = ctx.data_mut(|data| {
        let state =
            data.get_temp_mut_or_insert_with(ui.layer_id().id.with("popup-open"), || (frame, now));
        if frame > state.0 + 1 {
            state.1 = now;
        }
        state.0 = frame;
        state.1
    });
    let duration = ctx.style().animation_time as f64;
    let progress = if duration <= 0.0 {
        1.0
    } else {
        ((now - start) / duration).clamp(0.0, 1.0) as f32
    };
    let pixels_per_point = ctx.pixels_per_point();
    let offset = (6.0 * (1.0 - progress).powi(3) * pixels_per_point).round() / pixels_per_point;
    let transform = egui::emath::TSTransform::new(egui::vec2(0.0, offset), 1.0);
    ctx.set_transform_layer(ui.layer_id(), transform);
    if progress < 1.0 {
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_theme_change_resumes_from_current_colors_and_off_finishes_immediately() {
        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                time: Some(0.0),
                ..Default::default()
            },
            |ctx| {
                configure(ctx, AnimationMode::On);
                set_theme(ctx, egui::Visuals::dark());
            },
        );
        let _ = ctx.run(
            egui::RawInput {
                time: Some(1.0),
                ..Default::default()
            },
            |ctx| {
                set_theme(ctx, egui::Visuals::light());
            },
        );
        let _ = ctx.run(
            egui::RawInput {
                time: Some(1.13),
                ..Default::default()
            },
            |ctx| {
                update_theme(ctx);
                let before = ctx.style().visuals.panel_fill;
                let custom_before = themed_color(ctx, egui::Color32::WHITE, egui::Color32::BLACK);
                assert!(before.r() > 27 && before.r() < 248);
                set_theme(ctx, egui::Visuals::dark());
                assert_eq!(ctx.style().visuals.panel_fill, before);
                let custom_after = themed_color(ctx, egui::Color32::WHITE, egui::Color32::BLACK);
                assert!(custom_before.r().abs_diff(custom_after.r()) <= 1);
                configure(ctx, AnimationMode::Off);
                update_theme(ctx);
                assert_eq!(
                    ctx.style().visuals.panel_fill,
                    egui::Visuals::dark().panel_fill
                );
                assert!(
                    ctx.data(|data| data.get_temp::<ThemeTransition>(Id::new("theme-transition")))
                        .is_none()
                );
            },
        );
    }
}
