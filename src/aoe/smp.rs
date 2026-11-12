//! SMP sprites (`*.smp`), the format DE keeps a few old effects in (the building flames among
//! them). Only the main graphic layer is read: per pixel a palette index and a palette number.
use anyhow::{Result, bail};

/// One frame's main graphic, cropped to the rows and columns that have pixels.
pub struct Frame {
    /// Top-left of the cropped picture relative to the hotspot.
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
    /// `(palette number, colour index 0..1024)` per pixel, `None` where transparent.
    pub px: Vec<Option<(u8, u16)>>,
}

fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    match d.get(o..o + 2) {
        Some(b) => Ok(u16::from_le_bytes([b[0], b[1]])),
        None => bail!("smp: truncated at {o}"),
    }
}

fn u32_at(d: &[u8], o: usize) -> Result<usize> {
    match d.get(o..o + 4) {
        Some(b) => Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize),
        None => bail!("smp: truncated at {o}"),
    }
}

pub fn parse(d: &[u8]) -> Result<Vec<Frame>> {
    if d.get(0..4) != Some(b"SMP$") {
        bail!("not an SMP file");
    }
    let n = u32_at(d, 8)?;
    let mut out = Vec::with_capacity(n);
    for f in 0..n {
        let fo = u32_at(d, 64 + 4 * f)?;
        let layers = u32_at(d, fo + 28)?;
        // the main graphic layer of the frame
        let Some(l) = (0..layers).map(|k| fo + 32 + 32 * k).find(|&l| u32_at(d, l + 16).is_ok_and(|t| t == 2)) else {
            out.push(Frame { x: 0, y: 0, w: 0, h: 0, px: Vec::new() });
            continue;
        };
        let (w, h) = (u32_at(d, l)?, u32_at(d, l + 4)?);
        let (hx, hy) = (u32_at(d, l + 8)? as i32, u32_at(d, l + 12)? as i32);
        let (edges, cmds) = (fo + u32_at(d, l + 20)?, fo + u32_at(d, l + 24)?);
        let mut px = vec![None; w * h];
        for r in 0..h {
            let left = u16_at(d, edges + 4 * r)?;
            if left == 0xFFFF {
                continue;
            }
            // each row's commands start where the row table says
            let mut p = fo + u32_at(d, cmds + 4 * r)?;
            let mut x = left as usize;
            loop {
                let Some(&c) = d.get(p) else { bail!("smp: truncated row") };
                p += 1;
                let count = (c >> 2) as usize + 1;
                match c & 3 {
                    0 => x += count,
                    3 => break,
                    _ => {
                        for k in 0..count {
                            let Some(b) = d.get(p + 4 * k..p + 4 * k + 2) else { bail!("smp: truncated pixels") };
                            if x + k < w {
                                px[r * w + x + k] = Some((b[1] >> 2, b[0] as u16 + 256 * (b[1] & 3) as u16));
                            }
                        }
                        p += 4 * count;
                        x += count;
                    }
                }
            }
        }
        // crop away the empty canvas around the picture
        let rows: Vec<usize> = (0..h).filter(|&y| px[y * w..(y + 1) * w].iter().any(Option::is_some)).collect();
        let cols: Vec<usize> = (0..w).filter(|&x| (0..h).any(|y| px[y * w + x].is_some())).collect();
        let (Some(&y0), Some(&y1), Some(&x0), Some(&x1)) = (rows.first(), rows.last(), cols.first(), cols.last()) else {
            out.push(Frame { x: 0, y: 0, w: 0, h: 0, px: Vec::new() });
            continue;
        };
        let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
        let crop = (0..cw * ch).map(|i| px[(y0 + i / cw) * w + x0 + i % cw]).collect();
        out.push(Frame { x: x0 as i32 - hx, y: y0 as i32 - hy, w: cw, h: ch, px: crop });
    }
    Ok(out)
}
