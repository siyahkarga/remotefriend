//! Client: izleyen + kontrol eden taraf.
//! JPEG decode + eframe görüntü + mouse/klavye gönder + dosya gönder.

use anyhow::{Context, Result};
use remote_friend_common::{Handshake, InputEvent, MouseButton, Packet, FileChunk, RemoteKey, PROTOCOL_VERSION};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

struct Shared {
    texture: Option<(egui::ColorImage, u32, u32)>, // son frame
    status: String,
    host_w: u32,
    host_h: u32,
    fps: f32,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args: Vec<String> = std::env::args().collect();
    let def_host = args.get(1).cloned().unwrap_or("192.168.178.31:33200".into());
    let def_pass = args.get(2).cloned().unwrap_or("1234".into());
    let auto = args.len() > 1;

    let mut app = App {
        screen: Screen::Login,
        host_field: def_host,
        pass_field: def_pass,
        login_msg: "Host adresini girip Bağlan'a bas.".into(),
        shared: None,
        tx_out: None,
        last_mouse: None,
        mods: [false; 4],
        scroll_acc: (0.0, 0.0),
    };
    if auto {
        app.connect();
    }

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native("RemoteFriend", opts, Box::new(|_| Ok(Box::new(app))))
        .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

async fn net_loop(
    host: &str,
    password: &str,
    shared: Arc<Mutex<Shared>>,
    mut rx_out: UnboundedReceiver<Packet>,
) -> Result<()> {
    let mut socket = tokio::net::TcpStream::connect(host)
        .await
        .with_context(|| format!("bağlanamadı: {host}"))?;
    let hs = Packet::Handshake(Handshake {
        version: PROTOCOL_VERSION,
        password: password.to_string(),
        want_video: true,
        want_input: true,
    });
    write_packet(&mut socket, &hs).await?;
    let resp = read_packet(&mut socket).await?;
    match resp {
        Packet::Accept => shared.lock().unwrap().status = "bağlı".into(),
        Packet::Reject(m) => {
            shared.lock().unwrap().status = format!("reddedildi: {m}");
            anyhow::bail!("reject: {m}");
        }
        _ => anyhow::bail!("beklenmeyen yanıt"),
    }

    let (mut rd, mut wr) = socket.into_split();

    // gönderici (async recv: thread'i kilitlemez)
    tokio::spawn(async move {
        while let Some(p) = rx_out.recv().await {
            let buf = match remote_friend_common::encode(&p) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if wr.write_u32(buf.len() as u32).await.is_err() { break; }
            if wr.write_all(&buf).await.is_err() { break; }
        }
    });

    // alıcı: video (bağlantı başına taze decoder: P-frame'ler için şart)
    let mut last = SystemTime::now();
    let mut n = 0u32;
    let mut total = 0u64;
    let mut h264 = H264Dec::new()?;
    loop {
        let pkt = read_packet_split(&mut rd).await?;
        if let Packet::Video(f) = pkt {
            n += 1;
            total += 1;
            let img = match f.codec {
                remote_friend_common::VideoCodec::H264 => match h264.decode_frame(&f.data) {
                    Ok(img) => img,
                    Err(e) => {
                        if total < 10 || total % 100 == 0 {
                            tracing::warn!("h264 decode atlandı: {e:#}");
                        }
                        continue;
                    }
                },
                remote_friend_common::VideoCodec::Jpeg => decode_jpeg(&f.data)?,
                remote_friend_common::VideoCodec::RawRgba => continue, // placeholder, gösterme
            };
            let mut s = shared.lock().unwrap();
            s.host_w = f.width;
            s.host_h = f.height;
            s.texture = Some((img, f.width, f.height));
            let el = last.elapsed().unwrap().as_secs_f32();
            if el >= 1.0 {
                s.fps = n as f32 / el;
                n = 0;
                last = SystemTime::now();
            }
            if total % 50 == 0 {
                tracing::info!("video akiyor: toplam {total} frame");
            }
        }
    }
}

fn decode_jpeg(data: &[u8]) -> Result<egui::ColorImage> {
    let dynimg = image::load_from_memory(data).context("jpeg decode")?;
    let rgb = dynimg.to_rgba8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let pixels = rgb.into_raw();
    Ok(egui::ColorImage::from_rgba_unmultiplied([w, h], &pixels))
}

/// Bağlantı başına bir decoder (P-frame zinciri için state şart).
struct H264Dec {
    dec: openh264::decoder::Decoder,
}

impl H264Dec {
    fn new() -> Result<Self> {
        Ok(Self { dec: openh264::decoder::Decoder::new().context("h264 decoder açılamadı")? })
    }

