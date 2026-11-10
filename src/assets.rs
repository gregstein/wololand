//! Loads sprites and terrain textures straight from the user's own AoE2 DE install.
//! Nothing is copied to disk: assets are decoded in memory when a map theme is loaded.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::aoe::{dds, sld};

/// Strength of the baked SLD shadow layer (0..1).
const SHADOW: f32 = 0.55;

#[derive(Clone)]
pub struct Game {
    root: PathBuf,
    /// Use the `_x2` (UHD) sprites instead of `_x1`.
    pub uhd: bool,
}

/// A composed sprite frame: straight-alpha RGBA, `(ox, oy)` = top-left relative to the hotspot.
#[derive(Clone)]
pub struct Img {
    pub ox: i32,
    pub oy: i32,
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 4]>,
}

/// Square, power-of-two RGBA texture.
pub struct Tex {
    pub n: usize,
    pub px: Vec<[u8; 4]>,
}

fn has_graphics(p: &Path) -> bool {
    p.join("resources/_common/drs/graphics").is_dir()
}

/// Steam library folders listed in `libraryfolders.vdf`.
fn steam_libraries() -> Vec<PathBuf> {
    let mut libs = Vec::new();
    let steam = windows_registry::CURRENT_USER
        .open(r"Software\Valve\Steam")
        .and_then(|k| k.get_string("SteamPath"))
        .ok();
    let Some(steam) = steam else { return libs };
    let steam = PathBuf::from(steam.replace('/', "\\"));
    libs.push(steam.clone());
    if let Ok(vdf) = std::fs::read_to_string(steam.join("steamapps/libraryfolders.vdf")) {
        for line in vdf.lines() {
            let parts: Vec<&str> = line.split('"').collect();
            if parts.len() >= 4 && parts[1] == "path" {
                libs.push(PathBuf::from(parts[3].replace("\\\\", "\\")));
            }
        }
    }
    libs
}

impl Game {
    pub fn locate(custom: Option<&str>) -> Option<Game> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(c) = custom.filter(|c| !c.is_empty()) {
            candidates.push(PathBuf::from(c));
        }
        for lib in steam_libraries() {
            candidates.push(lib.join("steamapps/common/AoE2DE"));
        }
        candidates.push(PathBuf::from(r"C:\Program Files (x86)\Steam\steamapps\common\AoE2DE"));
        candidates.push(PathBuf::from(r"C:\XboxGames\Age of Empires II- Definitive Edition\Content"));
        candidates.into_iter().find(|p| has_graphics(p)).map(|root| Game { root, uhd: false })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// All frames of `<name>_x1.sld` (or `_x2`), shadow composed under the main graphic.
    /// `scale` resamples to the requested on-screen size.
    pub fn sprite(&self, name: &str, scale: f32) -> Result<Vec<Img>> {
        let suffix = if self.uhd { "x2" } else { "x1" };
        let path = self.root.join(format!("resources/_common/drs/graphics/{name}_{suffix}.sld"));
        let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let frames = sld::parse(&data).with_context(|| format!("decoding {name}"))?;
        let scale = if self.uhd { scale / 2.0 } else { scale };
        Ok(frames.iter().map(|f| compose(f).scaled(scale)).collect())
    }

    /// Terrain texture `name` (e.g. `g_gr2`), box-filtered down to `n`x`n`.
    pub fn texture(&self, name: &str, n: usize) -> Result<Tex> {
        let path = self.root.join(format!("resources/_common/terrain/textures/2x/{name}.dds"));
        let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let t = dds::read_dxt1(&data)?;
        let f = (t.w / n).max(1);
        let n = t.w / f;
        let mut px = vec![[0u8; 4]; n * n];
        for y in 0..n {
            for x in 0..n {
                let mut acc = [0u32; 4];
                for dy in 0..f {
                    for dx in 0..f {
                        let p = t.px[(y * f + dy) * t.w + x * f + dx];
                        for k in 0..4 {
                            acc[k] += p[k] as u32;
                        }
                    }
                }
                let d = (f * f) as u32;
                px[y * n + x] = [(acc[0] / d) as u8, (acc[1] / d) as u8, (acc[2] / d) as u8, 255];
            }
        }
        Ok(Tex { n, px })
    }
}

