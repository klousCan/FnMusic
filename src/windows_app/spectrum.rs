use eframe::egui::{self, Rect, Ui};
use fnmusic::spectrum::BANDS;

pub struct SpectrumView {
    levels: [f32; BANDS],
    last_frame: Option<f64>,
}

impl Default for SpectrumView {
    fn default() -> Self {
        Self {
            levels: [0.0; BANDS],
            last_frame: None,
        }
    }
}

impl SpectrumView {
    pub fn paint(
        &mut self,
        ui: &Ui,
        panel: Rect,
        target: [f32; BANDS],
        enabled: bool,
        playing: bool,
    ) {
        if !enabled
            || ui
                .ctx()
                .input(|input| input.viewport().minimized.unwrap_or(false))
        {
            self.levels.fill(0.0);
            self.last_frame = None;
            return;
        }
        let now = ui.ctx().input(|input| input.time);
        let dt = self
            .last_frame
            .replace(now)
            .map_or(1.0 / 30.0, |last| (now - last).clamp(0.0, 0.1) as f32);
        let animated = ui.style().animation_time > 0.0;
        let color = ui.visuals().hyperlink_color;
        let rect = panel.shrink2(egui::vec2(12.0, 0.0));
        let painter = ui
            .painter()
            .with_clip_rect(panel.intersect(ui.ctx().screen_rect()));
        let pitch = rect.width() / BANDS as f32;
        let width = (pitch - 3.0).clamp(1.0, 9.0);
        let mut moving = false;
        for (index, (level, target)) in self.levels.iter_mut().zip(target).enumerate() {
            let target = target.clamp(0.0, 1.0);
            let seconds = if target > *level { 0.055 } else { 0.18 };
            let mix = if animated {
                1.0 - (-dt / seconds).exp()
            } else {
                1.0
            };
            *level += (target - *level) * mix;
            if *level < 0.005 {
                *level = 0.0;
            }
            moving |= *level > 0.0;
            if *level == 0.0 {
                continue;
            }
            let x = rect.left() + pitch * index as f32 + (pitch - width) * 0.5;
            let bar = Rect::from_min_max(
                egui::pos2(x, rect.bottom() - 2.0 - *level * 28.0),
                egui::pos2(x + width, rect.bottom() - 2.0),
            );
            painter.rect_filled(bar, 2, color.gamma_multiply(0.8 * (0.22 + *level * 0.52)));
        }
        if animated && (playing || moving) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
}
