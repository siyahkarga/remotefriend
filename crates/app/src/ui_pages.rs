//! Pages of the main window besides Home: Devices, History, Access keys and Settings.

use egui::{Align, Layout, RichText};
use serde_json::json;

use crate::ui_kit::{self as kit, ACCENT, ERR, FAINT, MUTED, OK, TEXT, WARN};
use crate::{autostart, restart_app, App, Page};

/// "3 Oct 19:37" for a list.
fn when(secs: u64) -> String {
    remote_friend_host::local_time(secs)
}

/// Page content that scrolls, with a comfortable maximum width.
fn page_body(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
        // A little room on the right for the scroll bar.
        let width = (ui.available_width() - 12.0).min(820.0);
        ui.allocate_ui_with_layout(egui::vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(width);
            add(ui);
        });
    });
}

/// Label column of a settings form.
fn field_label(ui: &mut egui::Ui, text: &str, tip: &str) {
    ui.allocate_ui_with_layout(egui::vec2(120.0, 30.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(120.0);
        let resp = ui.add(egui::Label::new(RichText::new(text).color(MUTED)).truncate());
        if !tip.is_empty() {
            resp.on_hover_text(tip);
        }
    });
}

/// "app 83.135.241.220 (via internet)" -> "internet · 83.135.241.220".
fn peer_text(peer: &str) -> String {
    let internet = peer.contains("(via internet)");
    let addr = peer.trim_end_matches(" (via internet)").trim_start_matches("app ").trim();
    if internet { format!("internet · {addr}") } else { format!("local network · {addr}") }
}

impl App {
    // ---- Devices ----

    pub(crate) fn devices_page(&mut self, ui: &mut egui::Ui) {
        let devices = remote_friend_host::trusted_devices();
        let blocked = remote_friend_host::blocked_devices();
        page_body(ui, "devices", |ui| {
            kit::page_header(
                ui,
                "Devices",
                "Devices that may connect without this computer's password. Everyone else needs the password and your OK.",
            );

            kit::titled_card(
                ui,
                "🏠  My devices",
                "Connect without the password and without asking — even when nobody is at this computer. Only for your own computers and phones.",
                |ui| {
                    let mine: Vec<_> = devices.iter().filter(|d| d.mine).collect();
                    if mine.is_empty() {
                        kit::empty(ui, "None yet. Connect from your device once, choose “Always allow”, then press “Make it my device” under Trusted.");
                    }
                    for d in mine {
                        kit::row(
                            ui,
                            |ui| kit::row_text(ui, &d.label, &format!("Last used {} · added {}", when(d.last_used), when(d.added))),
                            |ui| {
                                if ui.add(kit::danger("Remove")).on_hover_text("It will need the password and your OK again").clicked()
                                    && remote_friend_host::remove_device(&d.handle)
                                {
                                    self.flash(format!("{} needs the password again", d.label));
                                }
                                if ui.add(kit::secondary("Ask me each time")).on_hover_text("Move it to Trusted: no password, but you confirm every connection").clicked()
                                    && remote_friend_host::set_device_mine(&d.handle, false)
                                {
                                    self.flash(format!("You will be asked when {} connects", d.label));
                                }
                            },
                        );
                    }
                },
            );
            ui.add_space(12.0);

            kit::titled_card(ui, "★  Trusted", "No password needed, but you confirm every connection.", |ui| {
                let trusted: Vec<_> = devices.iter().filter(|d| !d.mine).collect();
                if trusted.is_empty() {
                    kit::empty(ui, "None yet. Choose “Always allow” when a device asks to connect.");
                }
                for d in trusted {
                    kit::row(
                        ui,
                        |ui| kit::row_text(ui, &d.label, &format!("Last used {} · added {}", when(d.last_used), when(d.added))),
                        |ui| {
                            if ui.add(kit::danger("Block")).on_hover_text("Refuse it from now on without asking").clicked()
                                && remote_friend_host::block_device(&d.handle)
                            {
                                self.flash(format!("{} is blocked", d.label));
                            }
                            if ui.add(kit::secondary("Remove")).on_hover_text("It will need the password again").clicked()
                                && remote_friend_host::remove_device(&d.handle)
                            {
                                self.flash(format!("{} needs the password again", d.label));
                            }
                            if ui.add(kit::secondary("Make it my device")).on_hover_text("Connects without asking: only for devices you own").clicked() {
                                self.confirm_mine = Some((d.handle.clone(), d.label.clone()));
                            }
                        },
                    );
                }
            });
            ui.add_space(12.0);

            kit::titled_card(ui, "⛔  Blocked", "Refused without asking, even with the right password.", |ui| {
                if blocked.is_empty() {
                    kit::empty(ui, "Nobody is blocked. “Block” is offered when a device asks to connect.");
                }
                for b in &blocked {
                    kit::row(
                        ui,
                        |ui| kit::row_text(ui, &b.label, &format!("Blocked {}", when(b.added))),
                        |ui| {
                            if ui.add(kit::secondary("Unblock")).clicked() && remote_friend_host::unblock(&b.handle) {
                                self.flash(format!("{} may ask again", b.label));
                            }
                        },
                    );
                }
            });
            if !devices.is_empty() {
                ui.add_space(6.0);
                if ui.add(kit::quiet("Forget all my and trusted devices")).clicked() {
                    let n = remote_friend_host::forget_devices();
                    self.flash(format!("{n} device(s) need the password again"));
                }
            }
        });
    }

