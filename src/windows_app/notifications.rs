use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Frame, RichText, Stroke};
use fnmusic::library::Page;

use super::{
    icons::{self, Icon},
    widgets,
};

const LIFETIME: Duration = Duration::from_millis(6500);
const EXIT: Duration = Duration::from_millis(180);

#[derive(Clone, PartialEq)]
pub enum Action {
    Playback(String),
    Login,
    Page(Page, u32),
    Lyrics(u64),
    Favorite(u64, String, bool),
    CheckUpdates,
    DownloadUpdate,
    OpenUpdates,
}

impl Action {
    fn label(&self) -> &'static str {
        match self {
            Self::Playback(_) => "重新播放",
            Self::Login => "重新登录",
            Self::Page(..) | Self::Lyrics(_) | Self::Favorite(..) => "重试",
            Self::CheckUpdates => "重新检查",
            Self::DownloadUpdate => "重新下载",
            Self::OpenUpdates => "查看更新",
        }
    }
}

struct Tip {
    id: u64,
    title: String,
    description: String,
    details: String,
    action: Option<Action>,
    count: u32,
    expanded: bool,
    error: bool,
    remaining: Duration,
    created: Instant,
    ticked: Instant,
    closing: Option<Instant>,
}

#[derive(Default)]
pub struct Notifications {
    tips: Vec<Tip>,
    next_id: u64,
}

impl Notifications {
    pub fn clear(&mut self) {
        self.tips.clear();
    }

    pub fn error(
        &mut self,
        title: &str,
        description: &str,
        details: impl Into<String>,
        action: Option<Action>,
    ) {
        self.push(title, description, details.into(), action, true);
    }

    pub fn info(&mut self, message: String) {
        self.push(&message, "", String::new(), None, false);
    }

    fn push(
        &mut self,
        title: &str,
        description: &str,
        details: String,
        action: Option<Action>,
        error: bool,
    ) {
        let now = Instant::now();
        if let Some(tip) = self.tips.iter_mut().find(|tip| {
            tip.closing.is_none()
                && tip.title == title
                && tip.details == details
                && tip.action == action
        }) {
            tip.count = tip.count.saturating_add(1);
            tip.remaining = LIFETIME;
            tip.ticked = now;
            return;
        }
        self.next_id += 1;
        self.tips.insert(
            0,
            Tip {
                id: self.next_id,
                title: title.into(),
                description: description.into(),
                details,
                action,
                count: 1,
                expanded: false,
                error,
                remaining: LIFETIME,
                created: now,
                ticked: now,
                closing: None,
            },
        );
        self.tips.truncate(8);
    }

