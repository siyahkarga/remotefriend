//! A small bar on top of everything while someone is connected: how many, for how long,
//! which screen they see, and buttons to open RemoteFriends, take back control or end it.
//! It stays when the main window is minimized or covered, so this computer's user always
//! knows the screen is shared.
//!
//! Viewers see it too (it holds nothing secret), but their clicks and keys can't use it
//! (App::raw_input_hook runs for this window as well).

use egui::{Color32, RichText};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::ui_kit::{self as kit, ERR, MUTED, OK, TEXT};

const WIDTH: f32 = 640.0;
const HEIGHT: f32 = 46.0;

/// Call every frame of the main window: shows the bar while someone is connected.
pub(crate) fn show(ctx: &egui::Context) {
    if remote_friend_host::sessions::count() == 0 {
        return;
    }
    let mut builder = egui::ViewportBuilder::default()
        .with_title(remote_friend_host::privacy::SESSION_BAR_TITLE)
        .with_app_id("remotefriend")
        .with_inner_size([WIDTH, HEIGHT])
        .with_decorations(false)
        .with_resizable(false)
        .with_always_on_top()
        .with_taskbar(false)
        .with_active(false)
        .with_window_type(egui::X11WindowType::Utility);
    // Top centre of the screen the main window is on.
    if let Some(size) = ctx.input(|i| i.viewport().monitor_size) {
        builder = builder.with_position([((size.x - WIDTH) / 2.0).max(0.0), 6.0]);
    }
    ctx.show_viewport_deferred(egui::ViewportId::from_hash_of("rf-session-bar"), builder, |ctx, _| bar(ctx));
}

/// "4:05" or "1:02:33".
fn clock(secs: u64) -> String {
    match secs {
        s if s < 3600 => format!("{}:{:02}", s / 60, s % 60),
        s => format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60),
    }
}

fn bar(ctx: &egui::Context) {
    use remote_friend_host::sessions;
    ctx.request_repaint_after(Duration::from_millis(500));
    let live = sessions::list();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let since = live.iter().map(|s| s.since).min().unwrap_or(now);
    let blocked = sessions::input_blocked();
    let screens = remote_friend_host::monitors().len();
    let screen = remote_friend_host::current_monitor() + 1;
    let frame = egui::Frame::new()
        .fill(kit::SIDEBAR)
        .stroke(egui::Stroke::new(1.0_f32, kit::tint(OK, 0.45)))
        .corner_radius(kit::R_ROW)
        .inner_margin(egui::Margin::symmetric(10, 6));
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        // The bar moves when dragged by any empty part.
        let drag = ui.interact(ui.max_rect(), egui::Id::new("session-bar-drag"), egui::Sense::drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        ui.horizontal_centered(|ui| {
            // A slowly pulsing dot: something is happening.
            let t = ctx.input(|i| i.time) as f32;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            let glow = 0.55 + 0.45 * (t * 2.5).sin().abs();
            let color = if blocked { kit::WARN } else { OK };
            ui.painter().circle_filled(rect.center(), 5.0, color.gamma_multiply(glow));
            let who: Vec<String> = live.iter().map(|s| format!("{} ({})", s.label, s.access.word())).collect();
            let count = if live.len() == 1 { "1 connected".to_string() } else { format!("{} connected", live.len()) };
            ui.label(RichText::new(count).color(TEXT).strong()).on_hover_text(who.join("\n"));
            ui.label(RichText::new(clock(now.saturating_sub(since))).color(MUTED).monospace());
            let shown = if screens > 1 { format!("Screen {screen} of {screens}") } else { "Screen shared".to_string() };
            ui.label(RichText::new(shown).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let end = egui::Button::new(RichText::new("Disconnect").color(Color32::WHITE)).fill(kit::tint(ERR, 0.7));
                if ui.add(end).on_hover_text("End every session now").clicked() {
                    for s in &live {
                        sessions::disconnect(s.id);
                    }
                }
                let (label, tip) = if blocked {
                    ("Give back control", "Viewers may use mouse and keyboard again")
                } else {
                    ("Take back control", "Viewers can only watch until you give it back")
                };
                if ui.add(kit::secondary(label)).on_hover_text(tip).clicked() {
                    sessions::set_input_blocked(!blocked);
                }
                if ui.add(kit::quiet("Open")).on_hover_text("Show the RemoteFriends window").clicked() {
                    ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_clock_reads_like_a_stopwatch() {
        assert_eq!(super::clock(0), "0:00");
        assert_eq!(super::clock(245), "4:05");
        assert_eq!(super::clock(3753), "1:02:33");
    }
}
