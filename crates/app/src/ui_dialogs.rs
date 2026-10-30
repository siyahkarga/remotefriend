//! Questions that need an answer before anything else: a connection request, switching
//! sides, an unknown server, closing the app. They show on top of every screen.

use egui::{Align, Layout, RichText};
use remote_friend_host::{Answer, Decision, PromptKind};

use crate::ui_kit::{self as kit, ACCENT, ERR, MUTED, OK, TEXT, WARN};
use crate::{App, Screen};

/// Seconds a connection question waits before it answers "deny" by itself (host: ASK_TIMEOUT).
const ASK_SECONDS: f32 = 30.0;

pub(crate) fn dialog(id: impl std::hash::Hash) -> egui::Modal {
    egui::Modal::new(egui::Id::new(id))
        .frame(
            egui::Frame::new()
                .fill(kit::SURFACE)
                .stroke(egui::Stroke::new(1.0_f32, kit::BORDER))
                .corner_radius(kit::R_CARD + 2)
                .inner_margin(22),
        )
        .backdrop_color(egui::Color32::from_black_alpha(150))
}

/// Icon, title and text side by side: the top of every dialog.
pub(crate) fn heading(ui: &mut egui::Ui, icon: &str, color: egui::Color32, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal_top(|ui| {
        kit::icon_disc(ui, icon, color);
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(RichText::new(title).size(18.0).color(TEXT).strong());
            add(ui);
        });
    });
}

