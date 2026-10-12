//! Settings → Server → Manage access keys: the server owner lists, creates and revokes the keys
//! that let other people's computers use the server. It needs the owner key in Settings. Keys
//! made here are also remembered on this computer (issued_keys.json, with the email address
//! typed for them), because the server keeps only their hashes and could never show them again.
//! The email address is used only to address the email draft; it never goes to the server.

use egui::RichText;
use serde_json::{json, Value};
use std::sync::mpsc;
use std::time::Duration;

use crate::ui_kit::{self as kit, ERR, OK, WARN};

#[derive(Default)]
pub(crate) struct KeysUi {
    pending: Option<mpsc::Receiver<Result<Value, String>>>,
    loaded: bool,
    keys: Vec<Value>,
    error: Option<String>,
    name: String,
    email: String,
    computers: String,
    days: String,
    /// The key just created: (name, key, email).
    created: Option<(String, String, String)>,
    confirm_delete: Option<String>,
    issued: serde_json::Map<String, Value>,
}

impl KeysUi {
    pub(crate) fn show(&mut self) {
        self.error = None;
        self.created = None;
        self.confirm_delete = None;
        self.issued = load_issued();
        self.send(json!({ "cmd": "list" }));
    }

    fn send(&mut self, request: Value) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(remote_friend_host::manage_access_keys(request));
        });
        self.pending = Some(rx);
    }

    fn poll(&mut self) {
        let Some(rx) = &self.pending else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        self.pending = None;
        match result {
            Ok(answer) if answer["ok"] == true => {
                self.error = None;
                self.loaded = true;
                self.keys = answer["keys"].as_array().cloned().unwrap_or_default();
                if let (Some(key), Some(id)) = (answer["key"].as_str(), answer["id"].as_str()) {
                    let name = self.name.trim().to_string();
                    let email = self.email.trim().to_string();
                    self.issued.insert(id.to_string(), json!({ "name": name, "key": key, "email": email }));
                    save_issued(&self.issued);
                    if !email.is_empty() {
                        crate::open_url(&email_url(&name, &email, key));
                    }
                    self.created = Some((name, key.to_string(), email));
                    self.name.clear();
                    self.email.clear();
                    self.computers.clear();
                    self.days.clear();
                }
            }
            Ok(answer) => self.error = Some(answer["error"].as_str().unwrap_or("the server refused").to_string()),
            Err(e) => self.error = Some(e),
        }
    }

    /// The Access keys page (Settings → Server shows the way here for the owner).
    pub(crate) fn page(&mut self, ui: &mut egui::Ui) {
        self.poll();
        if self.pending.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(150));
        }
        if let Some(e) = &self.error {
            kit::callout(ui, ERR, |ui| {
                ui.label(RichText::new(e).color(kit::TEXT));
            });
            ui.add_space(10.0);
        }
        if let Some((name, key, email)) = self.created.clone() {
            kit::callout(ui, OK, |ui| {
                ui.label(RichText::new(format!("Key for {name}")).color(kit::TEXT).strong());
                ui.label(RichText::new(&key).monospace().size(17.0).color(kit::TEXT));
                if !email.is_empty() {
                    kit::note(ui, format!("An email to {email} was opened in your email program; check it and press Send."));
                }
                ui.horizontal(|ui| {
                    share_buttons(ui, &name, &key, &email);
                    if ui.add(kit::quiet("Done")).clicked() {
                        self.created = None;
                    }
                });
            });
            ui.add_space(10.0);
        }

        kit::titled_card(ui, "New key", "Name is required; the rest is optional.", |ui| {
            // Four fields in a row: the gaps and each field's inner margin (2 × 10) come off.
            let w = ((ui.available_width() - 3.0 * 10.0) / 4.0 - 20.0).max(80.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.add(kit::input(&mut self.name, "Name, e.g. Ali").desired_width(w));
                ui.add(kit::input(&mut self.email, "Email").desired_width(w))
                    .on_hover_text("Opens an email to this address with the key; kept only on this computer");
                ui.add(kit::input(&mut self.computers, "Computers: any").desired_width(w))
                    .on_hover_text("How many computers may use this key (empty: no limit)");
                ui.add(kit::input(&mut self.days, "Days: forever").desired_width(w))
                    .on_hover_text("How many days the key works (empty: no expiry)");
            });
            ui.add_space(6.0);
            let ready = !self.name.trim().is_empty() && self.pending.is_none();
            if ui.add_enabled(ready, kit::primary("Create key")).clicked() {
                match (number(&self.computers), number(&self.days)) {
                    _ if !email_ok(&self.email) => self.error = Some("That does not look like an email address.".into()),
                    (Ok(computers), Ok(days)) => {
                        let name = self.name.trim().to_string();
                        self.send(json!({ "cmd": "add", "name": name, "computers": computers, "days": days }));
                    }
                    _ => self.error = Some("Computers and days must be whole numbers (or empty).".into()),
                }
            }
        });
        ui.add_space(12.0);

        let mut request = None;
        kit::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Keys").size(16.0).color(kit::TEXT).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.pending.is_some() {
                        ui.spinner();
                    } else if ui.add(kit::quiet("Refresh")).clicked() {
                        self.send(json!({ "cmd": "list" }));
                    }
                });
            });
            ui.add_space(4.0);
            if self.loaded && self.keys.is_empty() {
                kit::empty(ui, "No keys yet.");
            }
            for k in self.keys.clone() {
                let id = k["id"].as_str().unwrap_or_default().to_string();
                let name = k["name"].as_str().unwrap_or_default().to_string();
                let used = k["computers"].as_u64().unwrap_or(0);
                let email = self.issued.get(&id).and_then(|v| v["email"].as_str()).unwrap_or_default().to_string();
                let computers = match k["max_computers"].as_u64() {
                    Some(max) => format!("{used} of {max} computers"),
                    None => format!("{used} computer(s)"),
                };
                let until = match k["expires"].as_str() {
                    Some(d) => format!("until {d}"),
                    None => "no expiry".to_string(),
                };
                let mut detail = vec![computers, until];
                if !email.is_empty() {
                    detail.insert(0, email.clone());
                }
                let status = k["status"].as_str().unwrap_or_default().to_string();
                let color = match status.as_str() {
                    "active" => OK,
                    "revoked" => ERR,
                    _ => WARN,
                };
                let key = self.issued.get(&id).and_then(|v| v["key"].as_str()).map(str::to_string);
                kit::row(
                    ui,
                    |ui| {
                        kit::row_text(ui, &name, &detail.join(" · "));
                    },
                    |ui| {
                        ui.menu_button(RichText::new("⋯").size(16.0), |ui| {
                            if status == "revoked" {
                                if ui.button("Restore").clicked() {
                                    request = Some(json!({ "cmd": "restore", "id": id }));
                                    ui.close();
                                }
                            } else if ui.button("Revoke").on_hover_text("Its computers go offline within seconds").clicked() {
                                request = Some(json!({ "cmd": "revoke", "id": id }));
                                ui.close();
                            }
                            if self.confirm_delete.as_deref() == Some(id.as_str()) {
                                if ui.button(RichText::new("Delete for good").color(ERR)).clicked() {
                                    request = Some(json!({ "cmd": "delete", "id": id }));
                                    self.confirm_delete = None;
                                    ui.close();
                                }
                            } else if ui.button("Delete…").clicked() {
                                self.confirm_delete = Some(id.clone());
                            }
                        });
                        if let Some(key) = &key {
                            share_buttons(ui, &name, key, &email);
                        }
                        kit::badge(ui, &status, color);
                    },
                );
            }
            ui.add_space(4.0);
            kit::note(ui, "The server keeps only a hash of each key. Keys made here are remembered on this computer so you can copy them again.");
        });
        if let Some(req) = request {
            if self.pending.is_none() {
                self.send(req);
            }
        }
    }
}