    fn decode_frame(&mut self, data: &[u8]) -> Result<egui::ColorImage> {
        use openh264::formats::YUVSource;
        let mut last: Option<(usize, usize, Vec<u8>)> = None;
        for nal in openh264::nal_units(data) {
            match self.dec.decode(nal) {
                Ok(Some(yuv)) => {
                    let (w, h) = yuv.dimensions();
                    let mut rgba = vec![0u8; w * h * 4];
                    yuv.write_rgba8(&mut rgba);
                    last = Some((w, h, rgba));
                }
                Ok(None) => {}
                Err(_) => {} // bozuk NAL atla, IDR'de toparlar
            }
        }
        let (w, h, rgba) = last.context("çözülebilir frame yok (henüz IDR gelmedi)")?;
        Ok(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
    }
}

enum Screen {
    Login,
    Viewer,
}

struct App {
    screen: Screen,
    // login ekranı
    host_field: String,
    pass_field: String,
    login_msg: String,
    // viewer (connect sonrası)
    shared: Option<Arc<Mutex<Shared>>>,
    tx_out: Option<UnboundedSender<Packet>>,
    last_mouse: Option<(u32, u32)>,
    mods: [bool; 4], // shift, ctrl, alt, meta (basılı mı)
    scroll_acc: (f32, f32),
}

impl App {
    /// Bağlan düğmesi / otomatik bağlanma: net thread'i başlat, görüntüye geç.
    fn connect(&mut self) {
        let host = self.host_field.trim().to_string();
        let password = self.pass_field.clone();
        if host.is_empty() {
            self.login_msg = "Adres boş olamaz.".into();
            return;
        }
        tracing::info!("bağlanılıyor: {host}");
        let shared = Arc::new(Mutex::new(Shared {
            texture: None,
            status: "bağlanıyor...".into(),
            host_w: 0,
            host_h: 0,
            fps: 0.0,
        }));
        let (tx_out, rx_out) = unbounded_channel::<Packet>();
        let sh = shared.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                if let Err(e) = net_loop(&host, &password, sh.clone(), rx_out).await {
                    sh.lock().unwrap().status = format!("hata: {e:#}");
                    tracing::warn!("net kapandı: {e:#}");
                }
            });
        });
        self.shared = Some(shared);
        self.tx_out = Some(tx_out);
        self.last_mouse = None;
        self.mods = [false; 4];
        self.scroll_acc = (0.0, 0.0);
        self.screen = Screen::Viewer;
    }

    fn tx(&self) -> Option<UnboundedSender<Packet>> {
        self.tx_out.clone()
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        match self.screen {
            Screen::Login => self.login_ui(ctx),
            Screen::Viewer => self.viewer_ui(ctx),
        }
    }
}

