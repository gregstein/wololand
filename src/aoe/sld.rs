//! Reader for Age of Empires II: Definitive Edition `.sld` sprite files.
//!
//! Layout (little endian):
//!   header : "SLDX", u16 version, u16 frame count, u16, u16, u32
//!   frame  : u16 canvas_w, u16 canvas_h, u16 hotspot_x, u16 hotspot_y, u8 layer mask, u8, u16 index
//!   layer  : u32 length (includes itself; the next layer starts 4-byte aligned) + body
//!   main / shadow body : u16 x1, y1, x2, y2, u8 flags, u8, u16 cmd count, cmds (skip u8, draw u8), blocks
//!   other bodies       : u8 flags, u8, u16 cmd count, cmds, blocks (same grid as the main layer)
//! The main layer is BC1, every other layer is BC4. Flag 0x80: skipped blocks keep the previous frame.
use anyhow::{bail, Context, Result};

use super::bc::{bc1_block, bc4_block};

pub struct Frame {
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    /// Main graphic (RGBA), placed at (`x`, `y`) inside the frame canvas.
    pub main: Option<Layer<[u8; 4]>>,
    /// Shadow opacity, placed inside the frame canvas.
    pub shadow: Option<Layer<u8>>,
}

#[derive(Clone)]
pub struct Layer<T: Copy> {
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
    pub px: Vec<T>,
}

struct Cursor<'a> {
    d: &'a [u8],
    p: usize,
}

impl Cursor<'_> {
    fn u8(&mut self) -> Result<u8> {
        let v = *self.d.get(self.p).context("unexpected end of SLD")?;
        self.p += 1;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes([self.u8()?, self.u8()?]))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes([self.u8()?, self.u8()?, self.u8()?, self.u8()?]))
    }
}

/// Walk the command list and hand every drawn 8-byte block to `put(block_index, bytes)`.
fn walk_blocks(c: &mut Cursor, total: usize, mut put: impl FnMut(usize, &[u8])) -> Result<()> {
    let n = c.u16()? as usize;
    let mut cmds = Vec::with_capacity(n);
    for _ in 0..n {
        cmds.push((c.u8()? as usize, c.u8()? as usize));
    }
    let mut bi = 0usize;
    for (skip, draw) in cmds {
        bi += skip;
        for _ in 0..draw {
            let blk = c.d.get(c.p..c.p + 8).context("SLD block out of range")?;
            if bi < total {
                put(bi, blk);
            }
            c.p += 8;
            bi += 1;
        }
    }
    Ok(())
}

fn decode_layer<T: Copy + Default, const N: usize>(
    c: &mut Cursor,
    prev: &Option<Layer<T>>,
    dec: impl Fn(&[u8]) -> [T; N],
) -> Result<Layer<T>> {
    let x1 = c.u16()? as i32;
    let y1 = c.u16()? as i32;
    let x2 = c.u16()? as i32;
    let y2 = c.u16()? as i32;
    let flags = c.u8()?;
    let _ = c.u8()?;
    let bw = ((x2 - x1).max(0) as usize).div_ceil(4);
    let bh = ((y2 - y1).max(0) as usize).div_ceil(4);
    let (gw, gh) = (bw * 4, bh * 4);
    let mut px = match prev {
        Some(p) if flags & 0x80 != 0 && p.w == gw && p.h == gh => p.px.clone(),
        _ => vec![T::default(); gw * gh],
    };
    walk_blocks(c, bw * bh, |bi, blk| {
        let (bx, by) = (bi % bw, bi / bw);
        for (k, p) in dec(blk).iter().enumerate() {
            px[(by * 4 + k / 4) * gw + bx * 4 + k % 4] = *p;
        }
    })?;
    Ok(Layer { x: x1, y: y1, w: gw, h: gh, px })
}

pub fn parse(data: &[u8]) -> Result<Vec<Frame>> {
    if data.len() < 16 || &data[0..4] != b"SLDX" {
        bail!("not an SLD file");
    }
    let mut c = Cursor { d: data, p: 6 };
    let nframes = c.u16()? as usize;
    c.p = 16;
    let mut frames = Vec::with_capacity(nframes);
    let mut prev_main: Option<Layer<[u8; 4]>> = None;
    let mut prev_shadow: Option<Layer<u8>> = None;
    for _ in 0..nframes {
        let _canvas_w = c.u16()?;
        let _canvas_h = c.u16()?;
        let hotspot_x = c.u16()? as i32;
        let hotspot_y = c.u16()? as i32;
        let mask = c.u8()?;
        c.p += 3;
        let (mut main, mut shadow) = (None, None);
        for bit in 0..5 {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let start = c.p;
            let len = c.u32()? as usize;
            let next = start + ((len + 3) & !3);
            match bit {
                0 => {
                    let l = decode_layer(&mut c, &prev_main, bc1_block)?;
                    prev_main = Some(l.clone());
                    main = Some(l);
                }
                1 => {
                    let l = decode_layer(&mut c, &prev_shadow, bc4_block)?;
                    prev_shadow = Some(l.clone());
                    shadow = Some(l);
                }
                _ => {} // outline, damage and player-colour masks are not needed
            }
            c.p = next;
        }
        frames.push(Frame { hotspot_x, hotspot_y, main, shadow });
    }
    Ok(frames)
}