fn compose(f: &sld::Frame) -> Img {
    let (hx, hy) = (f.hotspot_x, f.hotspot_y);
    let mut x0 = i32::MAX;
    let mut y0 = i32::MAX;
    let mut x1 = i32::MIN;
    let mut y1 = i32::MIN;
    if let Some(m) = &f.main {
        x0 = x0.min(m.x);
        y0 = y0.min(m.y);
        x1 = x1.max(m.x + m.w as i32);
        y1 = y1.max(m.y + m.h as i32);
    }
    if let Some(s) = &f.shadow {
        x0 = x0.min(s.x);
        y0 = y0.min(s.y);
        x1 = x1.max(s.x + s.w as i32);
        y1 = y1.max(s.y + s.h as i32);
    }
    if x0 >= x1 || y0 >= y1 {
        return Img { ox: 0, oy: 0, w: 0, h: 0, px: Vec::new() };
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut px = vec![[0u8; 4]; w * h];
    if let Some(s) = &f.shadow {
        for y in 0..s.h {
            let row = (s.y - y0) as usize + y;
            for x in 0..s.w {
                let a = s.px[y * s.w + x];
                if a > 0 {
                    px[row * w + (s.x - x0) as usize + x] = [0, 0, 0, (a as f32 * SHADOW) as u8];
                }
            }
        }
    }
    if let Some(m) = &f.main {
        for y in 0..m.h {
            let row = (m.y - y0) as usize + y;
            for x in 0..m.w {
                let p = m.px[y * m.w + x];
                if p[3] > 0 {
                    px[row * w + (m.x - x0) as usize + x] = p;
                }
            }
        }
    }
    Img { ox: x0 - hx, oy: y0 - hy, w, h, px }
}

impl Img {
    /// Bilinear resample (premultiplied) by `s`; returns a clone when `s` is ~1.
    pub fn scaled(self, s: f32) -> Img {
        if (s - 1.0).abs() < 0.01 || self.w == 0 {
            return self;
        }
        let nw = ((self.w as f32 * s).round() as usize).max(1);
        let nh = ((self.h as f32 * s).round() as usize).max(1);
        let get = |x: i32, y: i32| -> [f32; 4] {
            if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
                return [0.0; 4];
            }
            let p = self.px[y as usize * self.w + x as usize];
            let a = p[3] as f32 / 255.0;
            [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, p[3] as f32]
        };
        let mut px = vec![[0u8; 4]; nw * nh];
        for y in 0..nh {
            let fy = (y as f32 + 0.5) / s - 0.5;
            let (iy, ty) = (fy.floor() as i32, fy - fy.floor());
            for x in 0..nw {
                let fx = (x as f32 + 0.5) / s - 0.5;
                let (ix, tx) = (fx.floor() as i32, fx - fx.floor());
                let (a, b, c, d) = (get(ix, iy), get(ix + 1, iy), get(ix, iy + 1), get(ix + 1, iy + 1));
                let mut o = [0f32; 4];
                for k in 0..4 {
                    o[k] = (a[k] * (1.0 - tx) + b[k] * tx) * (1.0 - ty) + (c[k] * (1.0 - tx) + d[k] * tx) * ty;
                }
                if o[3] > 0.5 {
                    let inv = 255.0 / o[3];
                    px[y * nw + x] = [(o[0] * inv) as u8, (o[1] * inv) as u8, (o[2] * inv) as u8, o[3] as u8];
                }
            }
        }
        Img { ox: (self.ox as f32 * s).round() as i32, oy: (self.oy as f32 * s).round() as i32, w: nw, h: nh, px }
    }

    /// BGRA bytes for GPUI.
    pub fn bgra(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.w * self.h * 4);
        for p in &self.px {
            v.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
        v
    }
}
