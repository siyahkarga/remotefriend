//! Client: izleyen + kontrol eden taraf.
//! H.264/JPEG decode + eframe görüntü + mouse/klavye gönder + dosya gönder.

use anyhow::{Context, Result};
use remote_friend_common::{Handshake, InputEvent, MouseButton, Packet, FileChunk, RemoteKey, PROTOCOL_VERSION};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::io::Read as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{channel, Receiver, Sender};

mod history;
use history::RecentEntry;

/// LAN'da bulunan bilgisayar
#[derive(Clone)]
struct LanEntry {
    ip: String,
    name: String,
    port: u16,
    last_seen: Instant,
}

struct Shared {
    texture: Option<(egui::ColorImage, u32, u32)>, // son frame
    status: String,
    host_w: u32,
    host_h: u32,
    fps: f32,
}

fn main() -> Result<()> {
    remote_friend_common::tls::init_crypto();
    tracing_subscriber::fmt::init();
    let args: Vec<String> = std::env::args().collect();
    let def_host = args.get(1).cloned().unwrap_or(String::new());
    let def_pass = args.get(2).cloned().unwrap_or_default();
    let auto = args.len() > 1;

    // LAN discovery dinleyici (bulunanlar listeye düşer)
    let lan: Arc<Mutex<Vec<LanEntry>>> = Arc::new(Mutex::new(vec![]));
    let (disc_tx, disc_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || remote_friend_common::discovery::listen_loop(disc_tx));
    let lan_pump = lan.clone();
    std::thread::spawn(move || {
        while let Ok((ip, b)) = disc_rx.recv() {
            let mut v = lan_pump.lock().unwrap();
            if let Some(e) = v.iter_mut().find(|e| e.ip == ip && e.port == b.port) {
                e.name = b.name.clone();
                e.last_seen = Instant::now();
            } else {
                v.push(LanEntry { ip, name: b.name.clone(), port: b.port, last_seen: Instant::now() });
            }
        }
    });

    let mut app = App {
        screen: Screen::Login,
        host_field: def_host,
        pass_field: def_pass,
        server_field: remote_friend_common::identity::load_rv_config().server,
        login_msg: "Adres yaz ya da listeden seç.".into(),
        shared: None,
        tx_out: None,
        disconnect_tx: None,
        last_mouse: None,
        mods: [false; 4],
        scroll_acc: (0.0, 0.0),
        lan,
        recents: history::load_recents(),
        current_addr: String::new(),
        current_name: String::new(),
        thumb_saved: false,
        screen_tex: None,
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
    rx_out: Receiver<Packet>,
    disconnect: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let socket = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(host),
    )
    .await
    .map_err(|_| anyhow::anyhow!("bağlantı zaman aşımı (host kapalı ya da yanlış adres: {host})"))?
    .with_context(|| format!("bağlanamadı: {host}"))?;
    let (rd, wr) = socket.into_split();
    run_session(rd, wr, password, shared, rx_out, disconnect).await
}