/// Copy and Email buttons for a key (laid out right to left in rows).
fn share_buttons(ui: &mut egui::Ui, name: &str, key: &str, email: &str) {
    if ui.add(kit::secondary("Email…")).on_hover_text("Opens your email program with the key and instructions").clicked() {
        crate::open_url(&email_url(name, email, key));
    }
    if ui.add(kit::secondary("Copy")).clicked() {
        ui.ctx().copy_text(key.to_string());
    }
}

/// Empty, or something@domain.tld without spaces.
fn email_ok(email: &str) -> bool {
    let email = email.trim();
    email.is_empty()
        || (email.len() <= 254
            && !email.chars().any(|c| c.is_whitespace() || c.is_control() || c == '?' || c == '&')
            && email.split_once('@').is_some_and(|(user, domain)| !user.is_empty() && domain.contains('.')))
}

fn email_url(name: &str, email: &str, key: &str) -> String {
    let body = format!(
        "Hi {name},\n\nhere is your RemoteFriend access key:\n\n    {key}\n\n\
         1. Download RemoteFriend: https://blobidea.com/products/remotefriend\n\
         2. Open it, go to Settings > Server > Access key, paste the key and click \"Save and reconnect\".\n\n\
         The key is personal, please keep it to yourself.\n"
    );
    let to = percent(email.trim()).replace("%40", "@");
    format!("mailto:{to}?subject={}&body={}", percent("Your RemoteFriend access key"), percent(&body))
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Empty: none; otherwise a whole number.
fn number(s: &str) -> Result<Option<i64>, ()> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse::<i64>().map(Some).map_err(|_| ())
}

fn issued_path() -> std::path::PathBuf {
    remote_friend_common::identity::config_dir().join("issued_keys.json")
}

fn load_issued() -> serde_json::Map<String, Value> {
    std::fs::read_to_string(issued_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_issued(map: &serde_json::Map<String, Value>) {
    let data = serde_json::to_vec_pretty(map).unwrap_or_default();
    let _ = remote_friend_common::identity::write_private(&issued_path(), &data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_link_is_encoded() {
        let url = email_url("Ali Veli", "", "RF-ABCDE-FGHJK");
        assert!(url.starts_with("mailto:?subject=Your%20RemoteFriend%20access%20key&body=Hi%20Ali%20Veli"));
        assert!(email_url("Ali", "ali@example.com", "RF-X").starts_with("mailto:ali@example.com?subject="));
        assert!(email_ok("") && email_ok(" ali@example.com ") && !email_ok("ali") && !email_ok("a b@c.d") && !email_ok("a@b"));
        assert!(url.contains("RF-ABCDE-FGHJK") && !url.contains(' ') && !url.contains('\n'));
        assert_eq!(number(" 3 "), Ok(Some(3)));
        assert_eq!(number(""), Ok(None));
        assert!(number("x").is_err());
    }
}
