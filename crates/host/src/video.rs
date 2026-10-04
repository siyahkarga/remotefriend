//! Screen capture + encoding pipelines. Each pipeline (H.264, JPEG) runs once and
//! broadcasts its frames to all viewers; a slow viewer does not pile up frames.
//!
//! - Wayland: PipeWire delivers frames only when the screen changes (damage-based).
//! - X11/Windows/macOS: xcap captures every round; unchanged frames are not encoded.
//! - Keyframes (IDR) only when needed: new viewer, frame loss, explicit request.
//!   On a static screen the last frame is re-encoded, so a new viewer never sees black.
//! - On network congestion the bitrate drops automatically and slowly recovers afterwards.

use crate::convert::{self, RawFrame};
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

#[derive(Debug)]
pub(crate) struct EncodedFrame {
    pub seq: u64,
    /// Size of the sent (downscaled) frame.
    pub w: u32,
    pub h: u32,
    /// Capture size: client coordinates are mapped to this.
    pub src_w: u32,
    pub src_h: u32,
    pub key: bool,
    pub jpeg: bool,
    pub data: Vec<u8>,
}

impl EncodedFrame {
    /// Map a point from sent-frame space to capture space.
    pub(crate) fn to_capture(&self, x: u32, y: u32) -> (u32, u32) {
        if self.w == 0 || self.h == 0 {
            return (x, y);
        }
        let cx = (x.min(self.w - 1) as u64 * self.src_w as u64 / self.w as u64) as u32;
        let cy = (y.min(self.h - 1) as u64 * self.src_h as u64 / self.h as u64) as u32;
        (cx, cy)
    }
}

// ---- quality presets ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Preset {
    Fast = 0,
    Balanced = 1,
    Sharp = 2,
}