/// İnternet (VPS) yolu: TLS/plain → Hello(ID+auth) → onay → aynı oturum.
/// Not: TLS VPS'te sonlanır; mevcut relay tasarımında VPS operatörü trafiği görebilir.
async fn net_loop_rv(
    server: &str,
    fp: Option<String>,
    id: &str,
    password: &str,
    shared: Arc<Mutex<Shared>>,
    rx_out: Receiver<Packet>,
    disconnect: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    use remote_friend_common::io::{read_rv, write_rv};
    use remote_friend_common::RvMsg;
    shared.lock().unwrap().status = format!("sunucuya bağlanılıyor ({server})...").into();
    let (rd, wr, mode): (BoxRd, BoxWr, &'static str) = if let Some(fp) = fp {
        let s = remote_friend_common::tls::tls_connect(server, server.split(':').next().unwrap_or("rv"), Some(fp)).await?;
        let (r, w) = tokio::io::split(s);
        (Box::new(r), Box::new(w), "TLS")
    } else {
        // Parmak izi yoksa sistem kök sertifikalarıyla dene (domain + LE kuruluysa sorunsuz).
        match remote_friend_common::tls::tls_connect(server, server.split(':').next().unwrap_or("rv"), None).await {
            Ok(s) => {
                let (r, w) = tokio::io::split(s);
                (Box::new(r), Box::new(w), "TLS-CA")
            }
            Err(e) => {
                if std::env::var("RF_PLAIN_OK").map(|v| v == "1").unwrap_or(false) {
                    let s = tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        tokio::net::TcpStream::connect(server),
                    )
                    .await
                    .context("rendezvous TCP bağlantı zaman aşımı")??;
                    let (r, w) = s.into_split();
                    (Box::new(r), Box::new(w), "DÜZ")
                } else {
                    anyhow::bail!("TLS doğrulanamadı ({e:#}); IP + self-signed için sunucu ayarında FP gir ya da test için RF_PLAIN_OK=1");
                }
            }
        }
    };
    let _ = mode;
    let mut rd = rd;
    let mut wr = wr;
    write_rv(&mut wr, &RvMsg::Hello { id: id.to_string(), auth: Some(password.to_string()) }).await?;
    loop {
        match read_rv(&mut rd).await? {
            RvMsg::Accepted => break,
            RvMsg::Rejected(m) => {
                shared.lock().unwrap().status = format!("reddedildi: {m}");
                anyhow::bail!("reject: {m}");
            }
            RvMsg::WaitApproval => {
                shared.lock().unwrap().status = "host onayı bekleniyor...".into();
            }
            _ => {}
        }
    }
    run_session(rd, wr, password, shared, rx_out, disconnect).await
}

type BoxRd = Box<dyn tokio::io::AsyncRead + Unpin + Send>;
type BoxWr = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;

/// El sıkışma + onay + video/input döngüleri (her transport için ortak).
async fn run_session<R, W>(
    mut rd: R,
    mut wr: W,
    password: &str,
    shared: Arc<Mutex<Shared>>,
    mut rx_out: Receiver<Packet>,
    mut disconnect: tokio::sync::watch::Receiver<bool>,
) -> Result<()>
where
    R: tokio::io::AsyncReadExt + Unpin + Send + 'static,
    W: tokio::io::AsyncWriteExt + Unpin + Send + 'static,
{
    let hs = Packet::Handshake(Handshake {
        version: PROTOCOL_VERSION,
        password: password.to_string(),
        want_video: true,
        want_input: true,
    });
    write_packet(&mut wr, &hs).await?;
    // Accept / Reject / WaitingForApproval döngüsü
    loop {
        let resp = read_packet(&mut rd).await?;
        match resp {
            Packet::Accept => {
                shared.lock().unwrap().status = "bağlı".into();
                break;
            }
            Packet::Reject(m) => {
                shared.lock().unwrap().status = format!("reddedildi: {m}");
                anyhow::bail!("reject: {m}");
            }
            Packet::WaitingForApproval => {
                shared.lock().unwrap().status =
                    "host onayı bekleniyor... (host ekranında E'ye basılmalı)".into();
            }
            _ => anyhow::bail!("beklenmeyen yanıt"),
        }
    }

    // gönderici (async recv: thread'i kilitlemez)
    // Not: ham u32 yazımı (framing) run_session'a özel; write_packet'e dokunmaz.
    let wr = Arc::new(tokio::sync::Mutex::new(wr));
    let wr2 = wr.clone();
    tokio::spawn(async move {
        while let Some(p) = rx_out.recv().await {
            let mut g = wr2.lock().await;
            if remote_friend_common::io::write_packet(&mut *g, &p).await.is_err() {
                break;
            }
        }
    });

    // alıcı: video (bağlantı başına taze decoder: P-frame'ler için şart)
    let mut last = Instant::now();
    let mut n = 0u32;
    let mut total = 0u64;
    let mut h264 = H264Dec::new()?;
    let mut first_frame = false;
    loop {
        tokio::select! {
            _ = disconnect.changed() => {
                tracing::info!("kullanıcı bağlantıyı kesti");
                shared.lock().unwrap().status = "bağlantı kesildi".into();
                break;
            }
            res = read_packet_split(&mut rd) => {
                let pkt = match res {
                    Ok(p) => p,
                    Err(e) => {
                        shared.lock().unwrap().status = format!("bağlantı koptu: {e:#}");
                        break;
                    }
                };
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
            // Sadece en yeni kare tutulur. UI henüz tüketmediyse eski kare burada düşer;
            // böylece görüntü kuyruğu büyüyüp saniyeler geriden gelmez.
            s.texture = Some((img, f.width, f.height));
            let el = last.elapsed().as_secs_f32();
            if el >= 1.0 {
                s.fps = n as f32 / el;
                n = 0;
                last = Instant::now();
            }
            if !first_frame {
                first_frame = true;
                // Aynı Mutex yeniden kilitlenmemeli; aksi halde ilk karede deadlock olur.
                s.status = "bağlı".into();
            }
            drop(s);
            if total % 50 == 0 {
                tracing::info!("video akiyor: toplam {total} frame");
            }
        }
            }
    };
    }
    Ok(())
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
    server_field: String,
    login_msg: String,
    // viewer (connect sonrası)
    shared: Option<Arc<Mutex<Shared>>>,
    tx_out: Option<Sender<Packet>>,
    disconnect_tx: Option<tokio::sync::watch::Sender<bool>>,
    last_mouse: Option<(u32, u32)>,
    mods: [bool; 4], // shift, ctrl, alt, meta (basılı mı)
    scroll_acc: (f32, f32),
    // discovery + recents (arka plan thread'lerinden beslenir)
    lan: Arc<Mutex<Vec<LanEntry>>>,
    recents: Vec<RecentEntry>,
    // aktif bağlantı bilgisi (recent kaydı için)
    current_addr: String,
    current_name: String,
    thumb_saved: bool,
    // tek GPU texture (sızıntıyı önler)
    screen_tex: Option<egui::TextureHandle>,
}

