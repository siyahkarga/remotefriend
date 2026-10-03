//! Plays the remote computer's sound: Opus packets -> 48 kHz stereo -> default output device.

use remote_friend_host::audio::{Resampler, RATE};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// Sound is buffered this long before playback (re)starts; absorbs network jitter.
const START_MS: usize = 60;
/// Above this, the oldest sound is dropped so the delay never grows.
const MAX_MS: usize = 250;

/// Interleaved stereo at the device rate.
#[derive(Default)]
struct Ring {
    buf: VecDeque<f32>,
    playing: bool,
}

pub(crate) struct Player {
    dec: opus::Decoder,
    pcm: Vec<f32>,
    resampler: Option<Resampler>,
    ring: Arc<Mutex<Ring>>,
    stop: Arc<AtomicBool>,
    max_len: usize,
}

impl Player {
    /// Opens the default output device; None (logged) if there is no usable one.
    pub(crate) fn start() -> Option<Player> {
        let ring = Arc::new(Mutex::new(Ring::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (r, s) = (ring.clone(), stop.clone());
        // The output stream stays on its own thread (it is not Send on every platform).
        std::thread::Builder::new()
            .name("rf-sound-out".into())
            .spawn(move || run_output(r, s, ready_tx))
            .ok()?;
        let rate = match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(rate)) => rate,
            Ok(Err(e)) => {
                tracing::warn!("no sound output: {e}");
                return None;
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                tracing::warn!("no sound output: the device did not start");
                return None;
            }
        };
        let dec = opus::Decoder::new(RATE, opus::Channels::Stereo).ok()?;
        Some(Player {
            dec,
            pcm: vec![0.0; 5760 * 2],
            resampler: (rate != RATE).then(|| Resampler::new(RATE, rate)),
            ring,
            stop,
            max_len: rate as usize * 2 * MAX_MS / 1000,
        })
    }

    pub(crate) fn push_opus(&mut self, data: &[u8]) {
        let n = match self.dec.decode_float(data, &mut self.pcm, false) {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!("opus decode: {e}");
                return;
            }
        };
        let frames = &self.pcm[..n * 2];
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        match &mut self.resampler {
            Some(r) => {
                let mut out = Vec::with_capacity(frames.len() + 16);
                r.process(frames.chunks_exact(2).map(|f| [f[0], f[1]]), &mut out);
                ring.buf.extend(out);
            }
            None => ring.buf.extend(frames.iter().copied()),
        }
        if ring.buf.len() > self.max_len {
            let excess = (ring.buf.len() - self.max_len / 2) & !1;
            ring.buf.drain(..excess);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run_output(ring: Arc<Mutex<Ring>>, stop: Arc<AtomicBool>, ready: mpsc::Sender<Result<u32, String>>) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let Some(device) = cpal::default_host().default_output_device() else {
        let _ = ready.send(Err("no sound output device".into()));
        return;
    };
    let config = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(format!("cannot read the output format: {e}")));
            return;
        }
    };
    let (channels, rate) = (config.channels() as usize, config.sample_rate());
    let start_len = rate as usize * 2 * START_MS / 1000;
    let fill = move |out: &mut [f32]| {
        let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
        if !ring.playing && ring.buf.len() >= start_len {
            ring.playing = true;
        }
        for frame in out.chunks_mut(channels.max(1)) {
            let (l, r) = match ring.playing {
                true => match (ring.buf.pop_front(), ring.buf.pop_front()) {
                    (Some(l), Some(r)) => (l, r),
                    _ => {
                        ring.playing = false; // ran dry: refill before playing again
                        (0.0, 0.0)
                    }
                },
                false => (0.0, 0.0),
            };
            if let [only] = frame {
                *only = (l + r) * 0.5;
            } else {
                frame[0] = l;
                frame[1] = r;
                frame[2..].fill(0.0);
            }
        }
    };
    let on_error = |e: cpal::Error| tracing::warn!("sound output error: {e}");
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config.config(),
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| fill(out),
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_output_stream(
            config.config(),
            move |out: &mut [i16], _: &cpal::OutputCallbackInfo| {
                let mut tmp = vec![0.0f32; out.len()];
                fill(&mut tmp);
                for (o, s) in out.iter_mut().zip(tmp) {
                    *o = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                }
            },
            on_error,
            None,
        ),
        other => {
            let _ = ready.send(Err(format!("unsupported output format: {other:?}")));
            return;
        }
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(format!("cannot open the output: {e}")));
            return;
        }
    };
    if let Err(e) = stream.play() {
        let _ = ready.send(Err(format!("cannot start the output: {e}")));
        return;
    }
    let _ = ready.send(Ok(rate));
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }
}
