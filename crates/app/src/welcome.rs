//! The welcome screen: the first thing a new install shows. To be reachable from anywhere a
//! computer needs an access key; here people get theirs with their email address and a 6-digit
//! code (relay signup.rs: the same address always gets the same key, 2 computers, 1 year).
//! It can be skipped (connecting to other computers needs no key) and opened again from Home.

use egui::{Align, RichText};
use serde_json::{json, Value};
use std::sync::mpsc;
use std::time::Duration;

use crate::ui_kit::{self as kit, ERR, FAINT, OK, TEXT, WARN};

#[derive(Clone, PartialEq)]
enum Step {
    Email,
    Code,
    /// The key is already used on that many computers (its limit).
    Full(u64),
    Done,
    /// Typing a key from the server's owner instead.
    HaveKey,
}

pub(crate) struct Welcome {
    step: Step,
    email: String,
    code: String,
    key: String,
    pending: Option<(mpsc::Receiver<Result<Value, String>>, Step)>,
    error: Option<String>,
}

/// What the screen wants from the app.
pub(crate) enum Outcome {
    Stay,
    /// Save this access key and reconnect.
    Key(String),
    /// Close the screen.
    Close,
}

impl Welcome {
    pub(crate) fn new() -> Self {
        Welcome { step: Step::Email, email: String::new(), code: String::new(), key: String::new(), pending: None, error: None }
    }

    /// Development aid (screenshots): open at a later step.
    #[cfg(debug_assertions)]
    pub(crate) fn demo(step: &str) -> Self {
        let mut w = Welcome::new();
        w.email = "ali@example.com".into();
        w.step = match step {
            "code" => Step::Code,
            "full" => Step::Full(2),
            "done" => Step::Done,
            "key" => Step::HaveKey,
            _ => Step::Email,
        };
        w
    }

