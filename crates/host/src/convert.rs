//! Raw screen frame -> I420 (H.264) / RGB (JPEG) conversion.
//!
//! In a single pass: pixel order (BGRx/RGBx/...) + downscaling + color space.
//! The old pipeline made four full-frame copies (RGBA copy -> resize -> RGB copy -> YUV);
//! here the source is read once and row pairs are split across threads.

/// Source pixel layout (4 bytes/pixel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PixFmt {
    /// B, G, R, X (PipeWire BGRx/BGRA, Windows/macOS BGRA)
    Bgrx,
    /// R, G, B, X (xcap RgbaImage, PipeWire RGBx/RGBA)
    Rgbx,
}

/// Captured raw frame. `stride` is the row length in bytes.
pub(crate) struct RawFrame {
    pub w: u32,
    pub h: u32,
    pub stride: usize,
    pub fmt: PixFmt,
    pub data: Vec<u8>,
}

impl RawFrame {
    pub(crate) fn from_rgba(w: u32, h: u32, data: Vec<u8>) -> Self {
        Self { w, h, stride: w as usize * 4, fmt: PixFmt::Rgbx, data }
    }

    fn valid(&self) -> bool {
        self.w >= 2
            && self.h >= 2
            && self.stride >= self.w as usize * 4
            && self.data.len() >= self.stride * (self.h as usize - 1) + self.w as usize * 4
    }
}

/// Target size: aspect ratio is kept, width does not exceed `max_w`, both are even.
pub(crate) fn target_size(w: u32, h: u32, max_w: u32) -> (u32, u32) {
    let (mut dw, mut dh) = (w, h);
    if w > max_w {
        dw = max_w;
        dh = ((h as u64 * max_w as u64) / w as u64) as u32;
    }
    ((dw & !1).max(2), (dh & !1).max(2))
}

/// Range [a, b) on the source axis covered by each target pixel.
fn spans(src: u32, dst: u32) -> Vec<(u32, u32)> {
    (0..dst)
        .map(|i| {
            let a = (i as u64 * src as u64 / dst as u64) as u32;
            let b = ((i as u64 + 1) * src as u64 / dst as u64) as u32;
            (a.min(src - 1), b.clamp(a + 1, src))
        })
        .collect()
}

/// Produce one target row as RGB (averaged with a box filter).
#[inline]
fn sample_row(src: &RawFrame, xs: &[(u32, u32)], ys: (u32, u32), out: &mut [u8]) {
    let (ri, bi) = match src.fmt {
        PixFmt::Bgrx => (2, 0),
        PixFmt::Rgbx => (0, 2),
    };
    let identity = xs.len() as u32 == src.w && ys.1 - ys.0 == 1;
    if identity {
        let row = &src.data[ys.0 as usize * src.stride..][..src.w as usize * 4];
        for (s, d) in row.chunks_exact(4).zip(out.chunks_exact_mut(3)) {
            d[0] = s[ri];
            d[1] = s[1];
            d[2] = s[bi];
        }
        return;
    }
    for (x, &(xa, xb)) in xs.iter().enumerate() {
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for y in ys.0..ys.1 {
            let row = &src.data[y as usize * src.stride..];
            for px in row[xa as usize * 4..xb as usize * 4].chunks_exact(4) {
                r += px[ri] as u32;
                g += px[1] as u32;
                b += px[bi] as u32;
                n += 1;
            }
        }
        let d = &mut out[x * 3..x * 3 + 3];
        d[0] = (r / n) as u8;
        d[1] = (g / n) as u8;
        d[2] = (b / n) as u8;
    }
}

fn workers(rows: usize) -> usize {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    n.clamp(1, 4).min(rows.max(1))
}

/// BT.709 limited range (the encoder VUI is bt709). Integer math, 8-bit scale.
#[inline]
fn y709(r: i32, g: i32, b: i32) -> u8 {
    ((47 * r + 157 * g + 16 * b + 128) >> 8).wrapping_add(16).clamp(16, 235) as u8
}