impl Preset {
    pub(crate) fn from_name(s: &str) -> Option<Self> {
        match s {
            "fast" | "low" => Some(Self::Fast),
            "balanced" | "medium" => Some(Self::Balanced),
            "sharp" | "high" => Some(Self::Sharp),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Sharp => "sharp",
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Fast,
            2 => Self::Sharp,
            _ => Self::Balanced,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Profile {
    pub max_w: u32,
    pub bitrate: u32,
    pub fps: u32,
    pub jpeg_w: u32,
    pub jpeg_q: u8,
    pub jpeg_fps: u32,
}

fn env_u32(name: &str, range: std::ops::RangeInclusive<u32>) -> Option<u32> {
    std::env::var(name).ok().and_then(|s| s.trim().parse().ok()).filter(|v| range.contains(v))
}

/// Environment variables tune the "Balanced" profile; the others are derived from it.
pub(crate) fn profile(p: Preset) -> Profile {
    let fps = env_u32("RF_FPS", 5..=60).unwrap_or(30);
    let max_w = env_u32("RF_MAX_WIDTH", 640..=7680).unwrap_or(1920) & !1;
    let bitrate = env_u32("RF_BITRATE_BPS", 500_000..=50_000_000).unwrap_or(8_000_000);
    let jpeg_q = env_u32("RF_JPEG_Q", 30..=95).unwrap_or(70) as u8;
    let jpeg_fps = env_u32("RF_JPEG_FPS", 2..=20).unwrap_or(10);
    match p {
        Preset::Fast => Profile {
            max_w: max_w.min(1280),
            bitrate: (bitrate / 3).max(1_500_000),
            fps,
            jpeg_w: 960,
            jpeg_q: jpeg_q.saturating_sub(15).max(35),
            jpeg_fps: jpeg_fps.min(8),
        },
        Preset::Balanced => Profile { max_w, bitrate, fps, jpeg_w: max_w.min(1280), jpeg_q, jpeg_fps },
        Preset::Sharp => Profile {
            max_w: 3840,
            bitrate: (bitrate * 2).max(16_000_000),
            fps,
            jpeg_w: max_w.min(1920),
            jpeg_q: (jpeg_q + 12).min(90),
            jpeg_fps,
        },
    }
}

static PRESET: AtomicU8 = AtomicU8::new(255);

pub(crate) fn preset() -> Preset {
    let v = PRESET.load(Ordering::Relaxed);
    if v != 255 {
        return Preset::from_u8(v);
    }
    let p = std::env::var("RF_QUALITY")
        .ok()
        .and_then(|s| Preset::from_name(s.trim()))
        .unwrap_or(Preset::Balanced);
    PRESET.store(p as u8, Ordering::Relaxed);
    p
}

pub(crate) fn set_preset(p: Preset) {
    if preset() != p {
        tracing::info!("video quality: {}", p.name());
        PRESET.store(p as u8, Ordering::Relaxed);
    }
}

// ---- pipeline control ----

static FORCE_KEY: AtomicBool = AtomicBool::new(false);
static FORCE_JPEG: AtomicBool = AtomicBool::new(false);
static CONGESTION: AtomicU32 = AtomicU32::new(0);

/// Make the next H.264 frame a keyframe (new viewer / lost frame).
pub(crate) fn request_keyframe() {
    FORCE_KEY.store(true, Ordering::Relaxed);
}

/// A JPEG viewer missed a frame: send a fresh frame even if the screen is static.
pub(crate) fn request_jpeg_refresh() {
    FORCE_JPEG.store(true, Ordering::Relaxed);
}

/// A viewer could not keep up: lower the bitrate.
pub(crate) fn report_congestion() {
    CONGESTION.fetch_add(1, Ordering::Relaxed);
}

static H264_TX: OnceLock<broadcast::Sender<Arc<EncodedFrame>>> = OnceLock::new();
static JPEG_TX: OnceLock<broadcast::Sender<Arc<EncodedFrame>>> = OnceLock::new();

pub(crate) fn h264() -> &'static broadcast::Sender<Arc<EncodedFrame>> {
    H264_TX.get_or_init(|| {
        let (tx, _) = broadcast::channel(4);
        let worker = tx.clone();
        std::thread::Builder::new()
            .name("rf-h264".into())
            .spawn(move || h264_worker(worker))
            .expect("failed to start h264 thread");
        tx
    })
}

pub(crate) fn jpeg() -> &'static broadcast::Sender<Arc<EncodedFrame>> {
    JPEG_TX.get_or_init(|| {
        let (tx, _) = broadcast::channel(2);
        let worker = tx.clone();
        std::thread::Builder::new()
            .name("rf-jpeg".into())
            .spawn(move || jpeg_worker(worker))
            .expect("failed to start jpeg thread");
        tx
    })
}

// ---- capture ----

fn test_pattern() -> bool {
    std::env::var("RF_TEST_PATTERN").map(|v| v == "1").unwrap_or(false)
}

/// Wayland: the screen comes from the desktop portal (PipeWire), not from screenshots.
#[cfg(target_os = "linux")]
fn portal_capture() -> bool {
    crate::wayland::is_wayland() && std::env::var("RF_WAYLAND_XCAP").map(|v| v != "1").unwrap_or(true)
}

/// Is the mouse pointer drawn into the picture? The portal embeds it; the screenshot APIs used
/// on X11, Windows and macOS don't, so clients then draw a pointer of their own.
pub(crate) fn cursor_in_video() -> bool {
    #[cfg(target_os = "linux")]
    if portal_capture() && !test_pattern() {
        return crate::wayland::cursor_embedded();
    }
    false
}

// ---- which screen is shared ----

/// A screen that can be shared.
#[derive(Clone, Debug)]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// Chosen monitor (index into `monitors()`); usize::MAX = the primary one.
static MONITOR_SEL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);
/// Bumped on every change, so capture threads pick up the new monitor.
static MONITOR_GEN: AtomicU64 = AtomicU64::new(0);

fn monitor_watch() -> &'static tokio::sync::watch::Sender<u64> {
    static TX: std::sync::OnceLock<tokio::sync::watch::Sender<u64>> = std::sync::OnceLock::new();
    TX.get_or_init(|| tokio::sync::watch::channel(0).0)
}

/// Notified whenever the shared screen changes.
pub(crate) fn monitor_changes() -> tokio::sync::watch::Receiver<u64> {
    monitor_watch().subscribe()
}

