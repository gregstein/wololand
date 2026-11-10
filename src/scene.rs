//! Builds a map-themed wallpaper around the desktop icons.
//!
//! The icon rectangles become a signed distance field (negative inside an icon cluster). Each
//! theme turns that field plus some noise into terrain layers and object placement, all baked
//! into one RGBA image on a background thread. Only that image and a handful of animated
//! sprites are drawn per frame, which keeps the wallpaper cheap to run.
use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::assets::{Game, Img, Tex};
use crate::noise::{fbm, hash01, smoothstep, value, Rng};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    Arabia,
    Arena,
    BlackForest,
    Islands,
}

impl Theme {
    pub const ALL: [Theme; 4] = [Theme::Arabia, Theme::Arena, Theme::BlackForest, Theme::Islands];

    pub fn name(self) -> &'static str {
        match self {
            Theme::Arabia => "Arabia",
            Theme::Arena => "Arena",
            Theme::BlackForest => "Black Forest",
            Theme::Islands => "Islands",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Theme::Arabia => "Dry open grassland, scattered woodlines, gold, stone and roaming herds.",
            Theme::Arena => "Your icons walled in by stone, a ring of forest outside the gates.",
            Theme::BlackForest => "Dense dark pine forest with clearings around your icons.",
            Theme::Islands => "Every icon cluster is an island; ships sail between them.",
        }
    }

    /// Terrain textures used by the theme, in layer order.
    fn textures(self) -> &'static [&'static str] {
        match self {
            Theme::Arabia => &["g_gr3", "g_gr7", "g_gr4", "g_for", "g_des"],
            Theme::Arena => &["g_gr2", "g_gr6", "g_fo2", "g_gr4", "g_grs"],
            Theme::BlackForest => &["g_for", "g_fo2", "g_grs", "g_r01", "g_underbrush_leaves"],
            Theme::Islands => &["g_gr2", "g_gr6", "g_bch", "g_sh2"],
        }
    }

    /// Static object sprites used by the theme.
    fn sprites(self) -> &'static [&'static str] {
        match self {
            Theme::Arabia => &["n_tree_oak", "n_tree_palm", "n_mine_gold", "n_mine_stone", "n_forage_bush", "n_plant_grass_dry"],
            Theme::Arena => &["n_tree_oak", "n_tree_pine", "b_west_wall_stone", "n_mine_gold", "n_mine_stone", "n_forage_bush", "n_plant_grass_green"],
            Theme::BlackForest => &["n_tree_pine", "n_tree_oak", "n_tree_dead", "n_plant_grass_green"],
            Theme::Islands => &["n_tree_palm", "n_tree_oak", "n_plant_grass_green", "n_mine_gold"],
        }
    }

    pub fn has_water(self) -> bool {
        self == Theme::Islands
    }
}

/// Everything a theme needs, decoded once per theme switch and shared with the builder thread.
pub struct ThemeKit {
    pub theme: Theme,
    /// Sprite draw scale relative to the game's native size (follows display scaling).
    pub ds: f32,
    /// Terrain textures are decoded per build and dropped afterwards (they are the largest
    /// assets and are only needed while baking).
    game: Game,
    sprites: HashMap<&'static str, Vec<Img>>,
}

impl ThemeKit {
    pub fn load(game: &Game, theme: Theme, ds: f32) -> Result<ThemeKit> {
        let mut sprites = HashMap::new();
        for &n in theme.sprites() {
            let mut frames = game.sprite(n, ds)?;
            // the tree sheets mix green, autumn and bare variants: keep the green ones
            let keep: Option<Vec<usize>> = match n {
                "n_tree_oak" => Some((0..=8).chain(12..=14).chain(27..=41).collect()),
                "n_tree_pine" => Some((3..=14).chain(21..=23).collect()),
                _ => None,
            };
            if let Some(keep) = keep {
                let all = std::mem::take(&mut frames);
                frames = all.into_iter().enumerate().filter(|(i, _)| keep.contains(i)).map(|(_, f)| f).collect();
            }
            if theme == Theme::BlackForest {
                for f in &mut frames {
                    for p in &mut f.px {
                        for c in &mut p[..3] {
                            *c = (*c as f32 * 0.8) as u8;
                        }
                    }
                }
            }
            sprites.insert(n, frames);
        }
        let kit = ThemeKit { theme, ds, game: game.clone(), sprites };
        kit.textures()?; // fail early if the install is incomplete
        Ok(kit)
    }