impl App {
    /// Connection requests and other questions from the host engine.
    pub(crate) fn prompts_ui(&mut self, ctx: &egui::Context) {
        // Questions come from background threads: look twice a second on every screen, or one
        // that arrives while an idle screen (the welcome page) is shown waits for a mouse move.
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        let seq = remote_friend_host::prompt_sequence();
        if seq != self.last_prompt_seq {
            self.last_prompt_seq = seq;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical));
        }
        let prompts = remote_friend_host::pending_prompts();
        let Some(p) = prompts.first() else { return };
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        let answer = |a: Answer| remote_friend_host::answer(p.id, a);
        dialog(("prompt", p.id)).show(ctx, |ui| {
            ui.set_width(460.0);
            match &p.kind {
                PromptKind::Connection(req) => {
                    let (icon, color, title) =
                        if req.trusted { ("★", OK, "Trusted device") } else { ("🖥", ACCENT, "Connection request") };
                    heading(ui, icon, color, title, |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{} wants to see and control this computer.", req.name)).color(TEXT),
                            )
                            .wrap(),
                        );
                        kit::line(ui, RichText::new(&req.detail).size(12.0).color(MUTED)).on_hover_text(&req.detail);
                        if !req.trusted {
                            kit::note(ui, "They entered this computer's password. Allow only if you expect them.");
                        }
                    });
                    ui.add_space(14.0);
                    kit::countdown(ui, p.seconds_left as f32 / ASK_SECONDS, if req.trusted { OK } else { ACCENT });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if req.can_block
                            && ui
                                .add(kit::quiet("Block"))
                                .on_hover_text("Deny, and refuse this device from now on without asking")
                                .clicked()
                        {
                            answer(Answer::Connection(Decision::Block));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(kit::primary("Allow")).on_hover_text("Allow this connection").clicked() {
                                answer(Answer::Connection(Decision::Once));
                            }
                            if !req.trusted
                                && ui
                                    .add(kit::secondary("Always allow"))
                                    .on_hover_text("No password next time. You still confirm every connection.")
                                    .clicked()
                            {
                                answer(Answer::Connection(Decision::Trust));
                            }
                            if ui.add(kit::danger("Deny")).clicked() {
                                answer(Answer::Connection(Decision::Deny));
                            }
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("Denied automatically in {} s", p.seconds_left)).size(11.5).color(kit::FAINT));
                }
                PromptKind::SwitchSides { label } => {
                    heading(ui, "⇄", ACCENT, "Switch sides?", |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{label} offers to switch: you would control their computer, and they stop controlling this one."
                                ))
                                .color(TEXT),
                            )
                            .wrap(),
                        );
                    });
                    ui.add_space(14.0);
                    kit::split(ui, 32.0, |_| {}, |ui| {
                        if ui.add(kit::primary("Switch")).clicked() {
                            answer(Answer::Switch(true));
                        }
                        if ui.add(kit::secondary("No")).clicked() {
                            answer(Answer::Switch(false));
                        }
                    });
                    ui.label(RichText::new(format!("Declined automatically in {} s", p.seconds_left)).size(11.5).color(kit::FAINT));
                }
                PromptKind::ShowScreen { label, screen } => {
                    heading(ui, "🖥", ACCENT, "Show another screen?", |ui| {
                        ui.add(egui::Label::new(RichText::new(format!("{label} asks to see {screen}.")).color(TEXT)).wrap());
                        kit::note(ui, "Everyone connected sees the screen you show.");
                    });
                    ui.add_space(14.0);
                    kit::countdown(ui, p.seconds_left as f32 / ASK_SECONDS, ACCENT);
                    ui.add_space(12.0);
                    kit::split(ui, 32.0, |_| {}, |ui| {
                        if ui.add(kit::primary(&format!("Show {screen}"))).clicked() {
                            answer(Answer::Screen(true));
                        }
                        if ui.add(kit::secondary("Keep the current screen")).clicked() {
                            answer(Answer::Screen(false));
                        }
                    });
                    ui.label(RichText::new(format!("Declined automatically in {} s", p.seconds_left)).size(11.5).color(kit::FAINT));
                }
                PromptKind::ShareScreens { label, shared, total } => {
                    heading(ui, "🖥", ACCENT, "Share your other screens?", |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{label} asks to see another screen. Viewers can see {shared} of your {total} screens: only those picked in the desktop's sharing dialog."
                                ))
                                .color(TEXT),
                            )
                            .wrap(),
                        );
                        kit::note(ui, "RemoteFriends restarts and the desktop asks again: tick every screen there and keep remote interaction on. Viewers connect again afterwards.");
                    });
                    ui.add_space(14.0);
                    kit::countdown(ui, p.seconds_left as f32 / ASK_SECONDS, ACCENT);
                    ui.add_space(12.0);
                    kit::split(ui, 32.0, |_| {}, |ui| {
                        if ui.add(kit::primary("Share screens…")).clicked() {
                            answer(Answer::Screen(true));
                            // A moment for the viewers to hear why the connection ends.
                            std::thread::spawn(|| {
                                std::thread::sleep(std::time::Duration::from_millis(1500));
                                if !remote_friend_host::retry_screen_permission() {
                                    crate::restart_app();
                                }
                            });
                        }
                        if ui.add(kit::secondary("Not now")).clicked() {
                            answer(Answer::Screen(false));
                        }
                    });
                    ui.label(RichText::new(format!("Declined automatically in {} s", p.seconds_left)).size(11.5).color(kit::FAINT));
                }
                PromptKind::FileAccess { label, shared } => {
                    heading(ui, "📁", ACCENT, "Let them use your files?", |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{label} (a trusted device) wants to browse the folders and disks of this computer, send files into them and take files from them."
                                ))
                                .color(TEXT),
                            )
                            .wrap(),
                        );
                        kit::note(ui, "System folders and hidden files stay out of reach. You see every file that comes and goes.");
                        kit::note(ui, format!("Otherwise they can use only the shared folder: {shared}"));
                    });
                    ui.add_space(14.0);
                    kit::countdown(ui, p.seconds_left as f32 / 60.0, ACCENT);
                    ui.add_space(12.0);
                    kit::split(ui, 32.0, |_| {}, |ui| {
                        if ui.add(kit::primary("Allow for this session")).clicked() {
                            answer(Answer::Files(true));
                        }
                        if ui.add(kit::secondary("Only the shared folder")).clicked() {
                            answer(Answer::Files(false));
                        }
                    });
                    ui.label(RichText::new(format!("Only the shared folder in {} s", p.seconds_left)).size(11.5).color(kit::FAINT));
                }
                PromptKind::TrustServer { addr, fingerprint } => {
                    heading(ui, "🔒", WARN, "First connection to the server", |ui| {
                        kit::line(ui, RichText::new(format!("Server: {addr}")).color(TEXT));
                        kit::note(ui, "Its certificate fingerprint must match the one printed at the end of the server setup. It is remembered after you trust it.");
                        ui.add(egui::Label::new(RichText::new(fingerprint).monospace().size(12.0).color(TEXT)).wrap());
                    });
                    ui.add_space(14.0);
                    kit::split(ui, 32.0, |_| {}, |ui| {
                        if ui.add(kit::primary("Trust")).clicked() {
                            answer(Answer::Trust(true));
                        }
                        if ui.add(kit::secondary("Cancel")).clicked() {
                            answer(Answer::Trust(false));
                        }
                    });
                }
            }
        });
    }

    /// Closing ends remote access: ask first (also when a viewer tries to close it remotely).
    pub(crate) fn close_guard(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            let snap = remote_friend_host::snapshot();
            if snap.sessions > 0 || snap.online || matches!(self.screen, Screen::Viewer) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_confirm = true;
            }
        }
        if !self.close_confirm {
            return;
        }
        let sessions = remote_friend_host::sessions::count();
        let viewing = matches!(self.screen, Screen::Viewer);
        dialog("close_confirm").show(ctx, |ui| {
            ui.set_width(440.0);
            heading(ui, "⏻", ERR, "Close RemoteFriends?", |ui| {
                if sessions > 0 {
                    kit::note(ui, format!("{sessions} device(s) are connected to this computer right now. Closing ends their session."));
                }
                if viewing {
                    kit::note(ui, "Your connection to the remote computer ends too.");
                }
                kit::note(ui, "While RemoteFriends is closed, this computer cannot be reached remotely.");
            });
            ui.add_space(14.0);
            kit::split(ui, 32.0, |_| {}, |ui| {
                if ui.add(kit::danger_solid("Close")).clicked() {
                    self.close_confirm = false;
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.add(kit::secondary("Minimize instead")).clicked() {
                    self.close_confirm = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                if ui.add(kit::quiet("Cancel")).clicked() {
                    self.close_confirm = false;
                }
            });
        });
    }

    /// Server owner: "while you were away" when the server has unread notices. Once shown they
    /// count as read, so the same notices don't come back at every start.
    pub(crate) fn notices_ui(&mut self, ctx: &egui::Context) {
        if self.notices_prompted {
            return;
        }
        // Already looking at them.
        if self.page == crate::Page::Keys {
            self.notices_prompted = true;
            return;
        }
        let n = self.notices.lock().unwrap().clone();
        if !n.loaded {
            return;
        }
        // Only notices this computer hasn't shown yet.
        let shown_upto = remote_friend_common::identity::load_host_settings().notices_shown_upto;
        let fresh: Vec<&serde_json::Value> =
            n.events.iter().filter(|e| e["seen"] != true && e["id"].as_u64().unwrap_or(0) > shown_upto).collect();
        if fresh.is_empty() {
            self.notices_prompted = true;
            return;
        }
        if let Some(max) = fresh.iter().filter_map(|e| e["id"].as_u64()).max() {
            let mut s = remote_friend_common::identity::load_host_settings();
            if s.notices_shown_upto < max {
                s.notices_shown_upto = max;
                remote_friend_common::identity::save_host_settings(&s);
            }
        }
        let warnings = fresh.iter().filter(|e| e["kind"] == "warning").count();
        let (icon, color) = if warnings > 0 { ("⚠", WARN) } else { ("🔑", ACCENT) };
        dialog("notices").show(ctx, |ui| {
            ui.set_width(480.0);
            heading(ui, icon, color, "While you were away", |ui| {
                kit::note(ui, format!("Your server has {} new notice(s).", fresh.len()));
            });
            ui.add_space(10.0);
            for e in fresh.iter().take(5) {
                let mark = if e["kind"] == "warning" { "⚠ " } else { "" };
                ui.add(
                    egui::Label::new(RichText::new(format!("{mark}{}", e["text"].as_str().unwrap_or_default())).color(TEXT)).wrap(),
                );
            }
            ui.add_space(12.0);
            kit::split(ui, 32.0, |_| {}, |ui| {
                if ui.add(kit::primary("Open Access keys")).clicked() {
                    self.notices_prompted = true;
                    crate::notices::mark_seen(self.notices.clone());
                    self.keys_ui.show();
                    self.page = crate::Page::Keys;
                }
                if ui.add(kit::secondary("OK")).on_hover_text("They stay on the Access keys page").clicked() {
                    self.notices_prompted = true;
                    crate::notices::mark_seen(self.notices.clone());
                }
            });
        });
    }

    /// Devices page: confirm before a device may connect without anyone at this computer.
    pub(crate) fn confirm_mine_ui(&mut self, ctx: &egui::Context) {
        let Some((handle, name)) = self.confirm_mine.clone() else { return };
        dialog("confirm_mine").show(ctx, |ui| {
            ui.set_width(460.0);
            heading(ui, "🏠", WARN, &format!("Make “{name}” your device?"), |ui| {
                kit::note(
                    ui,
                    "It will connect without the password and without asking — even when nobody is at this computer. Only do this for computers and phones you own.",
                );
            });
            ui.add_space(14.0);
            kit::split(ui, 32.0, |_| {}, |ui| {
                if ui.add(kit::primary("Make it my device")).clicked() {
                    if remote_friend_host::set_device_mine(&handle, true) {
                        self.flash(format!("{name} is now one of your devices"));
                    }
                    self.confirm_mine = None;
                }
                if ui.add(kit::secondary("Cancel")).clicked() {
                    self.confirm_mine = None;
                }
            });
        });
    }
}
