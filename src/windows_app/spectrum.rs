use eframe::egui::{self, Color32, Rect, Ui};
use fnmusic::spectrum::BANDS;

use super::vault::Accent;

const BAR_PITCH: f32 = 7.0;
const BAR_GAP: f32 = 2.0;

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
        accent: Accent,
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
        let color = match accent {
            Accent::Red => super::motion::themed_color(
                ui.ctx(),
                Color32::from_rgb(93, 105, 126),
                Color32::from_rgb(155, 170, 192),
            ),
            Accent::Green | Accent::Blue => ui.visuals().hyperlink_color,
        };
        let rect = panel.shrink2(egui::vec2(12.0, 0.0));
        if rect.width() <= 0.0 {
            return;
        }
        let painter = ui
            .painter()
            .with_clip_rect(panel.intersect(ui.ctx().screen_rect()));
        let mut moving = false;
        for (level, target) in self.levels.iter_mut().zip(target) {
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
        }
        // Resample the measured bands at a steady spacing across the playback bar.
        let count = (rect.width() / BAR_PITCH).round().max(1.0) as usize;
        let pitch = rect.width() / count as f32;
        let width = pitch - BAR_GAP.min(pitch * 0.35);
        for index in 0..count {
            let band = ((index as f32 + 0.5) * BANDS as f32 / count as f32 - 0.5)
                .clamp(0.0, (BANDS - 1) as f32);
            let lower = band.floor() as usize;
            let upper = (lower + 1).min(BANDS - 1);
            let level = egui::lerp(self.levels[lower]..=self.levels[upper], band.fract());
            if level <= 0.005 {
                continue;
            }
            let x = rect.left() + pitch * index as f32 + (pitch - width) * 0.5;
            let bar = Rect::from_min_max(
                egui::pos2(x, rect.bottom() - 2.0 - level * 28.0),
                egui::pos2(x + width, rect.bottom() - 2.0),
            );
            painter.rect_filled(bar, 2, color.gamma_multiply(0.8 * (0.22 + level * 0.52)));
        }
        if animated && (playing || moving) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
}
