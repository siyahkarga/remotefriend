# RemoteFriend Performance Guide

## Why could v0.5.5 drop to 3–5 FPS?

1. The target was 15 FPS, and after capture/encode/send finished it slept a fixed extra 66 ms. Real FPS was roughly `1 / (processing time + 66 ms)`.
2. When the LAN browser page was opened over plain HTTP, WebCodecs was often unavailable, so the CPU-heavy full-frame JPEG fallback kicked in.
3. Every viewer captured and encoded the screen separately.
4. On every UI pass the client cloned the same RGBA frame and re-uploaded it to the GPU texture.
5. Video and input queues were unbounded or shaped in a way that increased latency.
6. RGBA → RGB → YUV420 and software OpenH264 are still CPU-expensive.

## v0.5.6 changes

- Fixed-rate 30 FPS pacing; no extra sleep or latency build-up when processing runs late.
- A single global H.264 capture/encode pipeline; all viewers receive the same encoded frame.
- A single global JPEG fallback pipeline.
- Small broadcast queues; a slow client skips old frames and catches up to the latest one.
- H.264 keyframe interval of about 1 second; new viewers recover quickly.
- OpenH264 auto-threading, screen-content realtime mode, frame skipping and a 6 Mbit/s default profile.
- xcap monitor selection is no longer redone on every frame.
- The client consumes the RGBA frame without cloning it and updates the GPU texture only when a new frame arrives.
- If the browser H.264 decode queue grows, it drops the old chain and waits for the next keyframe.
- JPEG decoding keeps only the newest pending frame.
- Input and network channels are bounded; stale mouse moves are dropped when needed.

## v0.6.2 changes

- **Single-pass conversion:** the raw frame (BGRx/RGBA) is converted straight to I420; downscaling happens in the same pass
  (box filter) on 4 threads. The old pipeline's RGBA copy → resize → RGB copy →
  YUV steps are gone (~1 ms at 1080p).
- **Damage-based streaming:** on Wayland, PipeWire delivers a frame only when the screen changes (30 fps preferred,
  up to 60); on X11/Windows/macOS unchanged frames are not encoded. Bandwidth on a static screen is ~0.
- **On-demand keyframes:** no forced IDR every second (more bits left for P-frames);
  an IDR is sent only for a new viewer, frame loss or a client request. On a static screen the last frame is re-encoded
  and sent to a new viewer (no black screen).
- **Latency cap:** the client acks every frame (`ack`); if more than 8 frames or anything older than 700 ms is
  unacknowledged, new frames are skipped at the source and a clean keyframe is requested. Seconds of latency
  no longer build up in network buffers.
- **Adaptive bitrate:** drops 30% on congestion and rises 15% after 6 s without problems (without
  recreating the encoder).
- **Quality presets:** Fast / Balanced / Sharp (switchable live from the browser).
- Dependencies are built optimized even in debug builds (smooth with `cargo run` too).

## Measuring the bottleneck from the terminal

While a viewer is active, the host prints this line every 10 seconds:

```text
video: 29.6 fps, 2400 kbit/s, convert 1.1 ms, encode 13.9 ms, 1920x1080, viewers 1
```

- If `encode` is well above 33 ms, software OpenH264 is limiting you to below 30 FPS; in the browser choose
  ⚙ → **Fast**, or set `RF_QUALITY=fast`.
- If fps is low but encoding is fast, the screen is changing little (normal), or permission was not granted on Wayland.
- If the host numbers are high but the client FPS is low, client decoding/GPU texture upload or network latency is the limit.
- If the browser info panel shows `JPEG`, the fallback is in use instead of HTTPS/WebCodecs.

## Choosing a profile

### Balanced

```text
RF_FPS=30
RF_MAX_WIDTH=1600
RF_BITRATE_BPS=6000000
```

### Lower CPU and bandwidth usage

```text
RF_FPS=20
RF_MAX_WIDTH=1280
RF_BITRATE_BPS=4000000
```

### 1080p sharpness

```text
RF_FPS=30
RF_MAX_WIDTH=1920
RF_BITRATE_BPS=8000000
```

This profile needs a strong CPU. Scenes where the whole screen changes constantly, such as video playback, are much heavier than a static desktop.

### Plain HTTP browser/JPEG

```text
RF_MAX_WIDTH=1280
RF_JPEG_FPS=8
RF_JPEG_Q=62
```

Don't expect 30 FPS in JPEG mode. For smooth video, use the native client or WebCodecs H.264 over valid HTTPS/WSS.

## Build settings

- Use a release build: `cargo build --release`.
- If `nasm` is installed, OpenH264 can use its SIMD/assembly path.
- For local builds targeting the same computer/CPU family, you can try `RUSTFLAGS="-C target-cpu=native"`.
- Don't use `target-cpu=native` for general distribution binaries; they may not run on other CPUs.

## The next big performance step

This hotfix improves the software H.264 architecture, but real 1080p60/1440p60 targets need a near-zero-copy path:

1. Windows Graphics Capture or DXGI Desktop Duplication.
2. Passing the GPU surface directly to NVENC/QSV/AMF.
3. PipeWire DMA-BUF + VAAPI/NVENC on Linux.
4. WebRTC for congestion control, jitter buffer, DTLS-SRTP and hardware browser decoding.
5. A separate low-latency data channel for input; controlled backpressure for files.

On a host with an NVIDIA GPU, NVENC is usually the single biggest performance gain over CPU OpenH264. This is not a small tweak; it requires changing the capture surface, color format, encoder and transport layer together.