    fn textures(&self) -> Result<(Vec<Tex>, Option<Tex>)> {
        let tex = self.theme.textures().iter().map(|n| self.game.texture(n, 1024)).collect::<Result<Vec<_>>>()?;
        let water = if self.theme.has_water() { Some(self.game.texture("g_wtr", 512)?) } else { None };
        Ok((tex, water))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Extra rounding radius (decorative shapes); 0 for icons.
    pub pad: f32,
}

/// Coarse classification grid used by the animated units.
pub const CELL: f32 = 8.0;
pub const OPEN: u8 = 1;
pub const WATER: u8 = 2;
/// Land units only roam within this distance (unscaled px) of the icon area.
const ROAM: f32 = 230.0;
pub const DEEP: u8 = 3;

pub struct Scene {
    pub w: usize,
    pub h: usize,
    /// Straight-alpha RGBA ground layer (transparent where open water shows through).
    pub ground: Vec<[u8; 4]>,
    /// Two periodic water tiles (base + translucent shimmer), when the theme has water.
    pub water: Option<(usize, usize, Vec<[u8; 4]>, Vec<[u8; 4]>)>,
    pub gw: usize,
    pub gh: usize,
    pub cells: Vec<u8>,
    /// Signed distance to the icon clusters per cell (negative inside).
    pub dist: Vec<f32>,
}

impl Scene {
    pub fn cell(&self, x: f32, y: f32) -> u8 {
        let (cx, cy) = ((x / CELL) as isize, (y / CELL) as isize);
        if cx < 0 || cy < 0 || cx >= self.gw as isize || cy >= self.gh as isize {
            return 0;
        }
        self.cells[cy as usize * self.gw + cx as usize]
    }
    pub fn dist_at(&self, x: f32, y: f32) -> f32 {
        let cx = ((x / CELL) as usize).min(self.gw - 1);
        let cy = ((y / CELL) as usize).min(self.gh - 1);
        self.dist[cy * self.gw + cx]
    }
}

// ------------------------------------------------------------------------------------- field

struct Field {
    gw: usize,
    gh: usize,
    d: Vec<f32>,
    n1: Vec<f32>,
    n2: Vec<f32>,
}

impl Field {
    #[inline]
    fn sample(&self, a: &[f32], x: f32, y: f32) -> f32 {
        let fx = (x / CELL - 0.5).clamp(0.0, self.gw as f32 - 1.001);
        let fy = (y / CELL - 0.5).clamp(0.0, self.gh as f32 - 1.001);
        let (ix, iy) = (fx as usize, fy as usize);
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let i = iy * self.gw + ix;
        let (a0, a1, a2, a3) = (a[i], a[i + 1], a[i + self.gw], a[i + self.gw + 1]);
        (a0 + (a1 - a0) * tx) * (1.0 - ty) + (a2 + (a3 - a2) * tx) * ty
    }
}

/// Signed distance from (x, y) to a rectangle (negative inside).
#[inline]
fn rect_sd(r: &Rect, x: f32, y: f32) -> f32 {
    let (cx, cy) = ((r.x0 + r.x1) * 0.5, (r.y0 + r.y1) * 0.5);
    let (hw, hh) = ((r.x1 - r.x0) * 0.5, (r.y1 - r.y0) * 0.5);
    let (dx, dy) = ((x - cx).abs() - hw, (y - cy).abs() - hh);
    let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
    outside + dx.max(dy).min(0.0)
}

/// Smooth union of all rectangles (expanded by `margin`), with noise-warped edges.
fn build_field(w: usize, h: usize, rects: &[Rect], margin: f32, warp: f32, blend: f32, seed: u32, ds: f32) -> Field {
    let gw = (w as f32 / CELL).ceil() as usize + 1;
    let gh = (h as f32 / CELL).ceil() as usize + 1;
    let mut d = vec![0f32; gw * gh];
    let mut n1 = vec![0f32; gw * gh];
    let mut n2 = vec![0f32; gw * gh];
    let f = 1.0 / (260.0 * ds);
    for gy in 0..gh {
        for gx in 0..gw {
            let (x, y) = ((gx as f32 + 0.5) * CELL, (gy as f32 + 0.5) * CELL);
            let i = gy * gw + gx;
            let wx = (fbm(x * f * 1.7, y * f * 1.7, seed ^ 11) - 0.5) * warp;
            let wy = (fbm(x * f * 1.7, y * f * 1.7, seed ^ 23) - 0.5) * warp;
            let (qx, qy) = (x + wx, y + wy);
            // log-sum-exp soft minimum merges nearby icons into one organic blob
            let mut acc = 0f32;
            let mut best = f32::MAX;
            for r in rects {
                let sd = rect_sd(r, qx, qy) - margin - r.pad;
                best = best.min(sd);
                acc += (-sd / blend).exp();
            }
            d[i] = if rects.is_empty() { 1e5 } else if acc > 0.0 { (-blend * acc.ln()).min(best) } else { best };
            n1[i] = fbm(x * f, y * f, seed ^ 0x51);
            n2[i] = fbm(x * f * 0.8, y * f * 0.8, seed ^ 0x77);
        }
    }
    Field { gw, gh, d, n1, n2 }
}

// --------------------------------------------------------------------------------- terrain

/// Isometric texture lookup: the texture repeats along the two diagonal tile axes like the
/// in-game terrain does. `p` is the screen period of the texture in pixels.
#[inline]
fn iso(t: &Tex, x: i32, y: i32, p: i32, ox: i32) -> [u8; 4] {
    let n = t.n as i32;
    let u = ((x + 2 * y + ox) * n).div_euclid(p).rem_euclid(n);
    let v = ((2 * y - x + ox * 3) * n).div_euclid(p).rem_euclid(n);
    t.px[(v * n + u) as usize]
}

#[inline]
fn mix(a: [f32; 3], b: [u8; 4], t: f32) -> [f32; 3] {
    [a[0] + (b[0] as f32 - a[0]) * t, a[1] + (b[1] as f32 - a[1]) * t, a[2] + (b[2] as f32 - a[2]) * t]
}

struct Ctx<'a> {
    kit: &'a ThemeKit,
    tex: &'a [Tex],
    f: &'a Field,
    seed: u32,
    ds: f32,
    /// Arena: radius of the walled area around each cluster.
    wall_r: f32,
}