fn xcap_monitors() -> Vec<MonitorInfo> {
    xcap::Monitor::all()
        .map(|all| {
            all.iter()
                .enumerate()
                .map(|(index, m)| MonitorInfo {
                    index,
                    name: m.name().unwrap_or_else(|_| format!("Screen {}", index + 1)),
                    width: m.width().unwrap_or(0),
                    height: m.height().unwrap_or(0),
                    primary: m.is_primary().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Screens that can be shared right now.
pub fn monitors() -> Vec<MonitorInfo> {
    if test_pattern() {
        return vec![MonitorInfo { index: 0, name: "Test pattern".into(), width: 1280, height: 720, primary: true }];
    }
    #[cfg(target_os = "linux")]
    if portal_capture() {
        return crate::wayland::screens()
            .into_iter()
            .enumerate()
            .map(|(index, (w, h))| MonitorInfo {
                index,
                name: format!("Screen {}", index + 1),
                width: w,
                height: h,
                primary: index == 0,
            })
            .collect();
    }
    xcap_monitors()
}

/// Index of the shared screen in `monitors()`.
pub fn current_monitor() -> usize {
    #[cfg(target_os = "linux")]
    if portal_capture() {
        return crate::wayland::active_screen();
    }
    match MONITOR_SEL.load(Ordering::Relaxed) {
        usize::MAX => xcap_monitors().iter().position(|m| m.primary).unwrap_or(0),
        i => i,
    }
}

/// Share another screen (all viewers see the change).
pub fn select_monitor(index: usize) -> bool {
    let ok = {
        #[cfg(target_os = "linux")]
        {
            if portal_capture() {
                crate::wayland::select_screen(index)
            } else {
                index < xcap_monitors().len()
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            index < xcap_monitors().len()
        }
    };
    if ok {
        MONITOR_SEL.store(index, Ordering::Relaxed);
        MONITOR_GEN.fetch_add(1, Ordering::Relaxed);
        monitor_watch().send_modify(|g| *g += 1);
        request_keyframe();
        request_jpeg_refresh();
    }
    ok
}

/// The monitor list and current choice, as sent to viewers.
pub(crate) fn monitors_json() -> serde_json::Value {
    let list: Vec<serde_json::Value> = monitors()
        .iter()
        .map(|m| serde_json::json!({"i": m.index, "name": m.name, "w": m.width, "h": m.height, "primary": m.primary}))
        .collect();
    serde_json::json!({"t": "monitors", "list": list, "current": current_monitor()})
}

/// Most recent captured frame (for the preview in the app).
static LAST_FRAME: Mutex<Option<Arc<RawFrame>>> = Mutex::new(None);

/// Small RGBA preview (at most `max_w` wide) of what viewers see, while a session runs.
pub fn preview(max_w: u32) -> Option<(u32, u32, Vec<u8>)> {
    let f = LAST_FRAME.lock().ok()?.clone()?;
    if f.w == 0 || f.h == 0 {
        return None;
    }
    let step = f.w.div_ceil(max_w.max(1)).max(1);
    let (w, h) = (f.w / step, f.h / step);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let row = (y * step) as usize * f.stride;
        for x in 0..w {
            let i = row + (x * step) as usize * 4;
            let p = f.data.get(i..i + 4)?;
            match f.fmt {
                crate::convert::PixFmt::Bgrx => out.extend_from_slice(&[p[2], p[1], p[0], 255]),
                crate::convert::PixFmt::Rgbx => out.extend_from_slice(&[p[0], p[1], p[2], 255]),
            }
        }
    }
    Some((w, h, out))
}

struct Capturer {
    #[cfg(target_os = "linux")]
    last_gen: u64,
    last_hash: u64,
    monitor: Option<xcap::Monitor>,
    monitor_gen: u64,
    pattern_t: u32,
}

impl Capturer {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            last_gen: 0,
            last_hash: 0,
            monitor: None,
            monitor_gen: 0,
            pattern_t: 0,
        }
    }

    /// Returns a new frame if there is one; `None` if the screen has not changed.
    fn next(&mut self, timeout: Duration) -> Result<Option<Arc<RawFrame>>> {
        let r = self.next_frame(timeout);
        if let Ok(Some(f)) = &r {
            if let Ok(mut g) = LAST_FRAME.lock() {
                *g = Some(f.clone());
            }
        }
        r
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Arc<RawFrame>>> {
        if test_pattern() {
            return Ok(Some(Arc::new(self.pattern())));
        }
        #[cfg(target_os = "linux")]
        if portal_capture() {
            crate::wayland::ensure_started();
            if crate::wayland::is_ready() || crate::wayland::portal_pending() {
                return Ok(crate::wayland::wait_frame(&mut self.last_gen, timeout));
            }
            std::thread::sleep(timeout);
            anyhow::bail!("no Wayland screen sharing permission (approve the permission dialog on the host desktop)");
        }
        let _ = timeout;
        self.xcap()
    }

    fn xcap(&mut self) -> Result<Option<Arc<RawFrame>>> {
        let gen = MONITOR_GEN.load(Ordering::Relaxed);
        if gen != self.monitor_gen {
            self.monitor_gen = gen;
            self.monitor = None;
            self.last_hash = 0;
        }
        if self.monitor.is_none() {
            let all = xcap::Monitor::all().context("failed to list monitors (macOS: Screen Recording permission required)")?;
            let primary = all.iter().position(|m| m.is_primary().unwrap_or(false)).unwrap_or(0);
            let chosen = match MONITOR_SEL.load(Ordering::Relaxed) {
                i if i < all.len() => i,
                _ => primary,
            };
            self.monitor = all.into_iter().nth(chosen);
            if self.monitor.is_none() {
                anyhow::bail!("no monitor");
            }
        }
        let mon = self.monitor.as_ref().expect("monitor was just selected");
        let img = match mon.capture_image() {
            Ok(img) => img,
            Err(e) => {
                self.monitor = None;
                return Err(e).context("screen capture failed (macOS: Screen Recording permission required)");
            }
        };
        let scale = mon.scale_factor().ok().filter(|s| *s > 0.0).unwrap_or(1.0);
        crate::input::set_geo(crate::input::CaptureGeo {
            scale,
            mon_x: mon.x().unwrap_or(0),
            mon_y: mon.y().unwrap_or(0),
        });
        let (w, h) = (img.width(), img.height());
        let mut data = img.into_raw();
        // Safe view: private windows are blacked out (window positions are in points on macOS).
        let window_scale = if cfg!(target_os = "macos") { scale } else { 1.0 };
        crate::privacy::mask(&mut data, w, h, mon.x().unwrap_or(0), mon.y().unwrap_or(0), window_scale);
        let hash = quick_hash(&data) ^ ((w as u64) << 32 | h as u64);
        if hash == self.last_hash {
            return Ok(None);
        }
        self.last_hash = hash;
        Ok(Some(Arc::new(RawFrame::from_rgba(w, h, data))))
    }

    /// Synthetic frame for CI/tests: the colors and a box shift over time.
    fn pattern(&mut self) -> RawFrame {
        let (w, h) = (1280u32, 720u32);
        self.pattern_t = self.pattern_t.wrapping_add(1);
        let t = self.pattern_t;
        let bx = (t * 8) % (w - 120);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let inside = x >= bx && x < bx + 120 && y > 300 && y < 420;
                if inside {
                    data.extend_from_slice(&[255, 255, 255, 255]);
                } else {
                    data.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, (t * 3 % 256) as u8, 255]);
                }
            }
        }
        RawFrame::from_rgba(w, h, data)
    }
}

