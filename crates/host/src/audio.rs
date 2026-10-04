//! System sound for remote sessions ("what this computer is playing").
//!
//! One capture runs while at least one session listens and stops a few seconds after the
//! last listener leaves. Output: 20 ms packets, 48 kHz stereo, Opus at 96 kbit/s, plus the
//! same audio as 24 kHz 16-bit PCM for browsers without an Opus decoder.
//! - Linux: a PipeWire stream that records the default output (monitor of the default sink).
//! - Windows: WASAPI loopback; macOS 14.6+: CoreAudio process tap (both through cpal).

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

pub const RATE: u32 = 48_000;
/// Samples per channel in one 20 ms packet.
pub const FRAME: usize = 960;
const BITRATE: i32 = 96_000;
/// The capture stops this long after the last listener left.
const LINGER: Duration = Duration::from_secs(5);
/// Silent packets still sent after the sound stops, then nothing until sound returns.
const SILENT_TAIL: u32 = 10;

pub struct AudioPacket {
    pub seq: u64,
    pub opus: Vec<u8>,
    /// The same 20 ms as 24 kHz stereo s16le.
    pub pcm24: Vec<u8>,
}

fn hub() -> &'static broadcast::Sender<Arc<AudioPacket>> {
    static HUB: OnceLock<broadcast::Sender<Arc<AudioPacket>>> = OnceLock::new();
    HUB.get_or_init(|| broadcast::channel(50).0)
}

static RUNNING: AtomicBool = AtomicBool::new(false);

fn listeners() -> usize {
    hub().receiver_count()
}

/// Listen to the computer's sound (starts the capture if it is not running).
pub fn subscribe() -> broadcast::Receiver<Arc<AudioPacket>> {
    let rx = hub().subscribe();
    if !RUNNING.swap(true, Ordering::AcqRel) {
        let spawned = std::thread::Builder::new().name("rf-audio".into()).spawn(|| loop {
            let res = if test_tone() { capture_test_tone() } else { capture() };
            match &res {
                Ok(()) => tracing::info!("sound capture stopped (nobody listening)"),
                Err(e) => tracing::warn!("sound capture unavailable: {e:#}"),
            }
            RUNNING.store(false, Ordering::Release);
            // A listener that arrived while the capture was stopping would otherwise get nothing.
            if res.is_err() || listeners() == 0 || RUNNING.swap(true, Ordering::AcqRel) {
                break;
            }
        });
        if spawned.is_err() {
            RUNNING.store(false, Ordering::Release);
        }
    }
    rx
}

/// Browser message body: [codec: 0 = Opus, 1 = PCM 24 kHz s16le stereo][seq: u32 BE][data].
pub fn web_payload(p: &AudioPacket, pcm: bool) -> Vec<u8> {
    let data = if pcm { &p.pcm24 } else { &p.opus };
    let mut v = Vec::with_capacity(5 + data.len());
    v.push(pcm as u8);
    v.extend_from_slice(&(p.seq as u32).to_be_bytes());
    v.extend_from_slice(data);
    v
}

/// Linear-interpolation resampler for interleaved stereo (fine for 44.1 <-> 48 kHz and
/// avoids pulling in a DSP library).
pub struct Resampler {
    step: f64,
    pos: f64,
    hist: Vec<[f32; 2]>,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to.max(1) as f64, pos: 0.0, hist: Vec::new() }
    }

    /// Appends the resampled frames to `out` (interleaved L, R).
    pub fn process(&mut self, input: impl IntoIterator<Item = [f32; 2]>, out: &mut Vec<f32>) {
        self.hist.extend(input);
        while self.pos + 1.0 < self.hist.len() as f64 {
            let i = self.pos as usize;
            let f = (self.pos - i as f64) as f32;
            let (a, b) = (self.hist[i], self.hist[i + 1]);
            out.push(a[0] + (b[0] - a[0]) * f);
            out.push(a[1] + (b[1] - a[1]) * f);
            self.pos += self.step;
        }
        let used = (self.pos as usize).min(self.hist.len().saturating_sub(1));
        self.hist.drain(..used);
        self.pos -= used as f64;
    }
}