impl Ctx<'_> {
    /// Forest density mask in [0, 1] at a point (same function drives trees and forest floor).
    fn forest(&self, d: f32, n1: f32, n2: f32) -> f32 {
        let ds = self.ds;
        match self.kit.theme {
            Theme::Arabia => smoothstep(0.635, 0.66, n2) * smoothstep(140.0 * ds, 200.0 * ds, d),
            Theme::Arena => {
                // a ring of forest outside the walls, open land in the middle of the map
                let band = (n1 - 0.5) * 160.0 * ds;
                smoothstep(self.wall_r + 50.0 * ds, self.wall_r + 90.0 * ds, d)
                    * (1.0 - smoothstep(self.wall_r + 420.0 * ds, self.wall_r + 480.0 * ds, d + band))
            }
            Theme::BlackForest => {
                let clearing = smoothstep(70.0 * ds, 120.0 * ds, d + (n1 - 0.5) * 120.0 * ds);
                let path = smoothstep(0.03, 0.055, (n2 - 0.5).abs());
                clearing * path
            }
            Theme::Islands => 0.0,
        }
    }

    /// One ground pixel (straight RGBA).
    fn pixel(&self, x: i32, y: i32) -> [u8; 4] {
        let (fx, fy) = (x as f32, y as f32);
        let ds = self.ds;
        let d = self.f.sample(&self.f.d, fx, fy);
        let n1 = self.f.sample(&self.f.n1, fx, fy);
        let n2 = self.f.sample(&self.f.n2, fx, fy);
        let fine = value(fx / (9.0 * ds), fy / (9.0 * ds), self.seed ^ 5) - 0.5;
        let t = self.tex;
        let p = (1843.0 * ds).round().max(64.0) as i32;
        let s = self.seed as i32 & 0xffff;
        let tex = |i: usize| iso(&t[i], x, y, p, s.wrapping_mul(i as i32 + 1) & 0x3fff);
        let base = tex(0);
        let mut c = [base[0] as f32, base[1] as f32, base[2] as f32];
        let mut alpha = 1.0f32;
        let forest = self.forest(d, n1, n2);
        match self.kit.theme {
            Theme::Arabia => {
                c = mix(c, tex(1), smoothstep(0.44, 0.58, n1 + fine * 0.12));
                c = mix(c, tex(2), smoothstep(0.60, 0.70, n1 + fine * 0.15) * 0.9);
                // trampled earth around the "town centre" (the icons)
                c = mix(c, tex(2), smoothstep(40.0 * ds, -20.0 * ds, d + fine * 40.0 * ds) * 0.3);
                c = mix(c, tex(4), smoothstep(0.70, 0.78, n2 + fine * 0.1) * smoothstep(60.0 * ds, 120.0 * ds, d) * 0.8);
                c = mix(c, tex(3), smoothstep(0.15, 0.6, forest + fine * 0.3));
            }
            Theme::Arena => {
                c = mix(c, tex(1), smoothstep(0.45, 0.62, n1 + fine * 0.12) * 0.8);
                // worn ground just inside the walls
                let ring = 1.0 - smoothstep(14.0 * ds, 40.0 * ds, (d - self.wall_r).abs() + fine * 20.0 * ds);
                c = mix(c, tex(3), ring * 0.6);
                c = mix(c, tex(4), smoothstep(self.wall_r + 30.0 * ds, self.wall_r + 70.0 * ds, d) * 0.7);
                c = mix(c, tex(2), smoothstep(0.2, 0.7, forest + fine * 0.3));
            }
            Theme::BlackForest => {
                c = mix(c, tex(1), smoothstep(0.45, 0.6, n1 + fine * 0.1) * 0.7);
                c = mix(c, tex(4), smoothstep(0.62, 0.72, n2 + fine * 0.1) * forest * 0.6);
                let open = 1.0 - smoothstep(0.25, 0.75, forest + fine * 0.35);
                c = mix(c, tex(2), open);
                c = mix(c, tex(3), open * smoothstep(0.45, 0.6, n1) * 0.6);
                // the forest floor is darker than in the game to read as "Black" Forest
                let k = 1.0 - forest * 0.22;
                c = [c[0] * k, c[1] * k, c[2] * k];
            }
            Theme::Islands => {
                let dn = d + fine * 14.0 * ds + (n1 - 0.5) * 30.0 * ds;
                c = mix(c, tex(1), smoothstep(0.45, 0.6, n1 + fine * 0.1) * 0.7);
                let beach_end = 26.0 * ds;
                c = mix(c, tex(2), smoothstep(-6.0 * ds, 6.0 * ds, dn));
                if dn > beach_end - 6.0 * ds {
                    // shallows fade out over the water texture, deep water darkens
                    let sh = tex(3);
                    let land = 1.0 - smoothstep(beach_end - 6.0 * ds, beach_end + 4.0 * ds, dn);
                    let shallow = 1.0 - smoothstep(beach_end, beach_end + 70.0 * ds, dn);
                    let deep = smoothstep(beach_end + 40.0 * ds, beach_end + 260.0 * ds, dn);
                    let sa = shallow * 0.85;
                    let ta = deep * 0.45;
                    // composite: deep tint, then shallows, then land on top
                    let mut col = [10.0f32, 34.0, 58.0];
                    let mut a = ta;
                    col = [
                        (sh[0] as f32 * sa + col[0] * a * (1.0 - sa)) / (sa + a * (1.0 - sa)).max(1e-4),
                        (sh[1] as f32 * sa + col[1] * a * (1.0 - sa)) / (sa + a * (1.0 - sa)).max(1e-4),
                        (sh[2] as f32 * sa + col[2] * a * (1.0 - sa)) / (sa + a * (1.0 - sa)).max(1e-4),
                    ];
                    a = sa + a * (1.0 - sa);
                    let out_a = land + a * (1.0 - land);
                    for k in 0..3 {
                        c[k] = (c[k] * land + col[k] * a * (1.0 - land)) / out_a.max(1e-4);
                    }
                    alpha = out_a;
                }
            }
        }
        [c[0].clamp(0.0, 255.0) as u8, c[1].clamp(0.0, 255.0) as u8, c[2].clamp(0.0, 255.0) as u8, (alpha * 255.0) as u8]
    }
}