    fn send(&mut self, request: Value, step: Step) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(remote_friend_host::signup(request));
        });
        self.pending = Some((rx, step));
        self.error = None;
    }

    fn verify(&mut self, reset: bool) {
        let me = remote_friend_host::snapshot();
        let request = json!({
            "cmd": "verify",
            "email": self.email.trim(),
            "code": self.code.trim(),
            "id": me.id,
            "name": me.name,
            "reset": reset,
        });
        self.send(request, Step::Code);
    }

    /// The whole window while it is shown.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, logo: Option<&egui::TextureHandle>) -> Outcome {
        let mut outcome = Outcome::Stay;
        if let Some((rx, sent_in)) = &self.pending {
            if let Ok(result) = rx.try_recv() {
                let sent_in = sent_in.clone();
                self.pending = None;
                match result {
                    Ok(answer) if answer["ok"] == true => match sent_in {
                        Step::Email => {
                            self.step = Step::Code;
                            self.code.clear();
                        }
                        _ => {
                            if let Some(key) = answer["key"].as_str() {
                                outcome = Outcome::Key(key.to_string());
                                self.step = Step::Done;
                            }
                        }
                    },
                    Ok(answer) if answer["full"] == true => self.step = Step::Full(answer["computers"].as_u64().unwrap_or(2)),
                    Ok(answer) => self.error = Some(answer["error"].as_str().unwrap_or("the server refused").to_string()),
                    Err(e) => self.error = Some(e),
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(200));
            }
        }
        let busy = self.pending.is_some();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(kit::BG).inner_margin(24)).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let width = ui.available_width().min(460.0);
                let side = ((ui.available_width() - width) / 2.0).max(0.0);
                ui.add_space((ui.available_height() * 0.08).min(60.0));
                ui.horizontal(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        ui.vertical_centered(|ui| {
                            if let Some(logo) = logo {
                                ui.image((logo.id(), egui::vec2(64.0, 64.0)));
                            }
                            ui.add_space(8.0);
                            ui.label(RichText::new("Welcome to RemoteFriend").size(24.0).color(TEXT).strong());
                        });
                        ui.add_space(14.0);
                        kit::card(ui, |ui| self.step_ui(ui, busy, &mut outcome));
                        ui.add_space(12.0);
                        if matches!(self.step, Step::Email | Step::Code | Step::HaveKey) {
                            // Centred: the two links are measured first.
                            let font = egui::TextStyle::Button.resolve(ui.style());
                            let text_w = |t: &str| ui.painter().layout_no_wrap(t.to_string(), font.clone(), TEXT).size().x + 8.0;
                            let mut links = text_w("Skip for now") + ui.spacing().item_spacing.x;
                            if self.step != Step::HaveKey {
                                links += text_w("I have an access key") + ui.spacing().item_spacing.x;
                            }
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.add_space(((width - links) / 2.0).max(0.0));
                                    if self.step != Step::HaveKey && ui.add(kit::quiet("I have an access key")).clicked() {
                                        self.step = Step::HaveKey;
                                        self.error = None;
                                    }
                                    if ui
                                        .add(kit::quiet("Skip for now"))
                                        .on_hover_text("You can still connect to other computers. Sign in later from Home.")
                                        .clicked()
                                    {
                                        outcome = Outcome::Close;
                                    }
                                });
                            });
                        }
                    });
                });
            });
        });
        outcome
    }

    fn step_ui(&mut self, ui: &mut egui::Ui, busy: bool, outcome: &mut Outcome) {
        let wide = |ui: &mut egui::Ui, b: egui::Button<'static>| kit::wide(ui, b, 40.0);
        match self.step.clone() {
            Step::Email => {
                ui.add(
                    egui::Label::new(
                        RichText::new("Sign in with your email address to reach this computer from anywhere — from your phone or another computer. It's free.")
                            .color(TEXT),
                    )
                    .wrap(),
                );
                ui.add_space(12.0);
                let resp = ui.add(kit::input(&mut self.email, "your@email.com").desired_width(f32::INFINITY));
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(8.0);
                let ready = !busy && self.email.contains('@') && self.email.contains('.');
                if (ui.add_enabled_ui(ready, |ui| wide(ui, kit::primary("Continue"))).inner.clicked() || (enter && ready)) && !busy {
                    let email = self.email.trim().to_string();
                    self.send(json!({ "cmd": "code", "email": email }), Step::Email);
                }
                kit::note(ui, "We'll email you a 6-digit code. Free for 2 computers, for 1 year.");
            }
            Step::Code => {
                ui.add(egui::Label::new(RichText::new(format!("Enter the code we sent to {}", self.email.trim())).color(TEXT)).wrap());
                ui.add_space(12.0);
                let resp = ui.add(
                    kit::input(&mut self.code, "123456")
                        .font(egui::FontId::monospace(24.0))
                        .horizontal_align(Align::Center)
                        .desired_width(f32::INFINITY),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(8.0);
                let ready = !busy && self.code.chars().filter(char::is_ascii_digit).count() == 6;
                if (ui.add_enabled_ui(ready, |ui| wide(ui, kit::primary("Confirm"))).inner.clicked() || (enter && ready)) && !busy {
                    self.verify(false);
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, kit::quiet("Send a new code")).clicked() {
                        let email = self.email.trim().to_string();
                        self.send(json!({ "cmd": "code", "email": email }), Step::Email);
                    }
                    ui.label(RichText::new("·").color(FAINT));
                    if ui.add(kit::quiet("Use another address")).clicked() {
                        self.step = Step::Email;
                        self.error = None;
                    }
                });
                kit::note(ui, "Check the spam folder too. The code works for 10 minutes.");
            }
            Step::Full(n) => {
                kit::callout(ui, WARN, |ui| {
                    ui.label(RichText::new(format!("Your key is already used on {n} computers.")).color(TEXT).strong());
                    kit::note(ui, "Use it on this computer? Your other computers lose their place; when they come online they take a free one again, so one of them may have to sign in again.");
                });
                ui.add_space(10.0);
                if wide(ui, kit::primary("Use it on this computer")).clicked() && !busy {
                    self.verify(true);
                }
                ui.add_space(4.0);
                if ui.add(kit::quiet("Cancel")).clicked() {
                    self.step = Step::Email;
                    self.code.clear();
                }
            }
            Step::Done => {
                ui.vertical_centered(|ui| {
                    kit::icon_disc(ui, "✔", OK);
                    ui.add_space(6.0);
                    ui.label(RichText::new("This computer can be reached from anywhere now.").size(16.0).color(TEXT).strong());
                    kit::note(ui, "On your other computer, sign in with the same address.");
                });
                ui.add_space(12.0);
                if wide(ui, kit::primary("Start")).clicked() {
                    *outcome = Outcome::Close;
                }
            }
            Step::HaveKey => {
                ui.add(egui::Label::new(RichText::new("Type the access key you got from the server's owner.").color(TEXT)).wrap());
                ui.add_space(12.0);
                ui.add(kit::input(&mut self.key, "RF-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX").desired_width(f32::INFINITY));
                ui.add_space(8.0);
                let ready = self.key.trim().len() >= 16;
                if ui.add_enabled_ui(ready, |ui| wide(ui, kit::primary("Save"))).inner.clicked() {
                    *outcome = Outcome::Key(self.key.trim().to_string());
                    self.step = Step::Done;
                }
                if ui.add(kit::quiet("Back to email sign-in")).clicked() {
                    self.step = Step::Email;
                }
            }
        }
        if busy {
            ui.add_space(6.0);
            ui.vertical_centered(|ui| ui.spinner());
        }
        if let Some(e) = &self.error {
            ui.add_space(6.0);
            kit::dot(ui, ERR, e.clone());
        }
    }
}
