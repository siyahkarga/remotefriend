//! Client: izleyen + kontrol eden taraf.
//! JPEG decode + eframe görüntü + mouse/klavye gönder + dosya gönder.

use anyhow::{Context, Result};
use remote_friend_common::{Handshake, InputEvent, MouseButton, Packet, FileChunk, PROTOCOL_VERSION};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    let host = args.get(1).cloned().unwrap_or("127.0.0.1:33200".into());
    let password = args.get(2).cloned().unwrap_or("1234".into());
    tracing::info!("bağlanılıyor: {host}");

    let shared = Arc::new(Mutex::new(Shared {
        texture: None,
        status: "bağlanıyor...".into(),
        host_w: 0,
        host_h: 0,
        fps: 0.0,
    }));

    // GUI -> net: input/file paketleri
    let (tx_out, rx_out) = mpsc::channel::<Packet>();

    // net thread (tokio)
    let sh = shared.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            if let Err(e) = net_loop(&host, &password, sh, rx_out).await {
                tracing::warn!("net kapandı: {e:#}");
            }
        });
    });

    let app = App { shared, tx_out };
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native("RemoteFriend Client", opts, Box::new(|_| Ok(Box::new(app))))
        .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

async fn net_loop(
    host: &str,
    password: &str,
    shared: Arc<Mutex<Shared>>,
    rx_out: mpsc::Receiver<Packet>,
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

    // gönderici
    tokio::spawn(async move {
        while let Ok(p) = rx_out.recv() {
            let buf = match remote_friend_common::encode(&p) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if wr.write_u32(buf.len() as u32).await.is_err() { break; }
            if wr.write_all(&buf).await.is_err() { break; }
        }
    });

    // alıcı: video
    let mut last = SystemTime::now();
    let mut n = 0u32;
    loop {
        let pkt = read_packet_split(&mut rd).await?;
        if let Packet::Video(f) = pkt {
            n += 1;
            let img = decode_jpeg(&f.data)?;
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

struct App {
    shared: Arc<Mutex<Shared>>,
    tx_out: mpsc::Sender<Packet>,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (img_opt, status, hw, hh, fps) = {
            let s = self.shared.lock().unwrap();
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
                        // hareketi sürekli gönder (Wayland/X11 fark etmez, host uygular)
                        let _ = self.tx_out.send(Packet::Input(InputEvent::MouseMove { x, y }));
                    }
                }
                if resp.clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        if let Some((x, y)) = to_host(p) {
                            let _ = self.tx_out.send(Packet::Input(InputEvent::MouseMove { x, y }));
                            let _ = self.tx_out.send(Packet::Input(InputEvent::MouseDown { button: MouseButton::Left }));
                            let _ = self.tx_out.send(Packet::Input(InputEvent::MouseUp { button: MouseButton::Left }));
                        }
                    }
                }
                if resp.secondary_clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        if let Some((x, y)) = to_host(p) {
                            let _ = self.tx_out.send(Packet::Input(InputEvent::MouseMove { x, y }));
                            let _ = self.tx_out.send(Packet::Input(InputEvent::MouseDown { button: MouseButton::Right }));
                            let _ = self.tx_out.send(Packet::Input(InputEvent::MouseUp { button: MouseButton::Right }));
                        }
                    }
                }

                // klavye
                let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
                for ev in events {
                    match &ev {
                        egui::Event::Text(t) => {
                            for c in t.chars() {
                                let _ = self.tx_out.send(Packet::Input(InputEvent::KeyDown { code: c as u32 }));
                                let _ = self.tx_out.send(Packet::Input(InputEvent::KeyUp { code: c as u32 }));
                            }
                        }
                        egui::Event::Key { key, pressed, .. } => {
                            if *pressed {
                                if let Some(c) = key_to_char(*key) {
                                    let _ = self.tx_out.send(Packet::Input(InputEvent::KeyDown { code: c as u32 }));
                                    let _ = self.tx_out.send(Packet::Input(InputEvent::KeyUp { code: c as u32 }));
                                }
                            }
                        }
                        _ => {}
                    }
                }
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
    fn send_file(&self, path: std::path::PathBuf) {
        let tx = self.tx_out.clone();
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

fn key_to_char(k: egui::Key) -> Option<char> {
    match k {
        egui::Key::Enter => Some('\n'),
        egui::Key::Space => Some(' '),
        egui::Key::Backspace => Some('\x08'),
        egui::Key::Tab => Some('\t'),
        egui::Key::Escape => Some('\x1b'),
        _ => None,
    }
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