impl App {
    /// Bağlan düğmesi / otomatik bağlanma: net thread'i başlat, görüntüye geç.
    /// 9 hane rakam = internet ID'si (VPS üzerinden), yoksa LAN adresi.
    fn connect(&mut self) {
        let host = self.host_field.trim().to_string();
        let password = self.pass_field.clone();
        if host.is_empty() {
            self.login_msg = "Adres boş olamaz.".into();
            return;
        }
        // ID mi adres mi?
        let digits: String = host.chars().filter(|c| c.is_ascii_digit()).collect();
        let via_rv = digits.len() == 9 && !host.contains(':') && !host.contains('.');
        // rendezvous sunucusu: alan > env > config
        let server = if via_rv {
            let f = self.server_field.trim().to_string();
            if !f.is_empty() {
                Some(f)
            } else {
                std::env::var("RF_RV_SERVER").ok().filter(|s| !s.is_empty()).or_else(|| {
                    let c = remote_friend_common::identity::load_rv_config();
                    if c.server.is_empty() { None } else { Some(c.server) }
                })
            }
        } else {
            None
        };
        if via_rv && server.is_none() {
            self.login_msg = "İnternet için sunucu adresi gir (Sunucu satırı) ya da RF_RV_SERVER ver.".into();
            return;
        }
        // sunucuyu hatırla
        if let Some(ref s) = server {
            let mut cfg = remote_friend_common::identity::load_rv_config();
            if cfg.server != *s {
                cfg.server = s.clone();
                remote_friend_common::identity::save_rv_config(&cfg);
            }
            self.server_field = s.clone();
        }
        tracing::info!("bağlanılıyor: {host}");
        let shared = Arc::new(Mutex::new(Shared {
            texture: None,
            status: "bağlanıyor...".into(),
            host_w: 0,
            host_h: 0,
            fps: 0.0,
        }));
        let (tx_out, rx_out) = channel::<Packet>(256);
        let (dc_tx, dc_rx) = tokio::sync::watch::channel(false);
        let sh = shared.clone();
        let host_c = host.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let r = if via_rv {
                    let fp = std::env::var("RF_RV_FP").ok().filter(|s| !s.is_empty()).or_else(|| {
                        let c = remote_friend_common::identity::load_rv_config();
                        if c.fp.is_empty() { None } else { Some(c.fp) }
                    });
                    net_loop_rv(&server.unwrap(), fp, &digits, &password, sh.clone(), rx_out, dc_rx).await
                } else {
                    net_loop(&host_c, &password, sh.clone(), rx_out, dc_rx).await
                };
                if let Err(e) = r {
                    sh.lock().unwrap().status = format!("hata: {e:#}");
                    tracing::warn!("net kapandı: {e:#}");
                }
            });
        });
        // recent'e yaz (isim LAN listesinden biliniyorsa)
        let known_name = self
            .lan
            .lock()
            .unwrap()
            .iter()
            .find(|e| format!("{}:{}", e.ip, e.port) == host)
            .map(|e| e.name.clone())
            .unwrap_or_default();
        history::touch_recent(&mut self.recents, &host, &known_name);
        self.current_addr = host;
        self.current_name = known_name;
        self.thumb_saved = false;
        self.screen_tex = None;
        self.shared = Some(shared);
        self.tx_out = Some(tx_out);
        self.disconnect_tx = Some(dc_tx);
        self.last_mouse = None;
        self.mods = [false; 4];
        self.scroll_acc = (0.0, 0.0);
        self.screen = Screen::Viewer;
    }

    fn tx(&self) -> Option<Sender<Packet>> {
        self.tx_out.clone()
    }

    /// Bağlantıyı kes ve ana ekrana dön.
    fn disconnect(&mut self) {
        if let Some(dc) = self.disconnect_tx.take() {
            let _ = dc.send(true);
        }
        self.tx_out = None;
        self.shared = None;
        self.screen_tex = None;
        self.login_msg = "Bağlantı kesildi.".into();
        self.screen = Screen::Login;
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
        // eski LAN kayıtlarını temizle (>8 sn sessiz)
        self.lan.lock().unwrap().retain(|e| e.last_seen.elapsed().as_secs() < 8);

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(10.0);
            ui.heading("RemoteFriend");
            ui.small("Aynı ağdaki bilgisayarlar otomatik bulunur. Bağlantı için host onayı şarttır.");
            ui.add_space(8.0);

            // adres satırı (9 hane = internet ID'si, yoksa LAN adresi)
            ui.horizontal(|ui| {
                ui.label("Adres/ID:");
                let addr_resp = ui.add(
                    egui::TextEdit::singleline(&mut self.host_field)
                        .hint_text("123 456 789 ya da 192.168.1.20:33200")
                        .desired_width(240.0),
                );
                ui.label("Şifre:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.pass_field)
                        .password(true)
                        .desired_width(90.0),
                );
                if ui.button("➡ Bağlan").clicked() || (addr_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    self.connect();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Sunucu:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_field)
                        .hint_text("VPS_IP:33202 (sadece internet ID için)")
                        .desired_width(240.0),
                );
            });
            if !self.login_msg.is_empty() {
                ui.small(&self.login_msg);
            }
            ui.add_space(8.0);
            ui.separator();

            // LAN'dakiler
            ui.add_space(4.0);
            ui.heading("Ağdaki Bilgisayarlar");
            {
                let lan = self.lan.lock().unwrap().clone();
                if lan.is_empty() {
                    ui.small("Aranıyor... (host tarafında remote-friend-host çalışmalı)");
                }
                for e in lan {
                    ui.horizontal(|ui| {
                        ui.label(format!("🖥 {}  ({}:{})", e.name, e.ip, e.port));
                        if ui.small_button("Bağlan").clicked() {
                            self.host_field = format!("{}:{}", e.ip, e.port);
                            self.connect();
                        }
                    });
                }
            }
            ui.add_space(8.0);
            ui.separator();

            // son bağlantılar
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("Son Bağlantılar");
                if ui.small_button("Temizle").clicked() {
                    self.recents.clear();
                    history::save_recents(&self.recents);
                }
            });
            if self.recents.is_empty() {
                ui.small("Henüz bağlantı yok.");
            }
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                // borrow sorunu için indexle dön
                let n = self.recents.len();
                for i in 0..n {
                    let (addr, name, fav, thumb) = {
                        let r = &self.recents[i];
                        (r.addr.clone(), r.name.clone(), r.fav, r.thumb.clone())
                    };
                    ui.horizontal(|ui| {
                        // thumbnail
                        if let Some(t) = thumb.as_ref().and_then(|f| {
                            load_thumb_texture(ui.ctx(), f)
                        }) {
                            ui.image((t.id(), egui::vec2(96.0, 54.0)));
                        }
                        ui.vertical(|ui| {
                            ui.label(format!("{}{}", if fav { "★ " } else { "" }, name));
                            ui.small(&addr);
                        });
                        if ui.small_button(if fav { "★" } else { "☆" }).clicked() {
                            self.recents[i].fav = !fav;
                            history::save_recents(&self.recents);
                        }
                        if ui.small_button("Bağlan").clicked() {
                            self.host_field = addr.clone();
                            self.connect();
                            return;
                        }
                    });
                    ui.separator();
                }
            });
        });
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}

