//! The computer's clipboard (text), kept in sync with viewers that are allowed to use it.
//!
//! One thread owns the system clipboard: it applies text sent by viewers and polls for
//! changes made on this computer, which are published to the sessions.

use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::broadcast;

/// Longest text synced (bigger clipboards are not sent).
pub(crate) const MAX_TEXT: usize = 256 * 1024;
const POLL: Duration = Duration::from_millis(700);

fn hub() -> &'static broadcast::Sender<String> {
    static HUB: OnceLock<broadcast::Sender<String>> = OnceLock::new();
    HUB.get_or_init(|| broadcast::channel(8).0)
}

/// Commands for the clipboard thread (started on first use).
fn thread() -> &'static Mutex<Option<Sender<String>>> {
    static TX: OnceLock<Mutex<Option<Sender<String>>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = channel::<String>();
        let started = std::thread::Builder::new().name("rf-clipboard".into()).spawn(move || {
            let mut cb = match arboard::Clipboard::new() {
                Ok(c) => c,
                Err(e) => {
                    tracing::info!("clipboard sync unavailable: {e}");
                    return;
                }
            };
            // Text that is already known (set by a viewer or already sent): no echo.
            let mut known = cb.get_text().unwrap_or_default();
            loop {
                match rx.recv_timeout(POLL) {
                    Ok(text) => {
                        if cb.set_text(text.clone()).is_ok() {
                            known = text;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if hub().receiver_count() == 0 {
                            continue;
                        }
                        if let Ok(text) = cb.get_text() {
                            if text != known && !text.is_empty() && text.len() <= MAX_TEXT {
                                known = text.clone();
                                let _ = hub().send(text);
                            }
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Mutex::new(started.ok().map(|_| tx))
    })
}

/// Changes of this computer's clipboard (while subscribed).
pub(crate) fn subscribe() -> broadcast::Receiver<String> {
    let _ = thread();
    hub().subscribe()
}

/// Put text from a viewer on this computer's clipboard.
pub(crate) fn set(text: String) {
    if text.len() > MAX_TEXT {
        return;
    }
    if let Some(tx) = thread().lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send(text);
    }
}