// --------------------------------------------------------------------------------- objects

struct Obj {
    x: i32,
    y: i32,
    set: &'static str,
    k: usize,
}

fn overlaps_icons(img: &Img, x: f32, y: f32, icons: &[Rect], pad: f32) -> bool {
    let (x0, y0) = (x + img.ox as f32, y + img.oy as f32);
    let (x1, y1) = (x0 + img.w as f32, y0 + img.h as f32);
    icons.iter().any(|r| x1 > r.x0 - pad && x0 < r.x1 + pad && y1 > r.y0 - pad && y0 < r.y1 + pad)
}

/// Iterate the isometric tile grid covering the screen: yields (i, j, centre x, centre y).
fn tiles(w: usize, h: usize, tw: f32, th: f32, mut f: impl FnMut(i32, i32, f32, f32)) {
    let n = ((w as f32 / tw) + (h as f32 / th) + 4.0) as i32;
    for i in -2..n {
        for j in -n..n {
            let x = (i - j) as f32 * tw * 0.5;
            let y = (i + j) as f32 * th * 0.5;
            if x < -tw || y < -th || x > w as f32 + tw || y > h as f32 + th * 4.0 {
                continue;
            }
            f(i, j, x, y);
        }
    }
}

/// Blit straight-alpha sprite onto straight-alpha target, clipped to rows `[r0, r1)`.
fn blit(dst: &mut [[u8; 4]], w: usize, r0: usize, r1: usize, o: &Obj, img: &Img) {
    let x0 = o.x + img.ox;
    let y0 = o.y + img.oy;
    let sy0 = (r0 as i32 - y0).max(0) as usize;
    let sy1 = ((r1 as i32 - y0).max(0) as usize).min(img.h);
    let sx0 = (-x0).max(0) as usize;
    let sx1 = ((w as i32 - x0).max(0) as usize).min(img.w);
    for sy in sy0..sy1 {
        let dy = (y0 + sy as i32) as usize - r0;
        for sx in sx0..sx1 {
            let s = img.px[sy * img.w + sx];
            if s[3] == 0 {
                continue;
            }
            let di = dy * w + (x0 + sx as i32) as usize;
            let dp = &mut dst[di];
            if s[3] == 255 {
                *dp = s;
                continue;
            }
            let sa = s[3] as f32 / 255.0;
            let da = dp[3] as f32 / 255.0;
            let oa = sa + da * (1.0 - sa);
            for k in 0..3 {
                dp[k] = ((s[k] as f32 * sa + dp[k] as f32 * da * (1.0 - sa)) / oa.max(1e-4)) as u8;
            }
            dp[3] = (oa * 255.0) as u8;
        }
    }
}

// ----------------------------------------------------------------------------------- build