fn load_thumb_texture(ctx: &egui::Context, fname: &str) -> Option<egui::TextureHandle> {
    let data = std::fs::read(history::thumb_path(fname)).ok()?;
    let img = image::load_from_memory(&data).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let ci = egui::ColorImage::from_rgba_unmultiplied([w, h], &img.into_raw());
    Some(ctx.load_texture(format!("thumb_{fname}"), ci, egui::TextureOptions::LINEAR))
}

impl App {
    fn viewer_ui(&mut self, ctx: &egui::Context) {
        let shared = self.shared.clone().unwrap();
        let tx = self.tx_out.clone().unwrap();
        let (new_frame, status, hw, hh, fps) = {
            let mut s = shared.lock().unwrap();
            // Kareyi kopyalamak yerine sahipliğini UI'ya aktar. Bir 1080p RGBA kare yaklaşık
            // 8 MiB'dir; clone etmek hem CPU hem bellek bant genişliğini gereksiz tüketir.
            (s.texture.take(), s.status.clone(), s.host_w, s.host_h, s.fps)
        };

        // Yalnızca gerçekten yeni kare geldiğinde GPU texture'ını güncelle.
        if let Some((img, _w, _h)) = new_frame {
            if !self.thumb_saved {
                if let Some(fname) = history::save_thumb(&self.current_addr, &img) {
                    if let Some(e) = self.recents.iter_mut().find(|e| e.addr == self.current_addr) {
                        e.thumb = Some(fname);
                        history::save_recents(&self.recents);
                    }
                }
                self.thumb_saved = true;
            }

            let [iw, ih] = img.size;
            let same = self.screen_tex.as_ref().is_some_and(|t| t.size() == [iw, ih]);
            if same {
                if let Some(tex) = self.screen_tex.as_mut() {
                    tex.set(img, egui::TextureOptions::LINEAR);
                }
            } else {
                self.screen_tex = Some(ctx.load_texture(
                    "screen",
                    img,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("⛔ Kes").clicked() {
                    self.disconnect();
                    return;
                }
                ui.label(format!("Durum: {status} | Host: {hw}x{hh} | {fps:.1} fps"));
                if ui.button("Dosya Gönder").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        self.send_file(path);
                    }
                }
            });
        });

        let failed = status.starts_with("hata")
            || status.starts_with("reddedildi")
            || status.starts_with("bağlantı koptu")
            || status.starts_with("bağlantı kesildi");

        // TextureHandle klonu yalnızca küçük bir referans klonudur; piksel verisini kopyalamaz.
        // Böylece closure içinde self'in input alanlarını güvenle değiştirebiliriz.
        let display_tex = self.screen_tex.clone();
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(tex) = display_tex.as_ref() {
                let avail = ui.available_size();
                let img_size = tex.size_vec2();
                let scale = (avail.x / img_size.x).min(avail.y / img_size.y).max(0.1);
                let disp = img_size * scale;
                let (rect, resp) = ui.allocate_exact_size(disp, egui::Sense::click_and_drag());

                ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );

                let to_host = |pos: egui::Pos2| -> Option<(u32, u32)> {
                    if hw == 0 || hh == 0 { return None; }
                    let p = pos - rect.min;
                    if p.x < 0.0 || p.y < 0.0 || p.x > rect.width() || p.y > rect.height() {
                        return None;
                    }
                    Some((
                        (p.x / rect.width() * hw as f32) as u32,
                        (p.y / rect.height() * hh as f32) as u32,
                    ))
                };

                if let Some(pos) = resp.hover_pos() {
                    if let Some((x, y)) = to_host(pos) {
                        if self.last_mouse != Some((x, y)) {
                            self.last_mouse = Some((x, y));
                            let _ = tx.try_send(Packet::Input(InputEvent::MouseMove { x, y }));
                        }
                    }
                }
                if resp.clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        if let Some((x, y)) = to_host(p) {
                            let _ = tx.try_send(Packet::Input(InputEvent::MouseMove { x, y }));
                            let _ = tx.try_send(Packet::Input(InputEvent::MouseDown { button: MouseButton::Left }));
                            let _ = tx.try_send(Packet::Input(InputEvent::MouseUp { button: MouseButton::Left }));
                        }
                    }
                }
                if resp.secondary_clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        if let Some((x, y)) = to_host(p) {
                            let _ = tx.try_send(Packet::Input(InputEvent::MouseMove { x, y }));
                            let _ = tx.try_send(Packet::Input(InputEvent::MouseDown { button: MouseButton::Right }));
                            let _ = tx.try_send(Packet::Input(InputEvent::MouseUp { button: MouseButton::Right }));
                        }
                    }
                }

                let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
                for ev in events {
                    match &ev {
                        egui::Event::Text(t) => {
                            for c in t.chars() {
                                if c.is_control() { continue; }
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
                                    let _ = tx.try_send(Packet::Input(InputEvent::Scroll { dx, dy }));
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let (mshift, mctrl, malt, mmeta) = ctx.input(|i| {
                    let m = &i.modifiers;
                    (m.shift, m.ctrl, m.alt, m.mac_cmd || (m.command && !m.ctrl))
                });
                self.sync_mod(RemoteKey::Shift, mshift);
                self.sync_mod(RemoteKey::Ctrl, mctrl);
                self.sync_mod(RemoteKey::Alt, malt);
                self.sync_mod(RemoteKey::Meta, mmeta);
            } else if failed {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("Bağlantı kurulamadı");
                        ui.label(&status);
                        ui.add_space(8.0);
                        if ui.button("⟵ Geri Dön").clicked() {
                            self.disconnect();
                        }
                    });
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("Görüntü bekleniyor... (host onayı gerekli olabilir)");
                });
            }
        });
        // Input gecikmesini düşürür; texture yalnızca yeni kare olduğunda güncellenir.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

