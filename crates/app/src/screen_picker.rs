//! Choosing which of the computer's screens to see: a picture of the screens as they are arranged
//! on that desktop (places and sizes only, never their content), opened once when a computer with
//! several screens first shows its picture, and from the session bar any time later.
//! Screens open to this viewer switch at once when mouse & keyboard are allowed; any other one
//! is asked of the computer's user, and the picker waits for the answer.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, FontId, Rect, RichText, Sense, Stroke, Vec2};

use crate::net::{ScreenInfo, Shared};
use crate::ui_kit as kit;
use crate::App;

/// The computer's user has 30 s to answer (host core.rs); a little more here.
const ANSWER_WAIT: Duration = Duration::from_secs(35);

impl App {
    /// What became of a screen request: shown, refused, or no answer in time.
    pub(crate) fn screen_answers(&mut self, shared: &Arc<Mutex<Shared>>, current: usize) {
        let refused = shared.lock().unwrap().screen_refused.take();
        let Some((wanted, since)) = self.screens_pending else {
            return;
        };
        if current == wanted {
            self.screens_pending = None;
            self.screens_open = false;
        } else if refused.is_some() {
            self.screens_pending = None;
            self.screens_note = Some(format!("The computer's user kept the current screen (screen {} was not shown).", wanted + 1));
        } else if since.elapsed() > ANSWER_WAIT {
            self.screens_pending = None;
            self.screens_note = Some("No answer from the computer's user.".into());
        }
    }

    /// The picker window. Returns the screen to ask the computer for.
    pub(crate) fn screen_picker(&mut self, ctx: &egui::Context, screens: &[ScreenInfo], current: usize, control: bool) -> Option<usize> {
        let mut open = true;
        let mut chosen = None;
        let waiting = self.screens_pending.map(|(i, _)| i);
        egui::Window::new("Choose a screen")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(480.0);
                ui.label(
                    RichText::new(format!("This computer has {} screens. Which one do you want to see?", screens.len()))
                        .color(kit::MUTED),
                );
                ui.add_space(10.0);
                if let Some(i) = draw_layout(ui, screens, current, control, waiting) {
                    chosen = Some(i);
                }
                ui.add_space(8.0);
                for s in screens {
                    let state = if s.i == current {
                        "shown now"
                    } else if Some(s.i) == waiting {
                        "waiting for the computer's user"
                    } else if s.open && control {
                        ""
                    } else {
                        "the computer's user is asked first"
                    };
                    let text = if state.is_empty() { s.label.clone() } else { format!("{} — {state}", s.label) };
                    if ui.selectable_label(s.i == current, text).clicked() {
                        chosen = Some(s.i);
                    }
                }
                ui.add_space(8.0);
                if let Some(i) = waiting {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Asked the computer's user to show screen {}…", i + 1));
                    });
                } else if let Some(note) = &self.screens_note {
                    ui.colored_label(kit::WARN, note);
                } else if !control {
                    kit::note(ui, "Without mouse & keyboard permission, the computer's user is asked before the screen changes.");
                }
                ui.add_space(8.0);
                if ui.add(kit::secondary("Keep this screen")).clicked() {
                    self.screens_open = false;
                }
            });
        if !open {
            self.screens_open = false;
        }
        let i = chosen?;
        if i == current {
            self.screens_open = false;
            return None;
        }
        if waiting.is_some() {
            return None; // one question at a time
        }
        let direct = screens.iter().find(|s| s.i == i).is_some_and(|s| s.open && control);
        self.screens_note = None;
        if direct {
            self.screens_open = false;
        } else {
            self.screens_pending = Some((i, Instant::now()));
        }
        Some(i)
    }
}

/// The screens as arranged on that desktop, scaled into the window. Returns a clicked screen.
fn draw_layout(ui: &mut egui::Ui, screens: &[ScreenInfo], current: usize, control: bool, waiting: Option<usize>) -> Option<usize> {
    // Computers before v0.15 send no places: side by side then.
    let placed = screens.iter().any(|s| s.x != 0 || s.y != 0);
    let mut next_x = 0.0;
    let boxes: Vec<Rect> = screens
        .iter()
        .map(|s| {
            let size = Vec2::new(s.w.max(1) as f32, s.h.max(1) as f32);
            let min = if placed {
                egui::pos2(s.x as f32, s.y as f32)
            } else {
                next_x += size.x;
                egui::pos2(next_x - size.x, 0.0)
            };
            Rect::from_min_size(min, size)
        })
        .collect();
    let bounds = boxes.iter().fold(Rect::NOTHING, |acc, r| acc.union(*r));
    let room = Vec2::new(ui.available_width(), 190.0);
    let scale = (room.x / bounds.width()).min(room.y / bounds.height());
    let (area, _) = ui.allocate_exact_size(Vec2::new(room.x, bounds.height() * scale), Sense::hover());
    let origin = area.center_top() - Vec2::new(bounds.width() * scale / 2.0, 0.0);
    let painter = ui.painter_at(area);
    let mut clicked = None;
    for (s, b) in screens.iter().zip(&boxes) {
        let rect = Rect::from_min_size(origin + (b.min - bounds.min) * scale, b.size() * scale).shrink(3.0);
        let resp = ui
            .interact(rect, ui.id().with(("screen", s.i)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let is_current = s.i == current;
        let direct = s.open && control;
        let fill = if is_current {
            kit::tint(kit::ACCENT, 0.35)
        } else if resp.hovered() {
            kit::SURFACE_HI
        } else {
            kit::SURFACE
        };
        let stroke = if is_current {
            Stroke::new(2.0_f32, kit::ACCENT)
        } else if !direct || Some(s.i) == waiting {
            Stroke::new(1.5_f32, kit::WARN)
        } else {
            Stroke::new(1.0_f32, kit::BORDER)
        };
        painter.rect(rect, 6.0, fill, stroke, egui::StrokeKind::Inside);
        painter.text(rect.center() - Vec2::new(0.0, 8.0), Align2::CENTER_CENTER, (s.i + 1).to_string(), FontId::proportional(26.0), kit::TEXT);
        let tag = if is_current {
            "shown now".to_string()
        } else if Some(s.i) == waiting {
            "asking…".to_string()
        } else if !direct {
            "asks first".to_string()
        } else {
            format!("{}×{}", s.w, s.h)
        };
        let tag_color = if is_current { kit::TEXT } else { kit::MUTED };
        painter.text(rect.center() + Vec2::new(0.0, 16.0), Align2::CENTER_CENTER, tag, FontId::proportional(11.5), tag_color);
        if s.primary {
            painter.text(rect.left_top() + Vec2::new(6.0, 4.0), Align2::LEFT_TOP, "main", FontId::proportional(10.5), kit::MUTED);
        }
        if resp.clicked() {
            clicked = Some(s.i);
        }
    }
    clicked
}