pub fn build(kit: &ThemeKit, w: usize, h: usize, icons: &[Rect], seed: u64) -> Scene {
    let ds = kit.ds;
    let theme = kit.theme;
    let seed32 = seed as u32 ^ (seed >> 32) as u32;
    let mut rng = Rng::new(seed ^ 0x5eed);

    // Islands get extra, decorative islands in large empty areas; an empty desktop gets a
    // single feature in the middle for the other maps.
    let mut shapes: Vec<Rect> = icons.to_vec();
    if theme == Theme::Islands {
        let count = 2 + rng.below(3);
        for _ in 0..count {
            let mut best: Option<(f32, f32, f32)> = None;
            for _ in 0..40 {
                let (x, y) = (rng.range(0.1, 0.9) * w as f32, rng.range(0.12, 0.88) * h as f32);
                let dmin = shapes.iter().map(|r| rect_sd(r, x, y)).fold(f32::MAX, f32::min);
                if best.is_none_or(|b| dmin > b.2) {
                    best = Some((x, y, dmin));
                }
            }
            if let Some((x, y, dmin)) = best.filter(|b| b.2 > 260.0 * ds) {
                let r = (dmin * 0.35).min(170.0 * ds) * rng.range(0.6, 1.0);
                shapes.push(Rect { x0: x - r * 0.45, y0: y - r * 0.12, x1: x + r * 0.45, y1: y + r * 0.12, pad: r * 0.5 });
            }
        }
    } else if shapes.is_empty() {
        let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
        shapes.push(Rect { x0: cx - 90.0 * ds, y0: cy - 60.0 * ds, x1: cx + 90.0 * ds, y1: cy + 60.0 * ds, pad: 0.0 });
    }
    let (margin, warp, blend) = match theme {
        Theme::Islands => (14.0 * ds, 110.0 * ds, 22.0 * ds),
        Theme::Arena => (24.0 * ds, 50.0 * ds, 40.0 * ds),
        _ => (20.0 * ds, 90.0 * ds, 36.0 * ds),
    };
    let field = build_field(w, h, &shapes, margin, warp, blend, seed32, ds);
    let (tex, water_tex) = kit.textures().unwrap_or_default();
    if tex.len() < kit.theme.textures().len() {
        return Scene { w, h, ground: vec![[40, 60, 30, 255]; w * h], water: None, gw: field.gw, gh: field.gh, cells: vec![0; field.gw * field.gh], dist: field.d };
    }
    let ctx = Ctx { kit, tex: &tex, f: &field, seed: seed32, ds, wall_r: 200.0 * ds };

    // ---- terrain, split across cores
    let mut ground = vec![[0u8; 4]; w * h];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let band = h.div_ceil(threads);
    std::thread::scope(|sc| {
        for (bi, chunk) in ground.chunks_mut(band * w).enumerate() {
            let ctx = &ctx;
            sc.spawn(move || {
                let y0 = bi * band;
                for (k, px) in chunk.iter_mut().enumerate() {
                    *px = ctx.pixel((k % w) as i32, (y0 + k / w) as i32);
                }
            });
        }
    });

    // ---- objects on the isometric tile grid
    let (tw, th) = (96.0 * ds, 48.0 * ds);
    let sp = |n: &str| kit.sprites.get(n).map(|v| v.as_slice()).unwrap_or(&[]);
    let pick = |v: &[Img], r: f32| -> Option<usize> { (!v.is_empty()).then(|| ((r * v.len() as f32) as usize).min(v.len() - 1)) };
    let mut objs: Vec<Obj> = Vec::new();
    let mut taken: HashMap<(i32, i32), ()> = HashMap::new();
    let d_at = |x: f32, y: f32| field.sample(&field.d, x, y);
    let n_at = |x: f32, y: f32| (field.sample(&field.n1, x, y), field.sample(&field.n2, x, y));
    let place = |objs: &mut Vec<Obj>, set: &'static str, x: f32, y: f32, r: f32, pad: f32| -> bool {
        let v = sp(set);
        let Some(k) = pick(v, r) else { return false };
        let img: &Img = &v[k];
        if img.w == 0 || overlaps_icons(img, x, y, icons, pad) {
            return false;
        }
        objs.push(Obj { x: x as i32, y: y as i32, set, k });
        true
    };

    tiles(w, h, tw, th, |i, j, cx, cy| {
        let jx = (hash01(i, j, seed32 ^ 1) - 0.5) * tw * 0.35;
        let jy = (hash01(i, j, seed32 ^ 2) - 0.5) * th * 0.35;
        let (x, y) = (cx + jx, cy + jy);
        let r = hash01(i, j, seed32 ^ 3);
        let v = hash01(i, j, seed32 ^ 4);
        let d = d_at(x, y);
        let (n1, n2) = n_at(x, y);
        let forest = ctx.forest(d, n1, n2);
        let pad = 6.0 * ds;
        let placed = match theme {
            Theme::Arabia => {
                if forest > 0.5 && r < 0.9 {
                    place(&mut objs, if v < 0.05 { "n_tree_palm" } else { "n_tree_oak" }, x, y, hash01(i, j, 9), pad)
                } else if d > 50.0 * ds && d < 260.0 * ds && r < 0.018 {
                    place(&mut objs, "n_tree_oak", x, y, v, pad)
                } else if forest < 0.1 && r > 0.94 {
                    place(&mut objs, "n_plant_grass_dry", x, y, v, 0.0)
                } else {
                    false
                }
            }
            Theme::Arena => {
                if forest > 0.5 && r < 0.92 {
                    place(&mut objs, if v < 0.3 { "n_tree_pine" } else { "n_tree_oak" }, x, y, hash01(i, j, 9), pad)
                } else if forest < 0.1 && d > ctx.wall_r + 120.0 * ds && r < 0.012 {
                    place(&mut objs, "n_tree_oak", x, y, v, pad)
                } else if forest < 0.1 && (d - ctx.wall_r).abs() > 40.0 * ds && r > 0.95 {
                    place(&mut objs, "n_plant_grass_green", x, y, v, 0.0)
                } else {
                    false
                }
            }
            Theme::BlackForest => {
                if forest > 0.45 && r < 0.97 {
                    let set = if v < 0.72 { "n_tree_pine" } else if v < 0.985 { "n_tree_oak" } else { "n_tree_dead" };
                    place(&mut objs, set, x, y, hash01(i, j, 9), pad)
                } else if forest < 0.3 && r > 0.9 {
                    place(&mut objs, "n_plant_grass_green", x, y, v, 0.0)
                } else {
                    false
                }
            }
            Theme::Islands => {
                if d < 2.0 * ds && d > -40.0 * ds && r < 0.28 {
                    place(&mut objs, "n_tree_palm", x, y, v, pad)
                } else if d < -40.0 * ds && r < 0.75 && n1 > 0.42 {
                    place(&mut objs, if v < 0.25 { "n_tree_palm" } else { "n_tree_oak" }, x, y, hash01(i, j, 9), pad)
                } else if d < -10.0 * ds && r > 0.93 {
                    place(&mut objs, "n_plant_grass_green", x, y, v, 0.0)
                } else {
                    false
                }
            }
        };
        if placed {
            taken.insert((i, j), ());
        }
    });

    // Arena: a stone wall ring on the tile grid around each cluster.
    if theme == Theme::Arena {
        let walls = sp("b_west_wall_stone");
        if walls.len() >= 5 {
            let pieces = trace_walls(&field, ctx.wall_r, w, h, ds);
            // clear trees that would stand on the wall line
            objs.retain(|o| !pieces.iter().any(|&(x, y, _)| (o.x as f32 - x).abs() < tw * 0.55 && (o.y as f32 - y).abs() < th * 0.7));
            for (x, y, frame) in pieces {
                if !overlaps_icons(&walls[frame], x, y, icons, 2.0 * ds) {
                    objs.push(Obj { x: x as i32, y: y as i32, set: "b_west_wall_stone", k: frame });
                }
            }
        }
    }

    // Resource piles near the icons (not on Black Forest, where only nature lives).
    if matches!(theme, Theme::Arabia | Theme::Arena | Theme::Islands) {
        let piles: &[(&'static str, usize, f32, f32)] = match theme {
            Theme::Arabia => &[("n_mine_gold", 7, 90.0, 220.0), ("n_mine_stone", 5, 90.0, 240.0), ("n_mine_gold", 5, 160.0, 320.0), ("n_forage_bush", 6, 50.0, 130.0)],
            Theme::Arena => &[("n_mine_gold", 7, 40.0, 150.0), ("n_mine_stone", 5, 40.0, 150.0), ("n_forage_bush", 6, 30.0, 120.0), ("n_mine_gold", 6, 620.0, 2000.0)],
            _ => &[("n_mine_gold", 4, -80.0, -30.0)],
        };
        let mut centres: Vec<(f32, f32)> = Vec::new();
        for &(set, n, dmin, dmax) in piles {
            let (dmin, dmax) = (dmin * ds, dmax * ds);
            for _ in 0..300 {
                let (x, y) = (rng.range(0.05, 0.95) * w as f32, rng.range(0.08, 0.92) * h as f32);
                if centres.iter().any(|c| (c.0 - x).powi(2) + (c.1 - y).powi(2) < (190.0 * ds).powi(2)) {
                    continue;
                }
                let d = d_at(x, y);
                let (n1, n2) = n_at(x, y);
                if d < dmin || d > dmax || ctx.forest(d, n1, n2) > 0.05 {
                    continue;
                }
                // a compact clump of tiles
                let offs = [(0, 0), (1, 0), (0, 1), (1, 1), (-1, 0), (0, -1), (2, 1), (1, 2)];
                let mut count = 0;
                for &(a, b) in offs.iter().take(n) {
                    let px = x + (a - b) as f32 * tw * 0.5;
                    let py = y + (a + b) as f32 * th * 0.5;
                    if place(&mut objs, set, px, py, rng.f(), 4.0 * ds) {
                        count += 1;
                    }
                }
                if count > 0 {
                    centres.push((x, y));
                    break;
                }
            }
        }
    }

    objs.sort_by_key(|o| (o.y, o.x));
    std::thread::scope(|sc| {
        for (bi, chunk) in ground.chunks_mut(band * w).enumerate() {
            let objs = &objs;
            sc.spawn(move || {
                let r0 = bi * band;
                let r1 = r0 + chunk.len() / w;
                for o in objs {
                    blit(chunk, w, r0, r1, o, &sp(o.set)[o.k]);
                }
            });
        }
    });

    // ---- cells for animated units
    let (gw, gh) = (field.gw, field.gh);
    let mut cells = vec![0u8; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let (x, y) = ((gx as f32 + 0.5) * CELL, (gy as f32 + 0.5) * CELL);
            let i = gy * gw + gx;
            let d = field.d[i];
            cells[i] = if theme == Theme::Islands {
                if d > 120.0 * ds { DEEP } else if d > 40.0 * ds { WATER } else if d < -4.0 * ds { OPEN } else { 0 }
            } else if d < ROAM * ds && ctx.forest(d, field.n1[i], field.n2[i]) < 0.2 && (theme != Theme::Arena || (d - ctx.wall_r).abs() > 30.0 * ds) {
                OPEN
            } else {
                0
            };
            let _ = (x, y);
        }
    }
    // Units are drawn over the baked scene, so a unit standing behind a tree would appear on top of
    // its canopy. Block every cell where a unit's feet would put its body under a tall object's
    // sprite: from the top of the sprite down to just in front of its base.
    let body = 34.0 * ds;
    for o in &objs {
        let img = &sp(o.set)[o.k];
        if (img.h as f32) < 30.0 * ds {
            continue; // low piles and bushes: units may stand next to them
        }
        let (x0, x1) = (o.x as f32 + img.ox as f32 - 10.0 * ds, o.x as f32 + (img.ox + img.w as i32) as f32 + 10.0 * ds);
        let (y0, y1) = (o.y as f32 + img.oy as f32, o.y as f32 + 6.0 * ds + body * 0.25);
        let (gx0, gx1) = ((x0 / CELL).floor().max(0.0) as usize, ((x1 / CELL).ceil() as usize).min(gw));
        let (gy0, gy1) = ((y0 / CELL).floor().max(0.0) as usize, ((y1 / CELL).ceil() as usize).min(gh));
        for gy in gy0..gy1 {
            for gx in gx0..gx1 {
                let c = &mut cells[gy * gw + gx];
                if *c == OPEN {
                    *c = 0;
                }
            }
        }
    }

    let water = water_tex.as_ref().map(|t| water_tiles(t, ds, seed32));
    Scene { w, h, ground, water, gw, gh, cells, dist: field.d }
}