impl App {
    fn send_key(&self, key: RemoteKey, down: bool) {
        if let Some(tx) = self.tx() {
            let _ = tx.try_send(Packet::Input(InputEvent::Key { key, down }));
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
            const CHUNK: usize = 64 * 1024;
            const DEFAULT_MAX: u64 = 512 * 1024 * 1024;
            let max = std::env::var("RF_MAX_FILE_BYTES")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_MAX);
            let total = match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => m.len(),
                Ok(_) => {
                    tracing::warn!("yalnızca normal dosya gönderilebilir");
                    return;
                }
                Err(e) => {
                    tracing::warn!("dosya bilgisi okunamadı: {e}");
                    return;
                }
            };
            if total == 0 || total > max {
                tracing::warn!("dosya boyutu sınır dışında: {total} (üst sınır {max})");
                return;
            }
            let mut file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("dosya açılamadı: {e}");
                    return;
                }
            };
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("dosya").to_string();
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| (d.as_nanos() as u64).max(1))
                .unwrap_or(1);
            let mut offset = 0u64;
            let mut buf = vec![0u8; CHUNK];
            while offset < total {
                let want = ((total - offset) as usize).min(CHUNK);
                let n = match file.read(&mut buf[..want]) {
                    Ok(0) => {
                        tracing::warn!("dosya beklenenden erken bitti: {name}");
                        return;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!("dosya okunamadı: {e}");
                        return;
                    }
                };
                let end = offset + n as u64;
                let fc = FileChunk {
                    transfer_id: id,
                    name: name.clone(),
                    offset,
                    total,
                    data: buf[..n].to_vec(),
                    last: end == total,
                };
                if tx.blocking_send(Packet::File(fc)).is_err() { return; }
                offset = end;
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

// Framed paket IO ortak modülden. read_packet_split eski adı korur.
use remote_friend_common::io::{read_packet, write_packet};
async fn read_packet_split<R>(r: &mut R) -> anyhow::Result<Packet>
where
    R: tokio::io::AsyncReadExt + Unpin,
{
    read_packet(r).await
}