impl App {
    fn login_ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.heading("RemoteFriend");
                ui.label("Bağlanılacak bilgisayarın adresi:");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Adres:");
                    ui.text_edit_singleline(&mut self.host_field);
                });
                ui.horizontal(|ui| {
                    ui.label("Şifre: ");
                    ui.add(egui::TextEdit::singleline(&mut self.pass_field).password(true));
                });
                ui.add_space(8.0);
                if ui.button("Bağlan").clicked() {
                    self.connect();
                }
                ui.add_space(4.0);
                ui.label(&self.login_msg);
                ui.add_space(16.0);
                ui.small("Örnek: 192.168.178.31:33200 (aynı ağ). İnternet için sinyal sunucusu sonraki fazda.");
            });
        });
    }

    fn viewer_ui(&mut self, ctx: &egui::Context) {
        let shared = self.shared.clone().unwrap();
        let tx = self.tx_out.clone().unwrap();
        let (img_opt, status, hw, hh, fps) = {
            let s = shared.lock().unwrap();
            (s.texture.clone(), s.status.clone(), s.host_w, s.host_h, s.fps)
        };

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Durum: {status} | Host: {hw}x{hh} | {fps:.1} fps"));
                if ui.button("Dosya Gönder").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        self.send_file(path);
                    }
                }
                ui.label("Tıkla/sürükle=mouse, klavye=tuş gönderir");
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some((img, _w, _h)) = img_opt {
                let tex = ui.ctx().load_texture("screen", img, egui::TextureOptions::LINEAR);
                // resmi panele sığdır
                let avail = ui.available_size();
                let img_size = tex.size_vec2();
                let scale = (avail.x / img_size.x).min(avail.y / img_size.y).max(0.1);
                let disp = img_size * scale;
                let (rect, resp) = ui.allocate_exact_size(disp, egui::Sense::click_and_drag());

                ui.painter().image(tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);

                // mouse -> host koordinatı
                let to_host = |pos: egui::Pos2| -> Option<(u32, u32)> {
                    if hw == 0 || hh == 0 { return None; }
                    let p = pos - rect.min;
                    if p.x < 0.0 || p.y < 0.0 || p.x > rect.width() || p.y > rect.height() {
                        return None;
                    }
                    Some(((p.x / rect.width() * hw as f32) as u32, (p.y / rect.height() * hh as f32) as u32))
                };

                if let Some(pos) = resp.hover_pos() {
                    if let Some((x, y)) = to_host(pos) {
                        // ayni koordinati spamleme: sadece degisince gonder
                        if self.last_mouse != Some((x, y)) {
                            self.last_mouse = Some((x, y));
                            let _ = tx.send(Packet::Input(InputEvent::MouseMove { x, y }));
                        }
                    }
                }
                if resp.clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        if let Some((x, y)) = to_host(p) {
                            let _ = tx.send(Packet::Input(InputEvent::MouseMove { x, y }));
                            let _ = tx.send(Packet::Input(InputEvent::MouseDown { button: MouseButton::Left }));
                            let _ = tx.send(Packet::Input(InputEvent::MouseUp { button: MouseButton::Left }));
                        }
                    }
                }
                if resp.secondary_clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        if let Some((x, y)) = to_host(p) {
                            let _ = tx.send(Packet::Input(InputEvent::MouseMove { x, y }));
                            let _ = tx.send(Packet::Input(InputEvent::MouseDown { button: MouseButton::Right }));
                            let _ = tx.send(Packet::Input(InputEvent::MouseUp { button: MouseButton::Right }));
                        }
                    }
                }

                // klavye: yazılabilir harfler Text ile, özel tuşlar Key ile (basma+bırakma)
                let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
                for ev in events {
                    match &ev {
                        egui::Event::Text(t) => {
                            for c in t.chars() {
                                if c.is_control() { continue; } // Enter/Tab vb Key yolundan gider
                                self.send_key(RemoteKey::Char(c), true);
                                self.send_key(RemoteKey::Char(c), false);
                            }
                        }
                        egui::Event::Key { key, pressed, repeat, .. } => {
                            if *repeat { continue; }
                            if let Some(rk) = egui_key_to_remote(*key) {
                                self.send_key(rk, *pressed);
                            }
                        }
                        egui::Event::MouseWheel { unit, delta, .. } => {
                            // imleç resim üstündeyken hosta kaydırma gönder
                            // (egui +y = içerik aşağı = protokol +dy = aşağı, birebir)
                            if resp.hover_pos().is_some() {
                                let mult = match unit {
                                    egui::MouseWheelUnit::Point => 1.0 / 50.0,
                                    egui::MouseWheelUnit::Line => 1.0,
                                    egui::MouseWheelUnit::Page => 10.0,
                                };
                                self.scroll_acc.0 += delta.x * mult;
                                self.scroll_acc.1 += delta.y * mult;
                                let dx = self.scroll_acc.0.trunc() as i32;
                                let dy = self.scroll_acc.1.trunc() as i32;
                                if dx != 0 || dy != 0 {
                                    self.scroll_acc.0 -= dx as f32;
                                    self.scroll_acc.1 -= dy as f32;
                                    let _ = tx.send(Packet::Input(InputEvent::Scroll { dx, dy }));
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // modifierlar (basılı takibi)
                let (mshift, mctrl, malt, mmeta) = ctx.input(|i| {
                    let m = &i.modifiers;
                    (m.shift, m.ctrl, m.alt, m.mac_cmd || (m.command && !m.ctrl))
                });
                self.sync_mod(RemoteKey::Shift, mshift);
                self.sync_mod(RemoteKey::Ctrl, mctrl);
                self.sync_mod(RemoteKey::Alt, malt);
                self.sync_mod(RemoteKey::Meta, mmeta);
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("Görüntü bekleniyor... host çalışıyor mu?");
                });
            }
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(66)); // ~15fps refresh
    }
}