/// Converts the raw frame to `dw x dh` I420. `out` is reused.
pub(crate) fn to_i420(src: &RawFrame, dw: u32, dh: u32, out: &mut Vec<u8>) -> bool {
    if !src.valid() || dw < 2 || dh < 2 || dw % 2 != 0 || dh % 2 != 0 {
        return false;
    }
    let (w, h) = (dw as usize, dh as usize);
    out.resize(w * h * 3 / 2, 0);
    let xs = spans(src.w, dw);
    let ys = spans(src.h, dh);
    let (y_plane, uv) = out.split_at_mut(w * h);
    let (u_plane, v_plane) = uv.split_at_mut(w * h / 4);

    let pairs = h / 2;
    let threads = workers(pairs);
    let per = pairs.div_ceil(threads);
    std::thread::scope(|scope| {
        let y_chunks = y_plane.chunks_mut(per * 2 * w);
        let u_chunks = u_plane.chunks_mut(per * w / 2);
        let v_chunks = v_plane.chunks_mut(per * w / 2);
        for (t, ((yc, uc), vc)) in y_chunks.zip(u_chunks).zip(v_chunks).enumerate() {
            let xs = &xs;
            let ys = &ys;
            scope.spawn(move || {
                let mut top = vec![0u8; w * 3];
                let mut bot = vec![0u8; w * 3];
                for p in 0..(uc.len() / (w / 2)) {
                    let pair = t * per + p;
                    sample_row(src, xs, ys[pair * 2], &mut top);
                    sample_row(src, xs, ys[pair * 2 + 1], &mut bot);
                    let yt = &mut yc[p * 2 * w..p * 2 * w + w];
                    for (d, s) in yt.iter_mut().zip(top.chunks_exact(3)) {
                        *d = y709(s[0] as i32, s[1] as i32, s[2] as i32);
                    }
                    let yb = &mut yc[p * 2 * w + w..p * 2 * w + 2 * w];
                    for (d, s) in yb.iter_mut().zip(bot.chunks_exact(3)) {
                        *d = y709(s[0] as i32, s[1] as i32, s[2] as i32);
                    }
                    let ur = &mut uc[p * w / 2..(p + 1) * w / 2];
                    let vr = &mut vc[p * w / 2..(p + 1) * w / 2];
                    for x in 0..w / 2 {
                        let i = x * 6;
                        let r = (top[i] as i32 + top[i + 3] as i32 + bot[i] as i32 + bot[i + 3] as i32 + 2) >> 2;
                        let g = (top[i + 1] as i32 + top[i + 4] as i32 + bot[i + 1] as i32 + bot[i + 4] as i32 + 2) >> 2;
                        let b = (top[i + 2] as i32 + top[i + 5] as i32 + bot[i + 2] as i32 + bot[i + 5] as i32 + 2) >> 2;
                        ur[x] = (128 + ((-26 * r - 87 * g + 113 * b + 128) >> 8)).clamp(16, 240) as u8;
                        vr[x] = (128 + ((113 * r - 102 * g - 11 * b + 128) >> 8)).clamp(16, 240) as u8;
                    }
                }
            });
        }
    });
    true
}

/// Converts the raw frame to `dw x dh` RGB (for the JPEG fallback path).
pub(crate) fn to_rgb(src: &RawFrame, dw: u32, dh: u32) -> Option<Vec<u8>> {
    if !src.valid() || dw == 0 || dh == 0 {
        return None;
    }
    let w = dw as usize;
    let xs = spans(src.w, dw);
    let ys = spans(src.h, dh);
    let mut out = vec![0u8; w * dh as usize * 3];
    let rows = dh as usize;
    let threads = workers(rows);
    let per = rows.div_ceil(threads);
    std::thread::scope(|scope| {
        for (t, chunk) in out.chunks_mut(per * w * 3).enumerate() {
            let xs = &xs;
            let ys = &ys;
            scope.spawn(move || {
                for (r, row) in chunk.chunks_exact_mut(w * 3).enumerate() {
                    sample_row(src, xs, ys[t * per + r], row);
                }
            });
        }
    });
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, fmt: PixFmt, px: [u8; 4]) -> RawFrame {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            data.extend_from_slice(&px);
        }
        RawFrame { w, h, stride: w as usize * 4, fmt, data }
    }

    #[test]
    fn target_size_keeps_aspect_and_even() {
        assert_eq!(target_size(2560, 1440, 1920), (1920, 1080));
        assert_eq!(target_size(1366, 768, 1920), (1366, 768));
        assert_eq!(target_size(1365, 767, 1920), (1364, 766));
    }

    #[test]
    fn white_and_black_map_to_limited_range() {
        let mut out = Vec::new();
        assert!(to_i420(&solid(64, 32, PixFmt::Rgbx, [255, 255, 255, 255]), 64, 32, &mut out));
        assert!(out[..64 * 32].iter().all(|&y| y == 235));
        assert!(out[64 * 32..].iter().all(|&c| (127..=129).contains(&c)));
        assert!(to_i420(&solid(64, 32, PixFmt::Bgrx, [0, 0, 0, 255]), 32, 16, &mut out));
        assert_eq!(out.len(), 32 * 16 * 3 / 2);
        assert!(out[..32 * 16].iter().all(|&y| y == 16));
    }

    #[test]
    fn channel_order_is_respected() {
        // Pure red: [0,0,255,x] in BGRx. V (Cr) must be high, U (Cb) low.
        let mut out = Vec::new();
        assert!(to_i420(&solid(8, 8, PixFmt::Bgrx, [0, 0, 255, 255]), 8, 8, &mut out));
        let u = out[64];
        let v = out[64 + 16];
        assert!(v > 200 && u < 110, "u={u} v={v}");
        let rgb = to_rgb(&solid(4, 4, PixFmt::Bgrx, [0, 0, 255, 255]), 2, 2).unwrap();
        assert_eq!(&rgb[..3], &[255, 0, 0]);
    }

    #[test]
    fn stride_padding_is_ignored() {
        let (w, h) = (6u32, 4u32);
        let stride = 32;
        let mut data = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let f = RawFrame { w, h, stride, fmt: PixFmt::Rgbx, data };
        let mut out = Vec::new();
        assert!(to_i420(&f, 6, 4, &mut out));
        assert!(out[..24].iter().all(|&y| y == 235));
    }
}