/// Wall frames in `b_*_wall_stone`: 0/1 the two tile diagonals, 2 a tower, 3 horizontal, 4 vertical.
const WALL_DIAG_DOWN: usize = 0;
const WALL_DIAG_UP: usize = 1;
const WALL_TOWER: usize = 2;
const WALL_H: usize = 3;
const WALL_V: usize = 4;

/// Follow the `level` contour of the field and lay wall pieces along it, picking the piece that
/// matches the local direction and spacing it like the game's tile grid. Towers hide the joints
/// where the direction changes. Returns (x, y, frame) hotspots.
fn trace_walls(f: &Field, level: f32, w: usize, h: usize, ds: f32) -> Vec<(f32, f32, usize)> {
    // contour crossings between neighbouring cells
    let mut pts: Vec<(f32, f32)> = Vec::new();
    let at = |gx: usize, gy: usize| f.d[gy * f.gw + gx] - level;
    for gy in 0..f.gh {
        for gx in 0..f.gw {
            let a = at(gx, gy);
            let (x, y) = ((gx as f32 + 0.5) * CELL, (gy as f32 + 0.5) * CELL);
            if gx + 1 < f.gw {
                let b = at(gx + 1, gy);
                if (a < 0.0) != (b < 0.0) {
                    pts.push((x + CELL * a / (a - b), y));
                }
            }
            if gy + 1 < f.gh {
                let b = at(gx, gy + 1);
                if (a < 0.0) != (b < 0.0) {
                    pts.push((x, y + CELL * a / (a - b)));
                }
            }
        }
    }
    let inside = |x: f32, y: f32| x > -40.0 && y > -40.0 && x < w as f32 + 40.0 && y < h as f32 + 60.0;
    pts.retain(|p| inside(p.0, p.1));
    // chain the points into polylines by nearest-neighbour walking
    let mut used = vec![false; pts.len()];
    let mut lines: Vec<Vec<(f32, f32)>> = Vec::new();
    for start in 0..pts.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut line = vec![pts[start]];
        for dir in 0..2 {
            loop {
                let cur = if dir == 0 { *line.last().unwrap() } else { line[0] };
                let mut best: Option<(usize, f32)> = None;
                for (k, p) in pts.iter().enumerate() {
                    if used[k] {
                        continue;
                    }
                    let dd = (p.0 - cur.0).powi(2) + (p.1 - cur.1).powi(2);
                    if dd < (CELL * 1.6).powi(2) && best.is_none_or(|b| dd < b.1) {
                        best = Some((k, dd));
                    }
                }
                let Some((k, _)) = best else { break };
                used[k] = true;
                if dir == 0 { line.push(pts[k]) } else { line.insert(0, pts[k]) }
            }
        }
        if line.len() > 6 {
            lines.push(line);
        }
    }
    let class_of = |dx: f32, dy: f32| -> usize {
        // direction of the wall line, folded into [0, 180) degrees (screen y points down)
        let a = dy.atan2(dx).to_degrees().rem_euclid(180.0);
        match a {
            a if !(13.0..167.0).contains(&a) => WALL_H,
            a if a < 58.0 => WALL_DIAG_DOWN,
            a if a < 122.0 => WALL_V,
            _ => WALL_DIAG_UP,
        }
    };
    let spacing = |c: usize| match c {
        WALL_H => 96.0,
        WALL_V => 48.0,
        _ => 53.7,
    } * ds;
    let mut out = Vec::new();
    for line in lines {
        // cumulative arc length
        let mut acc = vec![0f32; line.len()];
        for k in 1..line.len() {
            acc[k] = acc[k - 1] + ((line[k].0 - line[k - 1].0).powi(2) + (line[k].1 - line[k - 1].1).powi(2)).sqrt();
        }
        let total = *acc.last().unwrap();
        let point_at = |s: f32| -> (f32, f32) {
            let k = acc.partition_point(|&a| a < s).clamp(1, line.len() - 1);
            let t = ((s - acc[k - 1]) / (acc[k] - acc[k - 1]).max(1e-3)).clamp(0.0, 1.0);
            (line[k - 1].0 + (line[k].0 - line[k - 1].0) * t, line[k - 1].1 + (line[k].1 - line[k - 1].1) * t)
        };
        // Wide look-ahead plus a little hysteresis so wiggles in the contour don't flip the piece type,
        // and towers keep a minimum gap so joints don't pile up into rows of towers.
        let look = 56.0 * ds;
        let min_gap = 170.0 * ds;
        let mut s = 0.0;
        let mut prev: Option<usize> = None;
        let mut last_tower = f32::NEG_INFINITY;
        let mut pending = 0;
        while s <= total {
            let (a, b) = (point_at((s - look).max(0.0)), point_at((s + look).min(total)));
            let mut c = class_of(b.0 - a.0, b.1 - a.1);
            if let Some(q) = prev.filter(|&q| q != c) {
                pending += 1;
                if pending < 2 {
                    c = q; // needs two consecutive samples to switch
                }
            } else {
                pending = 0;
            }
            let p = point_at(s);
            if prev.is_some_and(|q| q != c) && s - last_tower >= min_gap {
                out.push((p.0, p.1, WALL_TOWER));
                last_tower = s;
                s += spacing(c) * 0.6;
            } else {
                out.push((p.0, p.1, c));
                s += spacing(c);
            }
            if prev != Some(c) {
                pending = 0;
            }
            prev = Some(c);
        }
    }
    out
}

