//! Reads the top mip level of a DXT1 `.dds` texture (the AoE2 DE terrain textures).
use anyhow::{bail, Result};

use super::bc::bc1_block;

pub struct Rgba {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 4]>,
}

pub fn read_dxt1(data: &[u8]) -> Result<Rgba> {
    if data.len() < 128 || &data[0..4] != b"DDS " {
        bail!("not a DDS file");
    }
    let h = u32::from_le_bytes(data[12..16].try_into()?) as usize;
    let w = u32::from_le_bytes(data[16..20].try_into()?) as usize;
    if &data[84..88] != b"DXT1" {
        bail!("unsupported DDS pixel format");
    }
    let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
    let body = &data[128..];
    if body.len() < bw * bh * 8 {
        bail!("truncated DDS");
    }
    let mut px = vec![[0u8; 4]; w * h];
    for by in 0..bh {
        for bx in 0..bw {
            for (k, p) in bc1_block(&body[(by * bw + bx) * 8..]).iter().enumerate() {
                let (x, y) = (bx * 4 + k % 4, by * 4 + k / 4);
                if x < w && y < h {
                    px[y * w + x] = *p;
                }
            }
        }
    }
    Ok(Rgba { w, h, px })
}
