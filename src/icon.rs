//! Procedurally drawn app icon: two monitors joined by a link.

fn inside_round_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

/// RGBA pixels, `size` x `size`.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let bg = [37u8, 99, 235, 255]; // blue
    let screen = [255u8, 255, 255, 255];
    let link = [250u8, 204, 21, 255]; // amber
    for j in 0..size {
        for i in 0..size {
            let (x, y) = ((i as f32 + 0.5) / s, (j as f32 + 0.5) / s);
            let mut c = [0u8; 4];
            if inside_round_rect(x, y, 0.0, 0.0, 1.0, 1.0, 0.22) {
                c = bg;
            }
            // left and right monitors
            if inside_round_rect(x, y, 0.10, 0.24, 0.44, 0.62, 0.04) || inside_round_rect(x, y, 0.56, 0.24, 0.90, 0.62, 0.04) {
                c = screen;
            }
            // stands
            if (inside_round_rect(x, y, 0.24, 0.62, 0.30, 0.72, 0.0) || inside_round_rect(x, y, 0.70, 0.62, 0.76, 0.72, 0.0))
                || (inside_round_rect(x, y, 0.17, 0.72, 0.37, 0.77, 0.02) || inside_round_rect(x, y, 0.63, 0.72, 0.83, 0.77, 0.02))
            {
                c = screen;
            }
            // link between them
            if inside_round_rect(x, y, 0.36, 0.39, 0.64, 0.47, 0.04) {
                c = link;
            }
            let o = ((j * size + i) * 4) as usize;
            px[o..o + 4].copy_from_slice(&c);
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