/// Turns captured samples (any rate, any channel count) into 20 ms packets.
struct Packetizer {
    enc: opus::Encoder,
    resampler: Option<Resampler>,
    channels: usize,
    pending: Vec<f32>,
    seq: u64,
    silent: u32,
    out: Vec<u8>,
}

impl Packetizer {
    fn new(rate: u32, channels: usize) -> Result<Self> {
        let mut enc = opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::Audio)
            .context("cannot open the Opus encoder")?;
        enc.set_bitrate(opus::Bitrate::Bits(BITRATE))?;
        Ok(Self {
            enc,
            resampler: (rate != RATE).then(|| Resampler::new(rate, RATE)),
            channels: channels.max(1),
            pending: Vec::with_capacity(FRAME * 4),
            seq: 0,
            silent: 0,
            out: vec![0; 4000],
        })
    }

    /// Feed interleaved samples with `self.channels` channels (extra channels are dropped,
    /// mono is played on both sides).
    fn push(&mut self, data: &[f32]) {
        let ch = self.channels;
        let stereo = data.chunks_exact(ch).map(|f| if ch == 1 { [f[0], f[0]] } else { [f[0], f[1]] });
        match &mut self.resampler {
            Some(r) => r.process(stereo, &mut self.pending),
            None => {
                for [l, r] in stereo {
                    self.pending.push(l);
                    self.pending.push(r);
                }
            }
        }
        while self.pending.len() >= FRAME * 2 {
            let frame: Vec<f32> = self.pending.drain(..FRAME * 2).collect();
            self.emit(&frame);
        }
    }

    fn emit(&mut self, frame: &[f32]) {
        self.seq += 1;
        if frame.iter().any(|s| s.abs() > 1e-4) {
            self.silent = 0;
        } else {
            self.silent = self.silent.saturating_add(1);
            if self.silent > SILENT_TAIL {
                return;
            }
        }
        let n = match self.enc.encode_float(frame, &mut self.out) {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!("opus encode: {e}");
                return;
            }
        };
        let mut pcm24 = Vec::with_capacity(FRAME * 2);
        for pair in frame.chunks_exact(4) {
            for c in 0..2 {
                let v = (pair[c] + pair[c + 2]) * 0.5;
                pcm24.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
            }
        }
        let _ = hub().send(Arc::new(AudioPacket { seq: self.seq, opus: self.out[..n].to_vec(), pcm24 }));
    }
}

/// Tracks how long nobody has been listening.
#[derive(Default)]
struct Idle(Option<Instant>);

impl Idle {
    fn expired(&mut self) -> bool {
        if listeners() > 0 {
            self.0 = None;
            return false;
        }
        self.0.get_or_insert_with(Instant::now).elapsed() >= LINGER
    }
}

fn test_tone() -> bool {
    std::env::var("RF_TEST_TONE").map(|v| v == "1").unwrap_or(false)
}