fn quick_hash(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut chunks = b.chunks_exact(8);
    for c in &mut chunks {
        h = (h ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(0x0100_0000_01b3).rotate_left(29);
    }
    for &x in chunks.remainder() {
        h = (h ^ x as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

// ---- H.264 ----

struct Enc {
    enc: openh264::encoder::Encoder,
    w: u32,
    h: u32,
    bitrate: u32,
}

impl Enc {
    fn new(w: u32, h: u32, bitrate: u32, fps: u32) -> Result<Self> {
        use openh264::encoder::{
            BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Level, Profile, UsageType, VuiConfig,
        };
        // Level 4.1 up to 1080p; 5.1 above that (1440p/4K).
        let level = if (w as u64 * h as u64) <= 1920 * 1088 { Level::Level_4_1 } else { Level::Level_5_1 };
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bitrate))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .usage_type(UsageType::ScreenContentRealTime)
            .profile(Profile::Baseline)
            .level(level)
            .skip_frames(true)
            // Not supported in screen content mode (OpenH264 turns it off anyway and logs a warning).
            .adaptive_quantization(false)
            .background_detection(false)
            .num_threads(0)
            .vui(VuiConfig::bt709())
            // Safety net: every 10 s; real keyframes are sent on request.
            .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 10));
        let enc = Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
            .context("failed to open h264 encoder")?;
        Ok(Self { enc, w, h, bitrate })
    }

    /// Change the target bitrate without recreating the encoder (no IDR).
    fn set_bitrate(&mut self, bps: u32) {
        let mut info = openh264_sys2::SBitrateInfo {
            iLayer: openh264_sys2::SPATIAL_LAYER_ALL,
            iBitrate: bps as std::os::raw::c_int,
        };
        // SAFETY: the encoder is initialized; the option struct is valid for the duration of the call.
        let rc = unsafe {
            self.enc.raw_api().set_option(
                openh264_sys2::ENCODER_OPTION_BITRATE,
                &mut info as *mut _ as *mut std::os::raw::c_void,
            )
        };
        if rc == 0 {
            self.bitrate = bps;
        } else {
            tracing::debug!("failed to change bitrate (rc={rc})");
        }
    }
}

