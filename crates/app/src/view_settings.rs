//! The viewer's ⚙ menu, like the phone page's settings: picture quality and size, the shared
//! clipboard, what the computer's user allows, and how the connection runs.

use egui::RichText;

use crate::net::Perms;
use crate::ui_kit as kit;
use crate::App;

/// What the menu shows about the session (read once per frame by the viewer).
pub(crate) struct Facts {
    pub quality: String,
    pub perms: Perms,
    pub direct: bool,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub rtt: u32,
    pub os: String,
}

/// Picture sizes: None = fit the window, otherwise picture pixels per screen pixel.
const SIZES: [(Option<f32>, &str); 4] = [(None, "Fit"), (Some(1.0), "100%"), (Some(1.5), "150%"), (Some(2.0), "200%")];

impl App {
    pub(crate) fn settings_menu(&mut self, ui: &mut egui::Ui, f: &Facts) {
        ui.set_min_width(300.0);
        kit::section_label(ui, "PICTURE QUALITY");
        ui.horizontal(|ui| {
            for (p, label, tip) in [
                ("fast", "Fast", "Smooth on slow networks"),
                ("balanced", "Balanced", "Sharp enough, smooth enough"),
                ("sharp", "Sharp", "More detail, more data"),
            ] {
                if ui.selectable_label(f.quality == p, label).on_hover_text(tip).clicked() {
                    self.send_control(serde_json::json!({"t": "quality", "p": p}));
                }
            }
        });
        ui.add_space(6.0);
        kit::section_label(ui, "SIZE");
        ui.horizontal(|ui| {
            for (zoom, label) in SIZES {
                let tip = if zoom.is_none() { "The whole screen in the window" } else { "Larger: scroll bars move around" };
                if ui.selectable_label(self.view_zoom == zoom, label).on_hover_text(tip).clicked() {
                    self.view_zoom = zoom;
                }
            }
        });
        ui.add_space(6.0);
        kit::section_label(ui, "CLIPBOARD");
        ui.add_enabled_ui(f.perms.clipboard, |ui| {
            ui.checkbox(&mut self.clip_share, "Share the clipboard with that computer")
                .on_hover_text("Copied text goes both ways; off: each computer keeps its own");
        });
        if !f.perms.clipboard {
            kit::note(ui, "The computer's user turned the shared clipboard off.");
        }
        ui.add_space(6.0);
        kit::section_label(ui, "THE COMPUTER'S USER ALLOWS");
        let p = f.perms;
        ui.horizontal_wrapped(|ui| {
            for (on, what) in [(p.control, "Mouse & keyboard"), (p.sound, "Sound"), (p.files, "Files"), (p.clipboard, "Clipboard")] {
                let (mark, color) = if on { ("✔", kit::OK) } else { ("✖", kit::MUTED) };
                ui.label(RichText::new(format!("{mark} {what}")).color(color))
                    .on_hover_text(if on { "Allowed" } else { "Turned off by the computer's user" });
            }
        });
        ui.add_space(6.0);
        kit::section_label(ui, "CONNECTION");
        ui.label(if f.direct {
            "Direct: straight between the devices (peer-to-peer)"
        } else {
            "Through the server, end-to-end encrypted"
        });
        let rtt = if f.rtt > 0 { format!(" · {} ms", f.rtt) } else { String::new() };
        ui.label(RichText::new(format!("{}×{} · {:.0} fps{rtt}", f.width, f.height, f.fps)).color(kit::MUTED));
        if !f.os.is_empty() {
            let os = match f.os.as_str() {
                "windows" => "Windows",
                "macos" => "macOS",
                "linux" => "Linux",
                other => other,
            };
            ui.label(RichText::new(format!("That computer runs {os}")).color(kit::MUTED));
        }
    }
}
