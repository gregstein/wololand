//! Minimal BC1 (DXT1) and BC4 block decoders.

#[inline]
fn rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u8;
    let g = ((c >> 5) & 63) as u8;
    let b = (c & 31) as u8;
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
}

/// Decode one 8-byte BC1 block into 16 RGBA pixels (row-major 4x4).
pub fn bc1_block(b: &[u8]) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let (p0, p1) = (rgb565(c0), rgb565(c1));
    let mut pal = [[0u8; 4]; 4];
    pal[0] = [p0[0], p0[1], p0[2], 255];
    pal[1] = [p1[0], p1[1], p1[2], 255];
    if c0 > c1 {
        for k in 0..3 {
            pal[2][k] = ((2 * p0[k] as u16 + p1[k] as u16) / 3) as u8;
            pal[3][k] = ((p0[k] as u16 + 2 * p1[k] as u16) / 3) as u8;
        }
        pal[2][3] = 255;
        pal[3][3] = 255;
    } else {
        for k in 0..3 {
            pal[2][k] = ((p0[k] as u16 + p1[k] as u16) / 2) as u8;
        }
        pal[2][3] = 255;
        pal[3] = [0, 0, 0, 0];
    }
    let idx = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    let mut out = [[0u8; 4]; 16];
    for (i, px) in out.iter_mut().enumerate() {
        *px = pal[((idx >> (2 * i)) & 3) as usize];
    }
    out
}

/// Decode one 8-byte BC4 block into 16 single-channel values.
pub fn bc4_block(b: &[u8]) -> [u8; 16] {
    let (r0, r1) = (b[0] as u32, b[1] as u32);
    let mut pal = [0u8; 8];
    pal[0] = r0 as u8;
    pal[1] = r1 as u8;
    if r0 > r1 {
        for i in 1..7u32 {
            pal[(i + 1) as usize] = (((7 - i) * r0 + i * r1) / 7) as u8;
        }
    } else {
        for i in 1..5u32 {
            pal[(i + 1) as usize] = (((5 - i) * r0 + i * r1) / 5) as u8;
        }
        pal[6] = 0;
        pal[7] = 255;
    }
    let mut bits: u64 = 0;
    for (k, &byte) in b[2..8].iter().enumerate() {
        bits |= (byte as u64) << (8 * k);
    }
    let mut out = [0u8; 16];
    for (i, v) in out.iter_mut().enumerate() {
        *v = pal[((bits >> (3 * i)) & 7) as usize];
    }
    out
}