fn h264_worker(tx: broadcast::Sender<Arc<EncodedFrame>>) {
    let mut cap = Capturer::new();
    let mut enc: Option<Enc> = None;
    let mut yuv = Vec::new();
    let mut last_raw: Option<Arc<RawFrame>> = None;
    let mut seq = 0u64;
    let mut last_receivers = 0usize;
    let mut last_attempt: Option<Instant> = None;
    let mut cur_preset = preset();
    let mut factor = 1.0f32;
    let mut last_congestion = Instant::now() - Duration::from_secs(60);
    let mut last_rate_check = Instant::now();
    let mut errors = 0u32;
    let t0 = Instant::now();
    let (mut st_frames, mut st_bytes, mut st_conv, mut st_enc, mut st_t) =
        (0u32, 0usize, Duration::ZERO, Duration::ZERO, Instant::now());

    loop {
        let receivers = tx.receiver_count();
        if receivers == 0 {
            if enc.is_some() {
                tracing::info!("no viewers left; H.264 pipeline idle");
            }
            enc = None;
            last_receivers = 0;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if receivers > last_receivers {
            FORCE_KEY.store(true, Ordering::Relaxed);
        }
        last_receivers = receivers;

        let p = preset();
        let prof = profile(p);
        let mut reconvert = false;
        if p != cur_preset {
            cur_preset = p;
            factor = 1.0;
            reconvert = true;
            FORCE_KEY.store(true, Ordering::Relaxed);
        }
        // Rate limit is based on the last capture attempt, so the loop doesn't burn CPU on a static screen.
        let period = Duration::from_micros(1_000_000 / prof.fps.max(1) as u64);
        if let Some(t) = last_attempt {
            let el = t.elapsed();
            if el < period {
                std::thread::sleep(period - el);
            }
        }
        last_attempt = Some(Instant::now());

        let force = FORCE_KEY.swap(false, Ordering::Relaxed);
        let wait = if force || reconvert { Duration::from_millis(40) } else { Duration::from_millis(250) };
        let raw = match cap.next(wait) {
            Ok(Some(raw)) => {
                errors = 0;
                last_raw = Some(raw.clone());
                raw
            }
            Ok(None) => match (&last_raw, force || reconvert) {
                (Some(raw), true) => raw.clone(),
                _ => {
                    if force {
                        FORCE_KEY.store(true, Ordering::Relaxed);
                    }
                    continue;
                }
            },
            Err(e) => {
                errors += 1;
                if errors <= 3 || errors % 50 == 0 {
                    tracing::warn!("capture error ({errors}): {e:#}");
                }
                if force {
                    FORCE_KEY.store(true, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };

        let (dw, dh) = convert::target_size(raw.w, raw.h, prof.max_w);
        let conv_t = Instant::now();
        if !convert::to_i420(&raw, dw, dh, &mut yuv) {
            continue;
        }
        st_conv += conv_t.elapsed();

        // Adaptive bitrate: drop 30% on congestion, raise 15% after 6 s without trouble.
        if last_rate_check.elapsed() >= Duration::from_secs(1) {
            last_rate_check = Instant::now();
            if CONGESTION.swap(0, Ordering::Relaxed) > 0 {
                factor = (factor * 0.7).max(0.2);
                last_congestion = Instant::now();
            } else if factor < 1.0 && last_congestion.elapsed() > Duration::from_secs(6) {
                factor = (factor * 1.15).min(1.0);
            }
        }
        let target_bps = ((prof.bitrate as f32 * factor) as u32).max(300_000);

        let needs_new = enc.as_ref().map_or(true, |e| e.w != dw || e.h != dh);
        if needs_new {
            match Enc::new(dw, dh, target_bps, prof.fps) {
                Ok(e) => {
                    tracing::info!("H.264 encoder: {dw}x{dh}, {} kbit/s, {} fps", target_bps / 1000, prof.fps);
                    enc = Some(e);
                }
                Err(e) => {
                    tracing::warn!("{e:#}");
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
        }
        let e = enc.as_mut().expect("encoder was just created");
        if (e.bitrate as i64 - target_bps as i64).unsigned_abs() > (e.bitrate / 20) as u64 {
            tracing::debug!("bitrate: {} -> {} kbit/s", e.bitrate / 1000, target_bps / 1000);
            e.set_bitrate(target_bps);
        }
        let force_key = force || reconvert || needs_new;
        if force_key {
            e.enc.force_intra_frame();
        }

        let (w, h) = (dw as usize, dh as usize);
        let (yp, uv) = yuv.split_at(w * h);
        let (up, vp) = uv.split_at(w * h / 4);
        let src = openh264::formats::YUVSlices::new((yp, up, vp), (w, h), (w, w / 2, w / 2));
        let enc_t = Instant::now();
        let ts = openh264::Timestamp::from_millis(t0.elapsed().as_millis() as u64);
        let (data, key) = match e.enc.encode_at(&src, ts) {
            Ok(bs) => {
                use openh264::encoder::FrameType;
                let key = matches!(bs.frame_type(), FrameType::IDR | FrameType::I);
                (bs.to_vec(), key)
            }
            Err(err) => {
                tracing::warn!("h264 encode error: {err}");
                enc = None;
                continue;
            }
        };
        st_enc += enc_t.elapsed();
        if force_key && !key {
            // Rate control skipped the frame: keep the keyframe request pending.
            FORCE_KEY.store(true, Ordering::Relaxed);
        }
        if data.is_empty() {
            continue;
        }
        seq += 1;
        st_frames += 1;
        st_bytes += data.len();
        let _ = tx.send(Arc::new(EncodedFrame {
            seq,
            w: dw,
            h: dh,
            src_w: raw.w,
            src_h: raw.h,
            key,
            jpeg: false,
            data,
        }));

        let el = st_t.elapsed();
        if el >= Duration::from_secs(10) {
            let n = st_frames.max(1) as f64;
            tracing::info!(
                "video: {:.1} fps, {:.0} kbit/s, convert {:.1} ms, encode {:.1} ms, {}x{}, viewers {}",
                st_frames as f64 / el.as_secs_f64(),
                st_bytes as f64 * 8.0 / 1000.0 / el.as_secs_f64(),
                st_conv.as_secs_f64() * 1000.0 / n,
                st_enc.as_secs_f64() * 1000.0 / n,
                dw,
                dh,
                receivers
            );
            (st_frames, st_bytes, st_conv, st_enc, st_t) = (0, 0, Duration::ZERO, Duration::ZERO, Instant::now());
        }
    }
}

// ---- JPEG (browsers without WebCodecs) ----

fn jpeg_worker(tx: broadcast::Sender<Arc<EncodedFrame>>) {
    use image::codecs::jpeg::JpegEncoder;
    let mut cap = Capturer::new();
    let mut last_raw: Option<Arc<RawFrame>> = None;
    let mut seq = 0u64;
    let mut last_receivers = 0usize;
    let mut last_attempt: Option<Instant> = None;
    let mut cur_preset = preset();
    let mut errors = 0u32;
    loop {
        let receivers = tx.receiver_count();
        if receivers == 0 {
            last_receivers = 0;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let mut force = receivers > last_receivers || FORCE_JPEG.swap(false, Ordering::Relaxed);
        last_receivers = receivers;
        let p = preset();
        if p != cur_preset {
            cur_preset = p;
            force = true;
        }
        let prof = profile(p);
        let period = Duration::from_micros(1_000_000 / prof.jpeg_fps.max(1) as u64);
        if let Some(t) = last_attempt {
            let el = t.elapsed();
            if el < period {
                std::thread::sleep(period - el);
            }
        }
        last_attempt = Some(Instant::now());
        let raw = match cap.next(Duration::from_millis(if force { 40 } else { 250 })) {
            Ok(Some(raw)) => {
                errors = 0;
                last_raw = Some(raw.clone());
                raw
            }
            Ok(None) => match (&last_raw, force) {
                (Some(raw), true) => raw.clone(),
                _ => continue,
            },
            Err(e) => {
                errors += 1;
                if errors <= 3 || errors % 50 == 0 {
                    tracing::warn!("JPEG capture error ({errors}): {e:#}");
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let (dw, dh) = convert::target_size(raw.w, raw.h, prof.jpeg_w);
        let Some(rgb) = convert::to_rgb(&raw, dw, dh) else { continue };
        let mut out = Vec::with_capacity(rgb.len() / 8);
        let mut enc = JpegEncoder::new_with_quality(&mut out, prof.jpeg_q);
        if enc.encode(&rgb, dw, dh, image::ExtendedColorType::Rgb8).is_err() {
            continue;
        }
        seq += 1;
        let _ = tx.send(Arc::new(EncodedFrame {
            seq,
            w: dw,
            h: dh,
            src_w: raw.w,
            src_h: raw.h,
            key: true,
            jpeg: true,
            data: out,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_mapping_scales_back() {
        let f = EncodedFrame { seq: 1, w: 1920, h: 1080, src_w: 3840, src_h: 2160, key: true, jpeg: false, data: vec![] };
        assert_eq!(f.to_capture(960, 540), (1920, 1080));
        assert_eq!(f.to_capture(5000, 5000), (3838, 2158));
    }

    #[test]
    fn presets_are_ordered() {
        let fast = profile(Preset::Fast);
        let bal = profile(Preset::Balanced);
        let sharp = profile(Preset::Sharp);
        assert!(fast.bitrate < bal.bitrate && bal.bitrate < sharp.bitrate);
        assert!(fast.max_w <= bal.max_w && bal.max_w <= sharp.max_w);
    }

    #[test]
    fn h264_encodes_converted_frame() {
        let raw = Capturer::new().pattern();
        let (w, h) = convert::target_size(raw.w, raw.h, 960);
        let mut yuv = Vec::new();
        assert!(convert::to_i420(&raw, w, h, &mut yuv));
        let mut e = Enc::new(w, h, 2_000_000, 30).unwrap();
        e.set_bitrate(1_000_000);
        let (wu, hu) = (w as usize, h as usize);
        let (yp, uv) = yuv.split_at(wu * hu);
        let (up, vp) = uv.split_at(wu * hu / 4);
        let src = openh264::formats::YUVSlices::new((yp, up, vp), (wu, hu), (wu, wu / 2, wu / 2));
        let bs = e.enc.encode(&src).unwrap();
        assert!(matches!(bs.frame_type(), openh264::encoder::FrameType::IDR));
        assert!(!bs.to_vec().is_empty());
    }
}