    pub fn show(&mut self, ctx: &egui::Context, top: f32) -> Vec<Action> {
        let now = Instant::now();
        let animated = ctx.style().animation_time > 0.0;
        self.tips.retain(|tip| {
            tip.closing
                .is_none_or(|at| animated && now.duration_since(at) < EXIT)
        });
        let mut actions = Vec::new();
        let width = 360.0_f32.min(ctx.screen_rect().width() - 32.0);
        let visible = if ctx.screen_rect().height() < 650.0 {
            2
        } else {
            3
        };
        let mut y = top;
        for tip in self.tips.iter_mut().take(visible) {
            let progress = if !animated {
                1.0
            } else if let Some(at) = tip.closing {
                1.0 - (now.duration_since(at).as_secs_f32() / EXIT.as_secs_f32()).min(1.0)
            } else {
                let t = (now.duration_since(tip.created).as_secs_f32() / 0.22).min(1.0);
                1.0 - (1.0 - t).powi(3)
            };
            let mut close = false;
            let mut focused = false;
            let response = egui::Area::new(egui::Id::new(("error-tip", tip.id)))
                .order(egui::Order::Tooltip)
                .anchor(
                    Align2::RIGHT_TOP,
                    egui::vec2(-16.0, y - 8.0 * (1.0 - progress)),
                )
                .show(ctx, |ui| {
                    ui.set_opacity(progress);
                    Frame::new()
                        .fill(ui.visuals().window_fill)
                        .stroke(ui.visuals().window_stroke)
                        .corner_radius(16)
                        .inner_margin(16)
                        .shadow(egui::Shadow {
                            offset: [0, 6],
                            blur: 24,
                            spread: 0,
                            color: Color32::BLACK.gamma_multiply(if ui.visuals().dark_mode {
                                0.24
                            } else {
                                0.08
                            }),
                        })
                        .show(ui, |ui| {
                            ui.set_width(width - 32.0);
                            ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
                            let muted = widgets::secondary_text(ui);
                            let badge = if tip.error {
                                super::motion::themed_color(
                                    ctx,
                                    Color32::from_rgb(194, 57, 47),
                                    Color32::from_rgb(255, 133, 124),
                                )
                            } else {
                                ui.visuals().hyperlink_color
                            };
                            ui.horizontal_top(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(30.0, 30.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(
                                    rect.center(),
                                    15.0,
                                    badge.gamma_multiply(0.12),
                                );
                                if tip.error {
                                    ui.painter().text(
                                        rect.center(),
                                        Align2::CENTER_CENTER,
                                        "!",
                                        FontId::proportional(19.0),
                                        badge,
                                    );
                                } else {
                                    icons::glyph(ui, rect.center(), Icon::Check, badge);
                                }
                                ui.vertical(|ui| {
                                    ui.set_width(width - 114.0);
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(RichText::new(&tip.title).size(14.0).strong());
                                        if tip.count > 1 {
                                            ui.label(
                                                RichText::new(format!("×{}", tip.count))
                                                    .size(11.0)
                                                    .color(muted),
                                            );
                                        }
                                    });
                                    if !tip.description.is_empty() {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&tip.description)
                                                    .size(12.0)
                                                    .color(muted),
                                            )
                                            .wrap(),
                                        );
                                    }
                                });
                                let response =
                                    icons::small(ui, Icon::WindowClose, "关闭提示", false);
                                close |= response.clicked();
                                focused |= response.has_focus();
                            });
                            if tip.action.is_some() || !tip.details.is_empty() {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    ui.add_space(40.0);
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    if let Some(action) = &tip.action {
                                        let accent = ui.visuals().hyperlink_color;
                                        let response = ui.add(
                                            egui::Button::new(
                                                RichText::new(action.label())
                                                    .size(12.0)
                                                    .color(accent),
                                            )
                                            .fill(accent.gamma_multiply(0.11))
                                            .stroke(Stroke::NONE)
                                            .corner_radius(7)
                                            .min_size(egui::vec2(0.0, 28.0)),
                                        );
                                        if response.clicked() {
                                            actions.push(action.clone());
                                            close = true;
                                        }
                                        focused |= response.has_focus();
                                    }
                                    if !tip.details.is_empty() {
                                        let response = ui.add(
                                            egui::Button::new(
                                                RichText::new(if tip.expanded {
                                                    "收起详情"
                                                } else {
                                                    "查看详情"
                                                })
                                                .size(12.0)
                                                .color(muted),
                                            )
                                            .frame(false)
                                            .min_size(egui::vec2(0.0, 28.0)),
                                        );
                                        if response.clicked() {
                                            tip.expanded = !tip.expanded;
                                        }
                                        focused |= response.has_focus();
                                    }
                                });
                            }
                            if tip.expanded {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    ui.add_space(40.0);
                                    ui.vertical(|ui| {
                                        ui.set_width(width - 72.0);
                                        egui::ScrollArea::vertical()
                                            .id_salt(("tip-details", tip.id))
                                            .max_height(112.0)
                                            .show(ui, |ui| {
                                                ui.set_width((ui.available_width() - 8.0).max(0.0));
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&tip.details)
                                                            .size(11.0)
                                                            .color(muted),
                                                    )
                                                    .wrap(),
                                                );
                                            });
                                        let copy = ui.add(
                                            egui::Button::new(
                                                RichText::new("复制详情").size(11.0).color(muted),
                                            )
                                            .frame(false),
                                        );
                                        if copy.clicked() {
                                            ctx.copy_text(tip.details.clone());
                                        }
                                        focused |= copy.has_focus();
                                    });
                                });
                            }
                        });
                });
            let hovered = response
                .response
                .rect
                .contains(ctx.pointer_hover_pos().unwrap_or(egui::Pos2::ZERO));
            if !hovered && !focused && !tip.expanded {
                tip.remaining = tip.remaining.saturating_sub(now.duration_since(tip.ticked));
            }
            tip.ticked = now;
            if close || tip.remaining.is_zero() {
                tip.closing.get_or_insert(now);
            }
            if progress < 1.0 || tip.closing.is_some() {
                ctx.request_repaint_after(Duration::from_millis(16));
            } else if !hovered && !focused && !tip.expanded {
                ctx.request_repaint_after(tip.remaining);
            }
            y += response.response.rect.height() + 10.0;
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_errors_merge_and_actions_remain_distinct() {
        let mut notifications = Notifications::default();
        notifications.error("连接失败", "请重试", "HTTP 502", Some(Action::Login));
        notifications.tips[0].remaining = Duration::ZERO;
        notifications.error("连接失败", "请重试", "HTTP 502", Some(Action::Login));
        assert_eq!(notifications.tips.len(), 1);
        assert_eq!(notifications.tips[0].count, 2);
        assert_eq!(notifications.tips[0].remaining, LIFETIME);
        notifications.error("连接失败", "请重试", "HTTP 502", Some(Action::CheckUpdates));
        assert_eq!(notifications.tips.len(), 2);
        notifications.clear();
        assert!(notifications.tips.is_empty());
    }
}
