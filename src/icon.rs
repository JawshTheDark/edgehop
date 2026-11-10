//! Procedurally drawn app icon: two monitors with an arrow hopping between them.

fn inside_round_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Colour of one sample point (x, y in 0..1), or None if transparent.
fn shade(x: f32, y: f32) -> Option<[f32; 3]> {
    if !inside_round_rect(x, y, 0.0, 0.0, 1.0, 1.0, 0.23) {
        return None;
    }
    const INDIGO: [f32; 3] = [99.0, 102.0, 241.0];
    const VIOLET: [f32; 3] = [139.0, 92.0, 246.0];
    const WHITE: [f32; 3] = [255.0, 255.0, 255.0];
    const AMBER: [f32; 3] = [251.0, 191.0, 36.0];
    let bg = lerp(INDIGO, VIOLET, ((x + y) / 2.0).clamp(0.0, 1.0));
    let mut c = bg;

    // Two monitors: white bezel, tinted screen.
    for (x0, x1) in [(0.10, 0.44), (0.56, 0.90)] {
        if inside_round_rect(x, y, x0, 0.50, x1, 0.76, 0.045) {
            c = WHITE;
            if inside_round_rect(x, y, x0 + 0.035, 0.535, x1 - 0.035, 0.725, 0.02) {
                c = lerp(bg, WHITE, 0.35);
            }
        }
        let mid = (x0 + x1) / 2.0;
        if inside_round_rect(x, y, mid - 0.075, 0.79, mid + 0.075, 0.83, 0.02) {
            c = WHITE;
        }
    }

    // The hop: an arc from the left monitor to the right one...
    let (cx, cy, r, w) = (0.5f32, 0.58f32, 0.29f32, 0.045f32);
    let (dx, dy) = (x - cx, cy - y);
    let d = (dx * dx + dy * dy).sqrt();
    let ang = dy.atan2(dx).to_degrees();
    if (d - r).abs() < w && (32.0..=152.0).contains(&ang) {
        c = AMBER;
    }
    // ...ending in an arrowhead pointing down onto the right monitor.
    let th = 32f32.to_radians();
    let (ex, ey) = (cx + r * th.cos(), cy - r * th.sin());
    let (tx, ty) = (th.sin(), th.cos()); // direction of travel (clockwise)
    let (px, py) = (-ty, tx);
    let tip = (ex + tx * 0.11, ey + ty * 0.11);
    let b1 = (ex + px * 0.09, ey + py * 0.09);
    let b2 = (ex - px * 0.09, ey - py * 0.09);
    let side = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
    let (s1, s2, s3) = (side(b1, tip), side(tip, b2), side(b2, b1));
    if (s1 >= 0.0 && s2 >= 0.0 && s3 >= 0.0) || (s1 <= 0.0 && s2 <= 0.0 && s3 <= 0.0) {
        c = AMBER;
    }
    Some(c)
}

/// RGBA pixels, `size` x `size` (4x4 supersampled for smooth edges).
pub fn rgba(size: u32) -> Vec<u8> {
    const N: u32 = 4;
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    for j in 0..size {
        for i in 0..size {
            let (mut acc, mut hits) = ([0f32; 3], 0u32);
            for sj in 0..N {
                for si in 0..N {
                    let x = (i as f32 + (si as f32 + 0.5) / N as f32) / s;
                    let y = (j as f32 + (sj as f32 + 0.5) / N as f32) / s;
                    if let Some(c) = shade(x, y) {
                        acc = [acc[0] + c[0], acc[1] + c[1], acc[2] + c[2]];
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let o = ((j * size + i) * 4) as usize;
                let h = hits as f32;
                px[o] = (acc[0] / h) as u8;
                px[o + 1] = (acc[1] / h) as u8;
                px[o + 2] = (acc[2] / h) as u8;
                px[o + 3] = (255 * hits / (N * N)) as u8;
            }
        }
    }
    px
}

/// Encode the icon as an (uncompressed-deflate) PNG, used for Linux packaging.
pub fn png(size: u32) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let px = rgba(size);
    let mut raw = Vec::with_capacity(px.len() + size as usize);
    for row in px.chunks((size * 4) as usize) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, blk) in blocks.iter().enumerate() {
        z.push((i == blocks.len() - 1) as u8);
        let n = blk.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(blk);
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