impl App {
    fn send_key(&self, key: RemoteKey, down: bool) {
        if let Some(tx) = self.tx() {
            let _ = tx.send(Packet::Input(InputEvent::Key { key, down }));
        }
    }

    fn sync_mod(&mut self, key: RemoteKey, now: bool) {
        let idx = match key {
            RemoteKey::Shift => 0,
            RemoteKey::Ctrl => 1,
            RemoteKey::Alt => 2,
            RemoteKey::Meta => 3,
            _ => return,
        };
        if self.mods[idx] != now {
            self.mods[idx] = now;
            self.send_key(key, now);
        }
    }

    fn send_file(&self, path: std::path::PathBuf) {
        let Some(tx) = self.tx() else { return };
        std::thread::spawn(move || {
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("dosya okunamadı: {e}");
                    return;
                }
            };
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("dosya").to_string();
            let id = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
            let total = data.len() as u64;
            for (i, chunk) in data.chunks(60_000).enumerate() {
                let last = (i + 1) * 60_000 >= data.len();
                let fc = FileChunk {
                    transfer_id: id,
                    name: name.clone(),
                    offset: (i * 60_000) as u64,
                    total,
                    data: chunk.to_vec(),
                    last,
                };
                if tx.send(Packet::File(fc)).is_err() { break; }
            }
            tracing::info!("dosya gönderildi: {name} ({total} byte)");
        });
    }
}

/// Yazılabilir harfler Text olayından gelir; burada sadece özel tuşlar.
fn egui_key_to_remote(k: egui::Key) -> Option<RemoteKey> {
    Some(match k {
        egui::Key::Enter => RemoteKey::Enter,
        egui::Key::Tab => RemoteKey::Tab,
        egui::Key::Backspace => RemoteKey::Backspace,
        egui::Key::Escape => RemoteKey::Escape,
        egui::Key::Delete => RemoteKey::Delete,
        egui::Key::Insert => RemoteKey::Insert,
        egui::Key::Home => RemoteKey::Home,
        egui::Key::End => RemoteKey::End,
        egui::Key::PageUp => RemoteKey::PageUp,
        egui::Key::PageDown => RemoteKey::PageDown,
        egui::Key::ArrowUp => RemoteKey::Up,
        egui::Key::ArrowDown => RemoteKey::Down,
        egui::Key::ArrowLeft => RemoteKey::Left,
        egui::Key::ArrowRight => RemoteKey::Right,
        egui::Key::F1 => RemoteKey::F1,
        egui::Key::F2 => RemoteKey::F2,
        egui::Key::F3 => RemoteKey::F3,
        egui::Key::F4 => RemoteKey::F4,
        egui::Key::F5 => RemoteKey::F5,
        egui::Key::F6 => RemoteKey::F6,
        egui::Key::F7 => RemoteKey::F7,
        egui::Key::F8 => RemoteKey::F8,
        egui::Key::F9 => RemoteKey::F9,
        egui::Key::F10 => RemoteKey::F10,
        egui::Key::F11 => RemoteKey::F11,
        egui::Key::F12 => RemoteKey::F12,
        _ => return None,
    })
}

async fn read_packet(s: &mut tokio::net::TcpStream) -> Result<Packet> {
    let len = s.read_u32().await? as usize;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).await?;
    Ok(remote_friend_common::decode(&buf)?)
}

async fn read_packet_split(r: &mut tokio::net::tcp::OwnedReadHalf) -> Result<Packet> {
    let len = r.read_u32().await? as usize;
    if len > 20_000_000 {
        anyhow::bail!("paket çok büyük");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(remote_friend_common::decode(&buf)?)
}

async fn write_packet(s: &mut tokio::net::TcpStream, p: &Packet) -> Result<()> {
    let buf = remote_friend_common::encode(p)?;
    s.write_u32(buf.len() as u32).await?;
    s.write_all(&buf).await?;
    Ok(())
}