    // ---- History ----

    pub(crate) fn history_page(&mut self, ui: &mut egui::Ui) {
        use remote_friend_host::sessions;
        let recent = sessions::recent();
        page_body(ui, "history", |ui| {
            kit::split(
                ui,
                40.0,
                |ui| {
                    ui.vertical(|ui| kit::page_header(ui, "History", "Who connected to this computer recently."));
                },
                |ui| {
                    if !recent.is_empty() && ui.add(kit::secondary("Clear")).clicked() {
                        sessions::clear_recent();
                    }
                },
            );
            kit::card(ui, |ui| {
                if recent.is_empty() {
                    kit::empty(ui, "No connections yet.");
                }
                for r in &recent {
                    let detail = format!(
                        "{} · {} · {}{}",
                        when(r.start),
                        remote_friend_host::duration_text(r.secs),
                        if r.app { "app" } else { "browser" },
                        if r.trusted { " · trusted" } else { "" },
                    );
                    kit::row(ui, |ui| kit::row_text(ui, &r.label, &detail), |ui| {
                        kit::line(ui, RichText::new(peer_text(&r.peer)).size(12.0).color(FAINT));
                    });
                }
            });
        });
    }

    // ---- Access keys ----

    pub(crate) fn keys_page(&mut self, ui: &mut egui::Ui) {
        page_body(ui, "keys", |ui| {
            kit::page_header(
                ui,
                "Access keys",
                "A key makes someone's computer reachable through your server. Connecting to a computer never needs one.",
            );
            self.notices_card(ui);
            self.keys_ui.page(ui);
        });
    }