/// Periodic isometric water tiles: an opaque base and a translucent, mirrored shimmer layer.
fn water_tiles(t: &Tex, ds: f32, seed: u32) -> (usize, usize, Vec<[u8; 4]>, Vec<[u8; 4]>) {
    let p = ((1.8 * t.n as f32 * ds).round() as i32).max(64) & !1;
    let (tw, th) = (p as usize, (p / 2) as usize);
    let mut a = vec![[0u8; 4]; tw * th];
    let mut b = vec![[0u8; 4]; tw * th];
    let n = t.n as i32;
    let o = (seed & 0x1ff) as i32;
    for y in 0..th as i32 {
        for x in 0..tw as i32 {
            let u = ((x + 2 * y) * n).div_euclid(p).rem_euclid(n);
            let v = ((2 * y - x) * n).div_euclid(p).rem_euclid(n);
            let c = t.px[(v * n + u) as usize];
            a[y as usize * tw + x as usize] = [c[0], c[1], c[2], 255];
            let c2 = t.px[(((n - 1 - v + o) & (n - 1)) * n + ((n - 1 - u) & (n - 1))) as usize];
            let lum = (c2[0] as f32 * 0.3 + c2[1] as f32 * 0.5 + c2[2] as f32 * 0.2) / 255.0;
            let k = smoothstep(0.35, 0.75, lum);
            b[y as usize * tw + x as usize] = [
                (c2[0] as f32 * 1.15).min(255.0) as u8,
                (c2[1] as f32 * 1.15).min(255.0) as u8,
                (c2[2] as f32 * 1.1).min(255.0) as u8,
                (k * 110.0) as u8,
            ];
        }
    }
    (tw, th, a, b)
}

/// A plausible desktop icon layout (columns from the top-left) for previews.
pub fn sample_icons(w: usize, h: usize, ds: f32, n: usize) -> Vec<Rect> {
    let (cw, ch) = (76.0 * ds, 92.0 * ds);
    let rows = ((h as f32 - 60.0 * ds) / ch).floor().max(1.0) as usize;
    (0..n)
        .map(|k| {
            let (c, r) = ((k / rows) as f32, (k % rows) as f32);
            let x = 2.0 * ds + c * cw;
            let y = 2.0 * ds + r * ch;
            Rect { x0: x, y0: y, x1: x + cw - 4.0 * ds, y1: y + ch - 8.0 * ds, pad: 0.0 }
        })
        .collect::<Vec<_>>()
        .into_iter()
        .filter(|r| r.x1 < w as f32)
        .collect()
}