/// RF_TEST_TONE=1: a barely audible 440 Hz tone instead of the real sound (for tests).
fn capture_test_tone() -> Result<()> {
    let mut pk = Packetizer::new(RATE, 2)?;
    let mut idle = Idle::default();
    let (start, mut t, mut frames) = (Instant::now(), 0u64, 0u64);
    while !idle.expired() {
        let buf: Vec<f32> = (0..FRAME)
            .flat_map(|_| {
                let v = (t as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.002;
                t += 1;
                [v, v]
            })
            .collect();
        pk.push(&buf);
        frames += 1;
        let due = start + Duration::from_millis(frames * 20);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn capture() -> Result<()> {
    use pipewire as pw;
    use pw::{properties::properties, spa};

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None).context("cannot connect to PipeWire")?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "remotefriend-sound",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
            // Record what goes to the default output instead of a microphone.
            *pw::keys::STREAM_CAPTURE_SINK => "true",
            *pw::keys::NODE_LATENCY => "960/48000",
        },
    )?;

    let weak = mainloop.downgrade();
    let _listener = stream
        .add_local_listener_with_user_data(Packetizer::new(RATE, 2)?)
        .state_changed(move |_, _, old, new| {
            tracing::debug!("sound stream state: {old:?} -> {new:?}");
            if matches!(new, pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected) {
                if let Some(ml) = weak.upgrade() {
                    ml.quit();
                }
            }
        })
        .param_changed(|_, pk, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let mut info = spa::param::audio::AudioInfoRaw::default();
            if info.parse(param).is_err() {
                return;
            }
            tracing::info!("sound format: {:?} {} Hz, {} channels", info.format(), info.rate(), info.channels());
            if info.rate() != RATE || info.channels() != 2 {
                if let Ok(p) = Packetizer::new(info.rate(), info.channels() as usize) {
                    *pk = p;
                }
            }
        })
        .process(|stream, pk| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let Some(data) = buffer.datas_mut().first_mut() else { return };
            let (offset, size) = (data.chunk().offset() as usize, data.chunk().size() as usize);
            let Some(bytes) = data.data() else { return };
            let Some(bytes) = offset.checked_add(size).and_then(|end| bytes.get(offset..end)) else { return };
            let samples: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            pk.push(&samples);
        })
        .register()?;

    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(RATE);
    info.set_channels(2);
    let mut position = [0u32; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow::anyhow!("pod serialize: {e}"))?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values).context("pod parse")?];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    // Stop once nobody has listened for a while.
    let weak = mainloop.downgrade();
    let idle = std::cell::RefCell::new(Idle::default());
    let timer = mainloop.loop_().add_timer(move |_| {
        if idle.borrow_mut().expired() {
            if let Some(ml) = weak.upgrade() {
                ml.quit();
            }
        }
    });
    timer
        .update_timer(Some(Duration::from_secs(1)), Some(Duration::from_secs(1)))
        .into_result()
        .map_err(|e| anyhow::anyhow!("timer: {e}"))?;
    tracing::info!("sound capture started (PipeWire)");
    mainloop.run();
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn capture() -> Result<()> {
    capture_cpal()
}

/// Windows (WASAPI loopback) and macOS 14.6+ (process tap): an input stream opened on the
/// default *output* device records what it plays. Compiled everywhere so it stays checked.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn capture_cpal() -> Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::mpsc::{sync_channel, RecvTimeoutError};

    let device = cpal::default_host().default_output_device().context("no sound output device")?;
    let config = device.default_output_config().context("cannot read the sound output format")?;
    let (channels, rate) = (config.channels() as usize, config.sample_rate());
    let (tx, rx) = sync_channel::<Vec<f32>>(64);
    let on_error = |e: cpal::Error| tracing::warn!("sound capture error: {e}");
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.config(),
            move |d: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = tx.try_send(d.to_vec());
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.config(),
            move |d: &[i16], _: &cpal::InputCallbackInfo| {
                let _ = tx.try_send(d.iter().map(|s| *s as f32 / 32768.0).collect());
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I32 => device.build_input_stream(
            config.config(),
            move |d: &[i32], _: &cpal::InputCallbackInfo| {
                let _ = tx.try_send(d.iter().map(|s| *s as f32 / 2_147_483_648.0).collect());
            },
            on_error,
            None,
        )?,
        other => anyhow::bail!("unsupported sound format: {other:?}"),
    };
    stream.play().context("cannot start sound capture")?;
    tracing::info!("sound capture started ({rate} Hz, {channels} channels)");
    let mut pk = Packetizer::new(rate, channels)?;
    let mut idle = Idle::default();
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(d) => pk.push(&d),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if idle.expired() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_keeps_duration() {
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        for _ in 0..10 {
            r.process((0..4410).map(|i| [i as f32, -(i as f32)]), &mut out);
        }
        let frames = out.len() / 2;
        assert!((47_990..=48_010).contains(&frames), "{frames}");
    }

    #[test]
    fn packets_are_20ms() {
        let mut rx = hub().subscribe();
        let mut pk = Packetizer::new(RATE, 2).unwrap();
        let tone: Vec<f32> = (0..FRAME * 2 * 3).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.3).collect();
        pk.push(&tone);
        for _ in 0..3 {
            let p = rx.try_recv().unwrap();
            assert!(!p.opus.is_empty() && p.opus.len() < 1000);
            assert_eq!(p.pcm24.len(), FRAME * 2);
        }
        assert!(rx.try_recv().is_err());
    }
}