    /// The server's notices for its owner, newest first.
    fn notices_card(&mut self, ui: &mut egui::Ui) {
        let n = self.notices.lock().unwrap().clone();
        if !n.loaded {
            return;
        }
        let subtitle = match (n.signup, n.mail) {
            (true, true) => "People can get a free key by email in the app (2 computers, 1 year). You also get these by email.",
            (true, false) => "People can get a free key by email in the app (2 computers, 1 year). Email notices are off on the server (RF_OWNER_EMAIL).",
            (false, _) => "Free keys by email are off on the server: it has no mail service key (resend.key).",
        };
        kit::titled_card(ui, "Notices", subtitle, |ui| {
            if n.events.is_empty() {
                kit::empty(ui, "Nothing yet. New free keys and warnings show up here.");
            }
            let mut request = None;
            for e in n.events.iter().take(20) {
                let id = e["id"].as_u64().unwrap_or(0);
                let warning = e["kind"] == "warning";
                let unseen = e["seen"] != true;
                let text = e["text"].as_str().unwrap_or_default();
                let done = e["done"].as_str().unwrap_or_default();
                let time = format!("{} UTC", e["time"].as_str().unwrap_or_default());
                egui::Frame::new().fill(kit::SURFACE_HI).corner_radius(kit::R_ROW).inner_margin(12).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    kit::split(
                        ui,
                        20.0,
                        |ui| {
                            ui.label(RichText::new(&time).size(12.0).color(MUTED));
                        },
                        |ui| {
                            if warning {
                                kit::badge(ui, "warning", ERR);
                            } else if unseen {
                                kit::badge(ui, "new", ACCENT);
                            }
                        },
                    );
                    ui.add(egui::Label::new(RichText::new(text).color(TEXT)).wrap());
                    if !done.is_empty() {
                        kit::dot(ui, OK, done);
                        return;
                    }
                    let (label, tip, req) = match e["action"].as_str().unwrap_or("") {
                        "revoke" => (
                            "Revoke this key",
                            "Its computers go offline; you can restore it in the list below",
                            json!({ "cmd": "revoke", "id": e["key"], "event": id }),
                        ),
                        "block" => (
                            "Block this address for 30 days",
                            "It can no longer connect, sign up or reach any computer through your server",
                            json!({ "cmd": "block", "event": id }),
                        ),
                        "more" => (
                            "Allow 25 more today",
                            "More free keys today; tomorrow the normal limit applies again",
                            json!({ "cmd": "more", "event": id }),
                        ),
                        _ => return,
                    };
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let confirming = self.notice_confirm == Some(id);
                        let button = if e["action"] == "more" {
                            kit::secondary(label)
                        } else if confirming {
                            kit::danger_solid("Click again to confirm")
                        } else {
                            kit::danger(label)
                        };
                        if ui.add_enabled(!n.busy, button).on_hover_text(tip).clicked() {
                            if e["action"] == "more" || confirming {
                                self.notice_confirm = None;
                                request = Some(req);
                            } else {
                                self.notice_confirm = Some(id);
                            }
                        }
                        if confirming && ui.add(kit::quiet("Cancel")).clicked() {
                            self.notice_confirm = None;
                        }
                    });
                });
            }
            if let Some(req) = request {
                crate::notices::act(self.notices.clone(), req, ui.ctx().clone());
            }
            if let Some(err) = &n.error {
                kit::dot(ui, ERR, err.clone());
            }
            if n.unseen > 0 && ui.add(kit::secondary("Mark all as read")).clicked() {
                crate::notices::mark_seen(self.notices.clone());
            }
        });
        ui.add_space(12.0);
    }

    // ---- Settings ----

    pub(crate) fn settings_page(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot) {
        page_body(ui, "settings", |ui| {
            kit::page_header(ui, "Settings", "");
            let mut tab = self.settings_tab;
            kit::segmented(
                ui,
                &mut tab,
                &[(0u8, "Server", ""), (1u8, "Security", ""), (2u8, "Privacy", ""), (3u8, "Connection", ""), (4u8, "General", "")],
            );
            self.settings_tab = tab;
            ui.add_space(12.0);
            match self.settings_tab {
                0 => self.settings_server(ui, snap),
                1 => self.settings_security(ui, snap),
                2 => self.settings_privacy(ui),
                3 => self.settings_connection(ui),
                _ => self.settings_general(ui),
            }
        });
    }

    fn settings_server(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot) {
        kit::card(ui, |ui| {
            kit::setting_row(
                ui,
                "Reachable over the internet",
                "Through a relay server. Off: only your local network can reach this computer.",
                |ui| {
                    kit::switch(ui, &mut self.settings_use_server);
                },
            );
            kit::divider(ui);
            ui.add_enabled_ui(self.settings_use_server, |ui| {
                egui::Grid::new("server_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    field_label(ui, "Access key", "Lets this computer use the server; ask the server owner for one");
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - 120.0).max(120.0);
                        ui.add(kit::input(&mut self.settings_key, "RF-XXXXX-…").password(!self.settings_key_visible).desired_width(width));
                        let label = if self.settings_key_visible { "Hide" } else { "Show" };
                        if ui.add(kit::quiet(label)).clicked() {
                            self.settings_key_visible = !self.settings_key_visible;
                        }
                        if !self.settings_key.trim().is_empty() && ui.add(kit::quiet("Copy")).clicked() {
                            ui.ctx().copy_text(self.settings_key.trim().to_string());
                            self.flash("Access key copied");
                        }
                    });
                    ui.end_row();
                    let built_in_available = !remote_friend_common::identity::DEFAULT_SERVER.is_empty();
                    field_label(ui, "Server", "Leave empty for the built-in server; another relay: host:33202");
                    ui.add(
                        kit::input(&mut self.settings_server, if built_in_available { "built-in server" } else { "host:33202" })
                            .desired_width(ui.available_width()),
                    );
                    ui.end_row();
                    let built_in = self.settings_server.trim().is_empty() && built_in_available;
                    field_label(ui, "Phone address", "Web address phones open, e.g. https://remote.example.com");
                    ui.add(
                        kit::input(&mut self.settings_web, if built_in { "built-in server's page" } else { "https://…" })
                            .desired_width(ui.available_width()),
                    );
                    ui.end_row();
                });
            });
            ui.add_space(6.0);
            kit::note(ui, "Connecting to other computers needs no key. The access key only makes this computer reachable from anywhere.");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(kit::primary("Save and reconnect")).clicked() {
                    remote_friend_host::set_server(&self.settings_server, &self.settings_web, &self.settings_key, self.settings_use_server);
                    self.flash("Server settings saved");
                }
                match (&snap.server, snap.online, &snap.server_error) {
                    (_, true, _) => {
                        kit::dot(ui, OK, "Connected");
                    }
                    (Some(_), false, Some(e)) if e.starts_with("registration rejected: ") => {
                        let reason = e.trim_start_matches("registration rejected: ");
                        let reason = reason.split(" (RemoteFriend").next().unwrap_or(reason);
                        kit::dot(ui, WARN, format!("The server says: {reason}"));
                    }
                    (Some(_), false, Some(e)) => {
                        kit::dot(ui, ERR, e.clone());
                    }
                    (Some(_), false, None) => {
                        kit::dot(ui, WARN, "Connecting…");
                    }
                    (None, _, _) => {}
                }
            });
        });
        // No working key yet: the welcome screen gets a free one by email.
        let refused = snap.server_error.as_deref().is_some_and(|e| e.starts_with("registration rejected"));
        let owner = crate::ui_home::owner_key_like(&self.settings_key);
        if self.settings_use_server && !owner && (self.settings_key.trim().is_empty() || refused) {
            ui.add_space(12.0);
            kit::callout(ui, ACCENT, |ui| {
                kit::split(
                    ui,
                    32.0,
                    |ui| {
                        ui.add(egui::Label::new(RichText::new("No access key? Get a free one with your email address.").color(TEXT)).wrap());
                    },
                    |ui| {
                        if ui.add(kit::primary("Sign in with email")).clicked() {
                            self.welcome = Some(crate::welcome::Welcome::new());
                        }
                    },
                );
            });
        }
        // Only the server's owner has a key that is not a personal "RF-…" access key.
        if owner {
            ui.add_space(12.0);
            kit::callout(ui, ACCENT, |ui| {
                kit::split(
                    ui,
                    32.0,
                    |ui| {
                        ui.add(egui::Label::new(RichText::new("This is the server's owner key: you can give other people access keys.").color(TEXT)).wrap());
                    },
                    |ui| {
                        if ui.add(kit::primary("Access keys")).clicked() {
                            self.keys_ui.show();
                            crate::notices::refresh(self.notices.clone(), ui.ctx().clone());
                            self.page = Page::Keys;
                        }
                    },
                );
            });
        }
    }

    fn settings_security(&mut self, ui: &mut egui::Ui, snap: &remote_friend_host::Snapshot) {
        use remote_friend_host::PasswordPolicy as P;
        if !snap.password_from_env {
            kit::titled_card(ui, "Password", "Only devices connecting for the first time need it.", |ui| {
                let mut policy = remote_friend_host::password_policy();
                let mut changed = false;
                for (value, title, description) in [
                    (P::OnStart, "Changes when RemoteFriends starts", "Recommended"),
                    (P::AfterSession, "Changes after every session", "For giving the password to people you help once"),
                    (P::Never, "Stays the same", "Until you press “New password”"),
                ] {
                    ui.horizontal(|ui| {
                        changed |= ui.radio_value(&mut policy, value, RichText::new(title).color(TEXT)).changed();
                        ui.label(RichText::new(description).size(12.0).color(FAINT));
                    });
                }
                if changed {
                    remote_friend_host::set_password_policy(policy);
                }
                ui.add_space(6.0);
                if ui.add(kit::secondary("New password now")).clicked() && remote_friend_host::renew_password().is_some() {
                    self.show_password = true;
                    self.flash("New password created");
                }
            });
            ui.add_space(12.0);
        }
        kit::card(ui, |ui| {
            kit::setting_row(
                ui,
                "Devices that don't need the password",
                &format!("{} device(s): your own devices and trusted ones, and who is blocked.", snap.trusted_devices),
                |ui| {
                    if ui.add(kit::secondary("Open Devices")).clicked() {
                        self.page = Page::Devices;
                    }
                },
            );
        });
    }

    fn settings_privacy(&mut self, ui: &mut egui::Ui) {
        kit::titled_card(ui, "What viewers see", "", |ui| {
            let mut full = remote_friend_host::privacy::full_view();
            if kit::segmented(
                ui,
                &mut full,
                &[(false, "🛡 Safe view", ""), (true, "🖥 Everything", "")],
            ) {
                remote_friend_host::privacy::set_full_view(full);
            }
            ui.add_space(4.0);
            if full {
                kit::note(ui, "Viewers see the screen exactly as you do, RemoteFriends included.");
            } else {
                kit::note(ui, "RemoteFriends and the private apps below are hidden from viewers.");
                if crate::privacy::wayland() {
                    kit::callout(ui, WARN, |ui| {
                        kit::note(
                            ui,
                            "This Linux desktop (Wayland) doesn't tell apps where windows are, so only RemoteFriends itself can be hidden (it is minimized while someone watches). Other windows stay visible.",
                        );
                    });
                }
            }
        });
        ui.add_space(12.0);
        kit::titled_card(
            ui,
            "Private apps",
            "Windows whose app or title contains one of these words are blacked out in the safe view (Windows, macOS, X11). Browsers keep their tab bar visible so viewers can switch tabs.",
            |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.settings_private_apps)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .margin(egui::Margin::symmetric(10, 8))
                        .background_color(egui::Color32::from_rgb(13, 15, 19))
                        .hint_text(RichText::new("keepass, bitwarden, banking").color(FAINT)),
                );
                ui.add_space(6.0);
                if ui.add(kit::primary("Save list")).clicked() {
                    let list = self.settings_private_apps.split([',', '\n']).map(|s| s.trim().to_string()).collect();
                    remote_friend_host::privacy::set_private_apps(list);
                    self.flash("Private apps saved");
                }
                ui.add_space(4.0);
                kit::note(
                    ui,
                    "Some apps hide themselves from every screen capture (e.g. Signal's “Screen security”); turn that off in the app if viewers should see it.",
                );
            },
        );
    }

    fn settings_connection(&mut self, ui: &mut egui::Ui) {
        kit::card(ui, |ui| {
            let mut direct = remote_friend_host::p2p::enabled();
            kit::setting_row(
                ui,
                "Direct connections when possible",
                "Picture, sound and input go straight between the devices instead of through the server — still end-to-end encrypted. The devices learn each other's internet address.",
                |ui| {
                    if kit::switch(ui, &mut direct).changed() {
                        remote_friend_host::set_direct_enabled(direct);
                    }
                },
            );
            kit::divider(ui);
            let mut lan = self.lan_enabled;
            kit::setting_row(
                ui,
                "Allow connections from the local network",
                "Computers on the same Wi-Fi can connect without the server. The phone page on the local network is plain http (not encrypted).",
                |ui| {
                    if kit::switch(ui, &mut lan).changed() {
                        remote_friend_host::set_lan_enabled(lan);
                        self.lan_enabled = lan;
                        self.lan_changed = !self.lan_changed;
                    }
                },
            );
            if self.lan_changed {
                ui.add_space(6.0);
                kit::callout(ui, WARN, |ui| {
                    kit::split(
                        ui,
                        32.0,
                        |ui| {
                            ui.label(RichText::new("Restart RemoteFriends to apply this.").color(TEXT));
                        },
                        |ui| {
                            if ui.add(kit::primary("Restart now")).clicked() {
                                restart_app();
                            }
                        },
                    );
                });
            }
        });
    }

    fn settings_general(&mut self, ui: &mut egui::Ui) {
        kit::card(ui, |ui| {
            let mut auto = self.autostart;
            kit::setting_row(ui, "Start RemoteFriends when I log in", "So this computer can be reached after a restart.", |ui| {
                if kit::switch(ui, &mut auto).changed() {
                    match autostart::set(auto) {
                        Ok(()) => self.autostart = auto,
                        Err(e) => self.flash(format!("Could not change: {e}")),
                    }
                }
            });
            kit::divider(ui);
            let mut check = crate::update::enabled();
            kit::setting_row(
                ui,
                "Check for new versions",
                "Asks blobidea.com twice a day which version is current; nothing else is sent.",
                |ui| {
                    if kit::switch(ui, &mut check).changed() {
                        crate::update::set_enabled(check);
                    }
                },
            );
            kit::divider(ui);
            let mut auto = crate::update::automatic();
            kit::setting_row(
                ui,
                "Install updates automatically",
                "Only when nobody is connected, after a 30-second notice. Updates are signed and checked before they run. On Linux (.deb) the desktop asks for your password, so you start those yourself.",
                |ui| {
                    if kit::switch(ui, &mut auto).changed() {
                        crate::update::set_automatic(auto);
                    }
                },
            );
            kit::divider(ui);
            let dir = remote_friend_host::receive_dir();
            kit::setting_row(ui, "Received files", &dir.display().to_string(), |ui| {
                if ui.add(kit::secondary("Open folder")).clicked() {
                    let _ = std::fs::create_dir_all(&dir);
                    crate::open_path(&dir);
                }
            });
        });
        ui.add_space(12.0);
        kit::card(ui, |ui| {
            ui.label(RichText::new(format!("RemoteFriends {}", env!("CARGO_PKG_VERSION"))).color(TEXT).strong());
            ui.horizontal_wrapped(|ui| {
                kit::link(ui, "blobidea.com/products/remotefriend", crate::update::DOWNLOAD_PAGE);
                ui.label(RichText::new("·").color(FAINT));
                kit::link(ui, "Source code (GPL-3.0)", "https://github.com/siyahkarga/remotefriend");
            });
            kit::note(ui, "Security problems: support@blobidea.com");
        });
    }
}
