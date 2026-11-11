//! Builds a map-themed wallpaper around the desktop icons.
//!
//! The icon rectangles become a signed distance field (negative inside an icon cluster). Each
//! theme turns that field plus some noise into terrain layers and object placement, all baked
//! into one RGBA image on a background thread. Only that image and a handful of animated
//! sprites are drawn per frame, which keeps the wallpaper cheap to run.
use std::collections::HashMap;
use std::f32::consts::TAU;

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
    Nomad,
    Hideout,
    GoldRush,
    ForestNothing,
    Fortress,
}

impl Theme {
    pub const ALL: [Theme; 9] = [
        Theme::Arabia,
        Theme::Arena,
        Theme::BlackForest,
        Theme::Islands,
        Theme::Nomad,
        Theme::Hideout,
        Theme::GoldRush,
        Theme::ForestNothing,
        Theme::Fortress,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Theme::Arabia => "Arabia",
            Theme::Arena => "Arena",
            Theme::BlackForest => "Black Forest",
            Theme::Islands => "Islands",
            Theme::Nomad => "Nomad",
            Theme::Hideout => "Hideout",
            Theme::GoldRush => "Gold Rush",
            Theme::ForestNothing => "Forest Nothing",
            Theme::Fortress => "Fortress",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Theme::Arabia => "Dry open grassland, scattered woodlines, gold, stone and roaming herds.",
            Theme::Arena => "Your icons walled in by stone, a ring of forest outside the gates.",
            Theme::BlackForest => "Dense dark pine forest with clearings around your icons.",
            Theme::Islands => "Every icon cluster is an island; ships sail between them and fishing ships work the shallows.",
            Theme::Nomad => "Two villagers raise a Town Center by the woods, berries or gold, and another builds a dock on the coast.",
            Theme::Hideout => "Palisade walls split the map into four quadrants and fence in your icons.",
            Theme::GoldRush => "Gold piles ring your icons while villagers mine them, and a wolf pack attacks villagers who fight back.",
            Theme::ForestNothing => "Forest from edge to edge, with small clearings cut around your icons.",
            Theme::Fortress => "Walled cities around your icons, castles in the countryside and people everywhere.",
        }
    }

    /// Terrain textures used by the theme, in layer order.
    fn textures(self) -> &'static [&'static str] {
        match self {
            Theme::Arabia | Theme::GoldRush => &["g_gr3", "g_gr7", "g_gr4", "g_for", "g_des"],
            Theme::Nomad => &["g_gr2", "g_gr6", "g_gr4", "g_for", "g_gr3", "g_bch", "g_sh2"],
            Theme::Arena | Theme::Hideout => &["g_gr2", "g_gr6", "g_fo2", "g_gr4", "g_grs"],
            Theme::BlackForest | Theme::ForestNothing => &["g_for", "g_fo2", "g_grs", "g_r01", "g_underbrush_leaves"],
            Theme::Islands => &["g_gr2", "g_gr6", "g_bch", "g_sh2"],
            Theme::Fortress => &["g_gr2", "g_gr6", "g_rd1", "g_gr4", "g_for"],
        }
    }

    /// Static object sprites used by the theme.
    fn sprites(self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = match self {
            Theme::Arabia => vec!["n_tree_oak", "n_tree_palm", "n_mine_gold", "n_mine_stone", "n_forage_bush", "n_plant_grass_dry"],
            Theme::GoldRush => vec!["n_tree_oak", "n_tree_palm", "n_mine_gold", "n_mine_stone", "n_forage_bush", "n_plant_grass_dry"],
            Theme::Arena => vec!["n_tree_oak", "n_tree_pine", "b_west_wall_stone", "n_mine_gold", "n_mine_stone", "n_forage_bush", "n_plant_grass_green"],
            Theme::Hideout => vec!["n_tree_oak", "n_tree_pine", "b_dark_wall_palisade", "n_mine_gold", "n_mine_stone", "n_forage_bush", "n_plant_grass_green"],
            Theme::Nomad => vec![
                "n_tree_oak",
                "n_tree_pine",
                "n_mine_gold",
                "n_mine_stone",
                "n_forage_bush",
                "n_plant_grass_green",
                "b_misc_foundation_town_center",
                "b_misc_foundation_dock",
            ],
            Theme::BlackForest | Theme::ForestNothing => vec!["n_tree_pine", "n_tree_oak", "n_tree_dead", "n_plant_grass_green"],
            Theme::Islands => vec!["n_tree_palm", "n_tree_oak", "n_plant_grass_green", "n_mine_gold"],
            Theme::Fortress => vec!["n_tree_oak", "n_tree_pine", "b_west_wall_stone", "n_mine_gold", "n_mine_stone", "n_forage_bush", "n_plant_grass_green"],
        };
        if self == Theme::Fortress {
            v.extend(FORT_BUILDINGS.iter().map(|b| b.0));
        }
        v
    }

    pub fn has_water(self) -> bool {
        matches!(self, Theme::Islands | Theme::Nomad)
    }

    /// Stone or palisade wall sprite traced around the icon clusters.
    fn wall_set(self) -> Option<&'static str> {
        match self {
            Theme::Arena | Theme::Fortress => Some("b_west_wall_stone"),
            Theme::Hideout => Some("b_dark_wall_palisade"),
            _ => None,
        }
    }

    /// Radius (unscaled px) of the walled area around each cluster.
    fn wall_r(self) -> f32 {
        match self {
            Theme::Hideout => 150.0,
            Theme::Fortress => 235.0,
            _ => 200.0,
        }
    }
}

/// Fortress buildings: sprite, how many, and whether they stand inside the city walls.
const FORT_BUILDINGS: &[(&str, usize, bool)] = &[
    ("b_west_castle_age3", 2, false),
    ("b_west_market_age3", 1, false),
    ("b_west_monastery_age3", 1, false),
    ("b_west_university_age3", 1, false),
    ("b_west_barracks_age3", 1, false),
    ("b_west_blacksmith_age3", 1, true),
    ("b_west_mill_age2", 2, false),
    ("b_west_tower_age3", 5, false),
    ("b_west_house_age3", 8, true),
    ("b_west_house_age2", 6, false),
];

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
        for n in theme.sprites() {
            // buildings fly the player's colours
            let pc = n.starts_with("b_").then_some(crate::assets::BLUE);
            let mut frames = game.sprite_pc(n, ds, pc)?;
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

/// What a worker standing at a [`Post`] does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PostKind {
    Miner,
    Builder,
    Lumber,
}

/// A spot where a villager stands and works, facing (`fx`, `fy`).
#[derive(Clone, Copy, Debug)]
pub struct Post {
    pub x: f32,
    pub y: f32,
    pub fx: f32,
    pub fy: f32,
    pub kind: PostKind,
}

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
    /// Land a unit can cross anywhere on screen (no forest, wall or tall object), per cell.
    pub free: Vec<bool>,
    /// Signed distance to the icon clusters per cell (negative inside).
    pub dist: Vec<f32>,
    /// Where working villagers stand.
    pub posts: Vec<Post>,
}

impl Scene {
    pub fn cell(&self, x: f32, y: f32) -> u8 {
        let (cx, cy) = ((x / CELL) as isize, (y / CELL) as isize);
        if cx < 0 || cy < 0 || cx >= self.gw as isize || cy >= self.gh as isize {
            return 0;
        }
        self.cells[cy as usize * self.gw + cx as usize]
    }
    /// Off-screen counts as free, so paths can start and end outside the screen.
    pub fn is_free(&self, x: f32, y: f32) -> bool {
        let (cx, cy) = ((x / CELL).floor() as isize, (y / CELL).floor() as isize);
        if cx < 0 || cy < 0 || cx >= self.gw as isize || cy >= self.gh as isize {
            return true;
        }
        self.free[cy as usize * self.gw + cx as usize]
    }
    pub fn dist_at(&self, x: f32, y: f32) -> f32 {
        let cx = ((x.max(0.0) / CELL) as usize).min(self.gw - 1);
        let cy = ((y.max(0.0) / CELL) as usize).min(self.gh - 1);
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

/// A sandy beach, then shallows fading out over the water texture and deep water darkening.
/// `dn` is the distance out from the land: the beach starts at 0, the water at 26 px.
/// Returns the colour and its alpha over the water layer.
fn shore(c: [f32; 3], dn: f32, ds: f32, beach: [u8; 4], sh: [u8; 4]) -> ([f32; 3], f32) {
    let mut c = mix(c, beach, smoothstep(-6.0 * ds, 6.0 * ds, dn));
    let beach_end = 26.0 * ds;
    if dn <= beach_end - 6.0 * ds {
        return (c, 1.0);
    }
    let land = 1.0 - smoothstep(beach_end - 6.0 * ds, beach_end + 4.0 * ds, dn);
    let shallow = 1.0 - smoothstep(beach_end, beach_end + 70.0 * ds, dn);
    let deep = smoothstep(beach_end + 40.0 * ds, beach_end + 260.0 * ds, dn);
    let sa = shallow * 0.85;
    let ta = deep * 0.45;
    // composite: deep tint, then shallows, then land on top
    let tint = [10.0f32, 34.0, 58.0];
    let a = sa + ta * (1.0 - sa);
    let col = [0, 1, 2].map(|k| (sh[k] as f32 * sa + tint[k] * ta * (1.0 - sa)) / a.max(1e-4));
    let out_a = land + a * (1.0 - land);
    for k in 0..3 {
        c[k] = (c[k] * land + col[k] * a * (1.0 - land)) / out_a.max(1e-4);
    }
    (c, out_a)
}

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
    /// Nomad: the sea along one side of the screen (on the right?, x of the shoreline).
    sea: Option<(bool, f32)>,
}

impl Ctx<'_> {
    /// How far a point lies out to sea, in px (negative on land; very negative with no sea).
    /// The coast wanders, and always leaves a strip of land around the icons.
    fn sea_at(&self, x: f32, y: f32) -> f32 {
        let Some((right, shore)) = self.sea else { return f32::MIN };
        let ds = self.ds;
        let n1 = self.f.sample(&self.f.n1, x, y);
        let wobble = (n1 - 0.5) * 200.0 * ds + (value(x / (150.0 * ds), y / (150.0 * ds), self.seed ^ 0x5ea) - 0.5) * 90.0 * ds;
        let out = if right { x - shore } else { shore - x } + wobble;
        out.min(self.f.sample(&self.f.d, x, y) - 140.0 * ds)
    }

    /// Forest density mask in [0, 1] at a point (same function drives trees and forest floor).
    fn forest(&self, d: f32, n1: f32, n2: f32) -> f32 {
        let ds = self.ds;
        match self.kit.theme {
            Theme::Arabia | Theme::Nomad => smoothstep(0.635, 0.66, n2) * smoothstep(140.0 * ds, 200.0 * ds, d),
            Theme::GoldRush => smoothstep(0.65, 0.675, n2) * smoothstep(170.0 * ds, 230.0 * ds, d),
            Theme::Arena | Theme::Hideout => {
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
            // nothing but forest: the clearings hug the icons
            Theme::ForestNothing => smoothstep(22.0 * ds, 52.0 * ds, d + (n1 - 0.5) * 40.0 * ds),
            Theme::Fortress => smoothstep(0.64, 0.665, n2) * smoothstep(self.wall_r + 150.0 * ds, self.wall_r + 230.0 * ds, d),
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
            Theme::Arabia | Theme::Nomad | Theme::GoldRush => {
                c = mix(c, tex(1), smoothstep(0.44, 0.58, n1 + fine * 0.12));
                c = mix(c, tex(2), smoothstep(0.60, 0.70, n1 + fine * 0.15) * 0.9);
                // trampled earth around the "town centre" (the icons)
                c = mix(c, tex(2), smoothstep(40.0 * ds, -20.0 * ds, d + fine * 40.0 * ds) * 0.3);
                c = mix(c, tex(4), smoothstep(0.70, 0.78, n2 + fine * 0.1) * smoothstep(60.0 * ds, 120.0 * ds, d) * 0.8);
                c = mix(c, tex(3), smoothstep(0.15, 0.6, forest + fine * 0.3));
                if self.sea.is_some() {
                    let dn = self.sea_at(fx, fy) + 30.0 * ds + fine * 14.0 * ds;
                    if dn > -12.0 * ds {
                        (c, alpha) = shore(c, dn, ds, tex(5), tex(6));
                    }
                }
            }
            Theme::Fortress => {
                c = mix(c, tex(1), smoothstep(0.45, 0.62, n1 + fine * 0.12) * 0.8);
                // paved streets inside the city walls, worn ground along them, forest floor
                let inside = 1.0 - smoothstep(self.wall_r - 40.0 * ds, self.wall_r - 10.0 * ds, d + fine * 24.0 * ds);
                c = mix(c, tex(2), inside * 0.8);
                let ring = 1.0 - smoothstep(14.0 * ds, 40.0 * ds, (d - self.wall_r).abs() + fine * 20.0 * ds);
                c = mix(c, tex(3), ring * 0.6);
                c = mix(c, tex(4), smoothstep(0.2, 0.7, forest + fine * 0.3));
            }
            Theme::Arena | Theme::Hideout => {
                c = mix(c, tex(1), smoothstep(0.45, 0.62, n1 + fine * 0.12) * 0.8);
                // worn ground just inside the walls
                let ring = 1.0 - smoothstep(14.0 * ds, 40.0 * ds, (d - self.wall_r).abs() + fine * 20.0 * ds);
                c = mix(c, tex(3), ring * 0.6);
                c = mix(c, tex(4), smoothstep(self.wall_r + 30.0 * ds, self.wall_r + 70.0 * ds, d) * 0.7);
                c = mix(c, tex(2), smoothstep(0.2, 0.7, forest + fine * 0.3));
            }
            Theme::BlackForest | Theme::ForestNothing => {
                c = mix(c, tex(1), smoothstep(0.45, 0.6, n1 + fine * 0.1) * 0.7);
                c = mix(c, tex(4), smoothstep(0.62, 0.72, n2 + fine * 0.1) * forest * 0.6);
                let open = 1.0 - smoothstep(0.25, 0.75, forest + fine * 0.35);
                c = mix(c, tex(2), open);
                c = mix(c, tex(3), open * smoothstep(0.45, 0.6, n1) * 0.6);
                // the forest floor is darker than in the game to read as "Black" Forest
                let k = 1.0 - forest * if self.kit.theme == Theme::BlackForest { 0.22 } else { 0.1 };
                c = [c[0] * k, c[1] * k, c[2] * k];
            }
            Theme::Islands => {
                let dn = d + fine * 14.0 * ds + (n1 - 0.5) * 30.0 * ds;
                c = mix(c, tex(1), smoothstep(0.45, 0.6, n1 + fine * 0.1) * 0.7);
                (c, alpha) = shore(c, dn, ds, tex(2), tex(3));
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
    // One random stream per purpose: adding or moving an icon then only changes what is near it,
    // instead of shifting every random choice made after it (islands, piles, buildings...).
    let stream = |k: u64| Rng::new(seed ^ k.wrapping_mul(0x9e37_79b9_7f4a_7c15));

    // Islands get extra, decorative islands in large empty areas; an empty desktop gets a
    // single feature in the middle for the other maps.
    let mut shapes: Vec<Rect> = icons.to_vec();
    if theme == Theme::Islands {
        // A fixed list of candidates for this seed: an icon only removes the ones it gets close to.
        let mut rng = stream(1);
        let want = 2 + rng.below(3);
        let mut kept = 0;
        for _ in 0..16 {
            let (x, y) = (rng.range(0.1, 0.9) * w as f32, rng.range(0.12, 0.88) * h as f32);
            let r = rng.range(90.0, 170.0) * ds;
            if kept >= want {
                continue;
            }
            let dmin = shapes.iter().map(|q| rect_sd(q, x, y) - q.pad).fold(f32::MAX, f32::min);
            if dmin > r + 190.0 * ds {
                shapes.push(Rect { x0: x - r * 0.45, y0: y - r * 0.12, x1: x + r * 0.45, y1: y + r * 0.12, pad: r * 0.5 });
                kept += 1;
            }
        }
    } else if shapes.is_empty() {
        let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
        shapes.push(Rect { x0: cx - 90.0 * ds, y0: cy - 60.0 * ds, x1: cx + 90.0 * ds, y1: cy + 60.0 * ds, pad: 0.0 });
    }
    let (margin, warp, blend) = match theme {
        Theme::Islands => (14.0 * ds, 110.0 * ds, 22.0 * ds),
        Theme::Arena | Theme::Hideout | Theme::Fortress => (24.0 * ds, 50.0 * ds, 40.0 * ds),
        Theme::ForestNothing => (16.0 * ds, 60.0 * ds, 30.0 * ds),
        _ => (20.0 * ds, 90.0 * ds, 36.0 * ds),
    };
    let field = build_field(w, h, &shapes, margin, warp, blend, seed32, ds);
    let (tex, water_tex) = kit.textures().unwrap_or_default();
    if tex.len() < kit.theme.textures().len() {
        let n = field.gw * field.gh;
        return Scene { w, h, ground: vec![[40, 60, 30, 255]; w * h], water: None, gw: field.gw, gh: field.gh, cells: vec![0; n], free: vec![false; n], dist: field.d, posts: Vec::new() };
    }
    // Nomad's coast runs along the side of the screen with fewer icons near it.
    let sea = (theme == Theme::Nomad).then(|| {
        let near = |right: bool| icons.iter().filter(|r| if right { r.x1 > w as f32 * 0.7 } else { r.x0 < w as f32 * 0.3 }).count();
        let (l, r) = (near(false), near(true));
        let right = r < l || (r == l && seed32 & 1 == 1);
        (right, if right { w as f32 * 0.84 } else { w as f32 * 0.16 })
    });
    let ctx = Ctx { kit, tex: &tex, f: &field, seed: seed32, ds, wall_r: theme.wall_r() * ds, sea };

    // ---- terrain, split across cores (at background priority: the user's apps come first)
    let mut ground = vec![[0u8; 4]; w * h];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let band = h.div_ceil(threads);
    std::thread::scope(|sc| {
        for (bi, chunk) in ground.chunks_mut(band * w).enumerate() {
            let ctx = &ctx;
            sc.spawn(move || {
                crate::desktop::background_thread();
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
    // Footprints of buildings and foundations (isometric ellipses: x, y, radius) that nothing
    // else may be placed on.
    let mut reserved: Vec<(f32, f32, f32)> = Vec::new();
    let hit = |res: &[(f32, f32, f32)], x: f32, y: f32, r: f32| {
        res.iter().any(|&(rx, ry, rr)| {
            let (dx, dy) = (x - rx, (y - ry) * 2.0);
            dx * dx + dy * dy < (rr + r).powi(2)
        })
    };
    let on_screen = |img: &Img, x: f32, y: f32| {
        x + img.ox as f32 >= 0.0 && y + img.oy as f32 >= 0.0 && x + (img.ox + img.w as i32) as f32 <= w as f32 && y + (img.oy + img.h as i32) as f32 <= h as f32
    };
    // no forest anywhere on an isometric footprint of radius r
    let clear_of_forest = |x: f32, y: f32, r: f32| {
        [(0.0, 0.0), (0.7, 0.0), (-0.7, 0.0), (0.0, 0.35), (0.0, -0.35)].iter().all(|&(u, v)| {
            let (px, py) = (x + u * r, y + v * r);
            let (n1, n2) = n_at(px, py);
            ctx.forest(d_at(px, py), n1, n2) < 0.05
        })
    };
    let in_icon = |x: f32, y: f32, pad: f32| icons.iter().any(|r| x > r.x0 - pad && x < r.x1 + pad && y > r.y0 - pad && y < r.y1 + pad);
    let in_screen = |x: f32, y: f32| x > 4.0 && y > 4.0 && x < w as f32 - 4.0 && y < h as f32 - 4.0;
    let mut structures: Vec<Obj> = Vec::new();
    let mut posts: Vec<Post> = Vec::new();

    // Fortress: castles, markets and houses, inside the city walls and out in the countryside.
    if theme == Theme::Fortress {
        for (bi, &(set, count, inside)) in FORT_BUILDINGS.iter().enumerate() {
            let v = sp(set);
            if v.is_empty() {
                continue;
            }
            let mut rng = stream(100 + bi as u64);
            let mut placed = 0;
            for _ in 0..500 {
                let (x, y) = (rng.range(0.03, 0.97) * w as f32, rng.range(0.08, 0.98) * h as f32);
                // animated buildings (the mill's sails) store later frames as deltas: use the first
                let k = rng.below(v.len());
                let k = if v.len() > 4 { 0 } else { k };
                if placed >= count {
                    continue;
                }
                let img = &v[k];
                if img.w == 0 {
                    continue;
                }
                let r = img.w as f32 * 0.42;
                let d = d_at(x, y);
                let (n1, n2) = n_at(x, y);
                let zone = if inside { d > 0.0 && d < ctx.wall_r - r * 0.55 - 22.0 * ds } else { d > ctx.wall_r + r * 0.55 + 30.0 * ds };
                if !zone || ctx.forest(d, n1, n2) > 0.05 || !clear_of_forest(x, y, r) || hit(&reserved, x, y, r) || overlaps_icons(img, x, y, icons, 4.0 * ds) {
                    continue;
                }
                reserved.push((x, y, r));
                structures.push(Obj { x: x as i32, y: y as i32, set, k });
                placed += 1;
            }
        }
    }

    tiles(w, h, tw, th, |i, j, cx, cy| {
        let jx = (hash01(i, j, seed32 ^ 1) - 0.5) * tw * 0.35;
        let jy = (hash01(i, j, seed32 ^ 2) - 0.5) * th * 0.35;
        let (x, y) = (cx + jx, cy + jy);
        if hit(&reserved, x, y, 12.0 * ds) || hit(&reserved, x, y - 70.0 * ds, 12.0 * ds) {
            return;
        }
        let r = hash01(i, j, seed32 ^ 3);
        let v = hash01(i, j, seed32 ^ 4);
        let d = d_at(x, y);
        let (n1, n2) = n_at(x, y);
        let forest = ctx.forest(d, n1, n2);
        let pad = 6.0 * ds;
        let tree = hash01(i, j, 9);
        match theme {
            Theme::Arabia | Theme::GoldRush => {
                let palm = if theme == Theme::GoldRush { 0.15 } else { 0.05 };
                if forest > 0.5 && r < 0.9 {
                    place(&mut objs, if v < palm { "n_tree_palm" } else { "n_tree_oak" }, x, y, tree, pad);
                } else if d > 50.0 * ds && d < 260.0 * ds && r < 0.018 {
                    place(&mut objs, "n_tree_oak", x, y, v, pad);
                } else if forest < 0.1 && r > 0.94 {
                    place(&mut objs, "n_plant_grass_dry", x, y, v, 0.0);
                }
            }
            Theme::Nomad => {
                if ctx.sea_at(x, y) > -40.0 * ds {
                    // the beach and the sea stay clear
                } else if forest > 0.5 && r < 0.9 {
                    place(&mut objs, if v < 0.35 { "n_tree_pine" } else { "n_tree_oak" }, x, y, tree, pad);
                } else if d > 60.0 * ds && d < 280.0 * ds && r < 0.02 {
                    place(&mut objs, "n_tree_oak", x, y, v, pad);
                } else if forest < 0.1 && r > 0.94 {
                    place(&mut objs, "n_plant_grass_green", x, y, v, 0.0);
                }
            }
            Theme::Arena | Theme::Hideout => {
                if forest > 0.5 && r < 0.92 {
                    place(&mut objs, if v < 0.3 { "n_tree_pine" } else { "n_tree_oak" }, x, y, tree, pad);
                } else if forest < 0.1 && d > ctx.wall_r + 120.0 * ds && r < 0.012 {
                    place(&mut objs, "n_tree_oak", x, y, v, pad);
                } else if forest < 0.1 && (d - ctx.wall_r).abs() > 40.0 * ds && r > 0.95 {
                    place(&mut objs, "n_plant_grass_green", x, y, v, 0.0);
                }
            }
            Theme::BlackForest => {
                if forest > 0.45 && r < 0.97 {
                    let set = if v < 0.72 { "n_tree_pine" } else if v < 0.985 { "n_tree_oak" } else { "n_tree_dead" };
                    place(&mut objs, set, x, y, tree, pad);
                } else if forest < 0.3 && r > 0.9 {
                    place(&mut objs, "n_plant_grass_green", x, y, v, 0.0);
                }
            }
            Theme::ForestNothing => {
                if forest > 0.45 && r < 0.97 {
                    let set = if v < 0.62 { "n_tree_pine" } else if v < 0.99 { "n_tree_oak" } else { "n_tree_dead" };
                    place(&mut objs, set, x, y, tree, pad);
                } else if forest < 0.3 && r > 0.92 {
                    place(&mut objs, "n_plant_grass_green", x, y, v, 0.0);
                }
            }
            Theme::Fortress => {
                if forest > 0.5 && r < 0.9 {
                    place(&mut objs, if v < 0.3 { "n_tree_pine" } else { "n_tree_oak" }, x, y, tree, pad);
                } else if forest < 0.1 && d > ctx.wall_r + 170.0 * ds && r < 0.008 {
                    place(&mut objs, "n_tree_oak", x, y, v, pad);
                } else if forest < 0.1 && d > ctx.wall_r + 40.0 * ds && r > 0.96 {
                    place(&mut objs, "n_plant_grass_green", x, y, v, 0.0);
                }
            }
            Theme::Islands => {
                if d < 2.0 * ds && d > -40.0 * ds && r < 0.28 {
                    place(&mut objs, "n_tree_palm", x, y, v, pad);
                } else if d < -40.0 * ds && r < 0.75 && n1 > 0.42 {
                    place(&mut objs, if v < 0.25 { "n_tree_palm" } else { "n_tree_oak" }, x, y, tree, pad);
                } else if d < -10.0 * ds && r > 0.93 {
                    place(&mut objs, "n_plant_grass_green", x, y, v, 0.0);
                }
            }
        }
    });

    // Walls: stone (Arena, Fortress) or palisade (Hideout) along a contour around each cluster.
    let near = |pieces: &[(f32, f32, usize)], x: f32, y: f32| pieces.iter().any(|&(px, py, _)| (x - px).abs() < tw * 0.55 && (y - py).abs() < th * 0.7);
    if let Some(wall_set) = theme.wall_set() {
        let walls = sp(wall_set);
        if walls.len() >= 5 {
            // Each line becomes runs of pieces; a run is cut where a piece can't stand (on an icon,
            // a building, or a Hideout gate), and runs too short to read as a wall are dropped.
            let can_stand = |p: &(f32, f32, usize)| !overlaps_icons(&walls[p.2], p.0, p.1, icons, 2.0 * ds) && !hit(&reserved, p.0, p.1, 0.0);
            let runs = |line: Vec<(f32, f32, usize)>, ok: &dyn Fn(&(f32, f32, usize)) -> bool, min: usize| -> Vec<(f32, f32, usize)> {
                let mut out = Vec::new();
                let mut run: Vec<(f32, f32, usize)> = Vec::new();
                for p in line.into_iter().chain(std::iter::once((f32::NAN, f32::NAN, usize::MAX))) {
                    if p.2 != usize::MAX && ok(&p) {
                        run.push(p);
                        continue;
                    }
                    if run.len() >= min {
                        out.extend(run.drain(..));
                    }
                    run.clear();
                }
                out
            };
            let mut pieces: Vec<(f32, f32, usize)> = contour_lines(&field, ctx.wall_r, w, h).iter().flat_map(|l| runs(lay_wall(l, ds), &can_stand, 3)).collect();
            if theme == Theme::Hideout {
                // Two diagonal palisades through the middle split the map into its four quadrants;
                // they open into gates where they meet the icons.
                let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
                let clear = |p: &(f32, f32, usize)| {
                    let d = d_at(p.0, p.1);
                    d > 70.0 * ds && (d - ctx.wall_r).abs() > 45.0 * ds && ((p.0 - cx).powi(2) + ((p.1 - cy) * 2.0).powi(2)).sqrt() > 40.0 * ds && can_stand(p)
                };
                for (dx, dy) in [(1.0f32, 0.5f32), (1.0, -0.5)] {
                    let len = (dx * dx + dy * dy).sqrt();
                    let n = (w as f32 / 4.0) as i32;
                    let line: Vec<(f32, f32)> = (-n..=n)
                        .map(|k| (cx + dx / len * 4.0 * k as f32, cy + dy / len * 4.0 * k as f32))
                        .filter(|p| p.0 > -60.0 && p.1 > -60.0 && p.0 < w as f32 + 60.0 && p.1 < h as f32 + 60.0)
                        .collect();
                    pieces.extend(runs(lay_wall(&line, ds), &clear, 3));
                }
                if d_at(cx, cy) > 70.0 * ds {
                    pieces.push((cx, cy, WALL_TOWER));
                }
            }
            let mut seen = std::collections::HashSet::new();
            pieces.retain(|p| seen.insert((p.0.round() as i32, p.1.round() as i32)));
            // clear trees that would stand on the wall line
            objs.retain(|o| !near(&pieces, o.x as f32, o.y as f32));
            for (x, y, frame) in pieces {
                objs.push(Obj { x: x as i32, y: y as i32, set: wall_set, k: frame });
            }
        }
    }

    // Gold Rush: a ring of gold piles around every cluster of icons, with a few gaps.
    if theme == Theme::GoldRush {
        let mut ring: Vec<(f32, f32, usize)> = Vec::new();
        for line in contour_lines(&field, 46.0 * ds, w, h) {
            for (x, y) in resample(&line, tw * 0.62) {
                if hash01((x / 16.0) as i32, (y / 16.0) as i32, seed32 ^ 0x90) >= 0.12 {
                    ring.push((x, y, 0));
                }
            }
        }
        objs.retain(|o| !near(&ring, o.x as f32, o.y as f32));
        for (x, y, _) in ring {
            place(&mut objs, "n_mine_gold", x, y, hash01(x as i32, y as i32, seed32 ^ 0x91), 2.0 * ds);
        }
    }

    // Resource piles (not on the forest maps, where only nature lives).
    let piles: &[(&'static str, usize, f32, f32)] = match theme {
        Theme::Arabia => &[("n_mine_gold", 7, 90.0, 220.0), ("n_mine_stone", 5, 90.0, 240.0), ("n_mine_gold", 5, 160.0, 320.0), ("n_forage_bush", 6, 50.0, 130.0)],
        Theme::GoldRush => &[
            ("n_mine_gold", 7, 130.0, 260.0),
            ("n_mine_gold", 6, 200.0, 420.0),
            ("n_mine_gold", 6, 260.0, 600.0),
            ("n_mine_stone", 5, 110.0, 260.0),
            ("n_forage_bush", 6, 70.0, 140.0),
        ],
        Theme::Nomad => &[("n_mine_gold", 7, 90.0, 240.0), ("n_mine_stone", 5, 90.0, 260.0), ("n_forage_bush", 6, 50.0, 140.0)],
        Theme::Arena => &[("n_mine_gold", 7, 40.0, 150.0), ("n_mine_stone", 5, 40.0, 150.0), ("n_forage_bush", 6, 30.0, 120.0), ("n_mine_gold", 6, 620.0, 2000.0)],
        Theme::Hideout => &[("n_mine_gold", 6, 30.0, 105.0), ("n_mine_stone", 5, 30.0, 105.0), ("n_forage_bush", 5, 25.0, 100.0)],
        Theme::Fortress => &[("n_mine_gold", 7, 320.0, 660.0), ("n_mine_stone", 5, 320.0, 660.0), ("n_forage_bush", 6, 300.0, 540.0)],
        Theme::Islands => &[("n_mine_gold", 4, -80.0, -30.0)],
        Theme::BlackForest | Theme::ForestNothing => &[],
    };
    let mut centres: Vec<(f32, f32)> = Vec::new();
    for (pi, &(set, n, dmin, dmax)) in piles.iter().enumerate() {
        let (dmin, dmax) = (dmin * ds, dmax * ds);
        let mut rng = stream(200 + pi as u64);
        for _ in 0..300 {
            let (x, y) = (rng.range(0.05, 0.95) * w as f32, rng.range(0.08, 0.92) * h as f32);
            if centres.iter().any(|c| (c.0 - x).powi(2) + (c.1 - y).powi(2) < (190.0 * ds).powi(2)) || hit(&reserved, x, y, 60.0 * ds) {
                continue;
            }
            let d = d_at(x, y);
            let (n1, n2) = n_at(x, y);
            if d < dmin || d > dmax || ctx.forest(d, n1, n2) > 0.05 || ctx.sea_at(x, y) > -70.0 * ds {
                continue;
            }
            // a compact clump of tiles
            let offs = [(0, 0), (1, 0), (0, 1), (1, 1), (-1, 0), (0, -1), (2, 1), (1, 2)];
            let mut count = 0;
            for &(a, b) in offs.iter().take(n) {
                let px = x + (a - b) as f32 * tw * 0.5;
                let py = y + (a + b) as f32 * th * 0.5;
                if !hit(&reserved, px, py, 30.0 * ds) && place(&mut objs, set, px, py, hash01(px as i32, py as i32, seed32 ^ 0x77 ^ pi as u32), 4.0 * ds) {
                    count += 1;
                }
            }
            if count > 0 {
                centres.push((x, y));
                break;
            }
        }
    }

    // Nomad: two villagers raise a Town Center next to a woodline, a berry patch or a gold mine
    // (picked at random, falling back to the others), and one builds a dock on the coast.
    if theme == Theme::Nomad {
        let empty = |objs: &[Obj], x: f32, y: f32, r: f32| !objs.iter().any(|o| hit(&[(x, y, r)], o.x as f32, o.y as f32, 0.0));
        let tc = sp("b_misc_foundation_town_center");
        if !tc.is_empty() {
            let mut rng = stream(77);
            let k = (tc.len() - 1).min(1 + rng.below(tc.len().max(2) - 1));
            let img = &tc[k];
            let r = img.w as f32 * 0.42;
            let first = rng.below(3);
            let kinds: [&dyn Fn(&str) -> bool; 3] = [&|s: &str| s.starts_with("n_tree"), &|s: &str| s == "n_forage_bush", &|s: &str| s == "n_mine_gold"];
            let mut spot = None;
            'kinds: for kk in 0..3 {
                let is = kinds[(first + kk) % 3];
                let mut targets: Vec<(f32, f32)> = objs.iter().filter(|o| is(o.set)).map(|o| (o.x as f32, o.y as f32)).collect();
                for i in (1..targets.len()).rev() {
                    targets.swap(i, rng.below(i + 1));
                }
                for &(tx, ty) in targets.iter().take(60) {
                    let a0 = rng.f() * TAU;
                    for step in 0..12 {
                        let a = a0 + step as f32 * TAU / 12.0;
                        let dist = r + 0.8 * tw;
                        let (x, y) = (tx + a.cos() * dist, ty + a.sin() * dist * 0.5);
                        let fits = on_screen(img, x, y)
                            && !overlaps_icons(img, x, y, icons, 6.0 * ds)
                            && ctx.sea_at(x, y) < -(r + 60.0 * ds)
                            && empty(&objs, x, y, r * 1.08)
                            && !hit(&reserved, x, y, r);
                        if fits {
                            spot = Some((x, y));
                            break 'kinds;
                        }
                    }
                }
            }
            if let Some((x, y)) = spot {
                reserved.push((x, y, r));
                structures.push(Obj { x: x as i32, y: y as i32, set: "b_misc_foundation_town_center", k });
                // two builders on the near corners of the 4x4 footprint, facing its middle
                let (a, b) = (img.w as f32 * 0.48, img.w as f32 * 0.24);
                let mut n = 0;
                for (ux, uy) in [(-0.6f32, 0.62f32), (0.6, 0.62), (0.0, 1.1), (-1.08, 0.0), (1.08, 0.0)] {
                    let (px, py) = (x + ux * a, y + uy * b);
                    if n < 2 && in_screen(px, py) && !in_icon(px, py, 6.0 * ds) {
                        posts.push(Post { x: px, y: py, fx: x, fy: y, kind: PostKind::Builder });
                        n += 1;
                    }
                }
            }
        }
        let dock = sp("b_misc_foundation_dock");
        if let (Some((right, _)), false) = (ctx.sea, dock.is_empty()) {
            let k = dock.len() - 1;
            let img = &dock[k];
            let r = img.w as f32 * 0.42;
            let toward_sea = if right { 1.0 } else { -1.0 };
            let mut rng = stream(78);
            let off = rng.f();
            'rows: for i in 0..12 {
                let y = h as f32 * (0.2 + ((off + i as f32 * 0.382) % 1.0) * 0.6);
                // walk out from the land until the dock's middle stands in the shallows
                let mut x = w as f32 * 0.5;
                while x > 0.0 && x < w as f32 {
                    if ctx.sea_at(x, y) > 0.35 * r {
                        break;
                    }
                    x += toward_sea * 6.0;
                }
                // its landward corner touches the beach, its seaward corner is out on the water
                let land_corner = (x - toward_sea * r * 1.05, y);
                let fits = on_screen(img, x, y)
                    && ctx.sea_at(land_corner.0, land_corner.1) < 0.0
                    && ctx.sea_at(x + toward_sea * r, y) > 0.0
                    && !overlaps_icons(img, x, y, icons, 6.0 * ds)
                    && !hit(&reserved, x, y, r);
                if !fits {
                    continue;
                }
                // the builder stands on the beach beside the landward corner
                for (ux, uy) in [(-1.25f32, 0.15f32), (-1.15, 0.45), (-1.15, -0.3)] {
                    let (px, py) = (x + toward_sea * ux * r, y + uy * r * 0.5);
                    if ctx.sea_at(px, py) < -6.0 * ds && in_screen(px, py) && !in_icon(px, py, 6.0 * ds) {
                        reserved.push((x, y, r));
                        objs.retain(|o| !hit(&[(x, y, r * 1.1)], o.x as f32, o.y as f32, 0.0));
                        structures.push(Obj { x: x as i32, y: y as i32, set: "b_misc_foundation_dock", k });
                        posts.push(Post { x: px, y: py, fx: x, fy: y, kind: PostKind::Builder });
                        break 'rows;
                    }
                }
            }
        }
    }

    objs.extend(structures);
    objs.sort_by_key(|o| (o.y, o.x));
    std::thread::scope(|sc| {
        for (bi, chunk) in ground.chunks_mut(band * w).enumerate() {
            let objs = &objs;
            sc.spawn(move || {
                crate::desktop::background_thread();
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
    let mut free = vec![false; gw * gh];
    let walled = theme.wall_set().is_some();
    let roam = if theme == Theme::Fortress { f32::MAX } else { ROAM * ds };
    for i in 0..gw * gh {
        let d = field.d[i];
        if theme == Theme::Islands {
            cells[i] = if d > 120.0 * ds { DEEP } else if d > 40.0 * ds { WATER } else if d < 20.0 * ds { OPEN } else { 0 };
            continue;
        }
        let f = ctx.forest(d, field.n1[i], field.n2[i]);
        let wall_ok = !walled || (d - ctx.wall_r).abs() > 30.0 * ds;
        cells[i] = if d < roam && f < 0.2 && wall_ok { OPEN } else { 0 };
        free[i] = f < 0.25 && wall_ok;
        if ctx.sea.is_some() {
            let sea = ctx.sea_at(((i % gw) as f32 + 0.5) * CELL, ((i / gw) as f32 + 0.5) * CELL);
            if sea > -14.0 * ds {
                cells[i] = if sea > 110.0 * ds { DEEP } else if sea > 10.0 * ds { WATER } else { 0 };
                free[i] = false;
            }
        }
    }
    // Units are drawn over the baked scene, so a unit standing behind a tree would appear on top of
    // its canopy. Block every cell where a unit's feet would put its body under a tall object's
    // sprite: from the top of the sprite down to just in front of its base.
    let body = 34.0 * ds;
    for o in &objs {
        let img = &sp(o.set)[o.k];
        if (img.h as f32) < 30.0 * ds || o.set.starts_with("n_mine") || o.set.starts_with("n_forage") {
            continue; // low piles and bushes: units may stand next to them
        }
        let (x0, x1) = (o.x as f32 + img.ox as f32 - 10.0 * ds, o.x as f32 + (img.ox + img.w as i32) as f32 + 10.0 * ds);
        let (y0, y1) = (o.y as f32 + img.oy as f32, o.y as f32 + 6.0 * ds + body * 0.25);
        let (gx0, gx1) = ((x0 / CELL).floor().max(0.0) as usize, ((x1 / CELL).ceil().max(0.0) as usize).min(gw));
        let (gy0, gy1) = ((y0 / CELL).floor().max(0.0) as usize, ((y1 / CELL).ceil().max(0.0) as usize).min(gh));
        for gy in gy0..gy1 {
            for gx in gx0..gx1 {
                let c = &mut cells[gy * gw + gx];
                if *c == OPEN {
                    *c = 0;
                }
                free[gy * gw + gx] = false;
            }
        }
    }
    let cell_at = |x: f32, y: f32| -> u8 {
        let (cx, cy) = ((x / CELL) as isize, (y / CELL) as isize);
        if cx < 0 || cy < 0 || cx >= gw as isize || cy >= gh as isize { 0 } else { cells[cy as usize * gw + cx as usize] }
    };

    // ---- work posts: miners beside gold and stone, woodcutters at the edge of a clearing
    let piles_xy: Vec<(f32, f32)> = objs.iter().filter(|o| o.set == "n_mine_gold" || o.set == "n_mine_stone").map(|o| (o.x as f32, o.y as f32)).collect();
    for &(x, y) in &piles_xy {
        let side = if hash01(x as i32, y as i32, seed32 ^ 0x33) < 0.5 { -1.0 } else { 1.0 };
        for s2 in [side, -side] {
            let (px, py) = (x + s2 * 0.30 * tw, y + 0.36 * th);
            let crowded = piles_xy.iter().any(|&(qx, qy)| {
                let (dx, dy) = (px - qx, (py - qy) * 2.0);
                dx * dx + dy * dy < (0.33 * tw).powi(2)
            });
            let gi = ((py.max(0.0) / CELL) as usize).min(gh - 1) * gw + ((px.max(0.0) / CELL) as usize).min(gw - 1);
            let behind_tree = if theme == Theme::Islands { cell_at(px, py) != OPEN } else { !free[gi] };
            if crowded || behind_tree || !in_screen(px, py) || in_icon(px, py, 4.0 * ds) {
                continue;
            }
            posts.push(Post { x: px, y: py, fx: x, fy: y, kind: PostKind::Miner });
            break;
        }
    }
    if theme == Theme::ForestNothing {
        let trees: Vec<(f32, f32)> = objs.iter().filter(|o| o.set.starts_with("n_tree")).map(|o| (o.x as f32, o.y as f32)).collect();
        let mut n = 0;
        for &(x, y) in &trees {
            if n >= 60 {
                break;
            }
            for (u, v) in [(-0.36f32, 0.14f32), (0.36, 0.14), (0.0, 0.5)] {
                let (px, py) = (x + u * tw, y + v * th);
                let (n1, n2) = n_at(px, py);
                let open = ctx.forest(d_at(px, py), n1, n2) < 0.45;
                let crowded = trees.iter().any(|&(qx, qy)| (qx, qy) != (x, y) && (px - qx).powi(2) + ((py - qy) * 2.0).powi(2) < (0.34 * tw).powi(2));
                if open && !crowded && in_screen(px, py) && !in_icon(px, py, 4.0 * ds) {
                    posts.push(Post { x: px, y: py, fx: x, fy: y - 6.0 * ds, kind: PostKind::Lumber });
                    n += 1;
                    break;
                }
            }
        }
    }

    // Now that the posts are placed beside them, block the resources themselves and the desktop
    // icons: walking units go around them, never across. A unit is drawn upwards from its feet, so
    // the band just below an icon is blocked too, or its body would cover the label.
    let mut block = |x0: f32, y0: f32, x1: f32, y1: f32, round: bool| {
        let (gx0, gx1) = ((x0 / CELL).floor().max(0.0) as usize, ((x1 / CELL).ceil().max(0.0) as usize).min(gw));
        let (gy0, gy1) = ((y0 / CELL).floor().max(0.0) as usize, ((y1 / CELL).ceil().max(0.0) as usize).min(gh));
        let (cx, cy, rx, ry) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5, (x1 - x0) * 0.5, (y1 - y0) * 0.5);
        for gy in gy0..gy1 {
            for gx in gx0..gx1 {
                let (px, py) = ((gx as f32 + 0.5) * CELL, (gy as f32 + 0.5) * CELL);
                if round && ((px - cx) / rx).powi(2) + ((py - cy) / ry).powi(2) > 1.0 {
                    continue;
                }
                let c = &mut cells[gy * gw + gx];
                if *c == OPEN {
                    *c = 0;
                }
                free[gy * gw + gx] = false;
            }
        }
    };
    for o in objs.iter().filter(|o| o.set.starts_with("n_mine") || o.set.starts_with("n_forage")) {
        let (x, y) = (o.x as f32, o.y as f32);
        block(x - 0.42 * tw, y - 0.42 * th, x + 0.42 * tw, y + 0.42 * th + 10.0 * ds, true);
    }
    for r in icons {
        block(r.x0 - 12.0 * ds, r.y0 - 4.0 * ds, r.x1 + 12.0 * ds, r.y1 + 30.0 * ds, false);
    }

    let water = water_tex.as_ref().map(|t| water_tiles(t, ds, seed32));
    Scene { w, h, ground, water, gw, gh, cells, free, dist: field.d, posts }
}

/// Wall frames in `b_*_wall_*`: 0 rises to the right, 1 falls to the right, 2 a tower,
/// 3 horizontal, 4 vertical.
const WALL_DIAG_DOWN: usize = 1;
const WALL_DIAG_UP: usize = 0;
const WALL_TOWER: usize = 2;
const WALL_H: usize = 3;
const WALL_V: usize = 4;

/// The `level` contour of the field as polylines (on screen, plus a small border).
fn contour_lines(f: &Field, level: f32, w: usize, h: usize) -> Vec<Vec<(f32, f32)>> {
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
    lines
}

/// Cumulative arc length of a polyline.
fn arc(line: &[(f32, f32)]) -> Vec<f32> {
    let mut acc = vec![0f32; line.len()];
    for k in 1..line.len() {
        acc[k] = acc[k - 1] + ((line[k].0 - line[k - 1].0).powi(2) + (line[k].1 - line[k - 1].1).powi(2)).sqrt();
    }
    acc
}

fn point_at(line: &[(f32, f32)], acc: &[f32], s: f32) -> (f32, f32) {
    let k = acc.partition_point(|&a| a < s).clamp(1, line.len() - 1);
    let t = ((s - acc[k - 1]) / (acc[k] - acc[k - 1]).max(1e-3)).clamp(0.0, 1.0);
    (line[k - 1].0 + (line[k].0 - line[k - 1].0) * t, line[k - 1].1 + (line[k].1 - line[k - 1].1) * t)
}

/// Points every `step` px along a polyline.
fn resample(line: &[(f32, f32)], step: f32) -> Vec<(f32, f32)> {
    if line.len() < 2 {
        return Vec::new();
    }
    let acc = arc(line);
    let total = *acc.last().unwrap();
    let mut out = Vec::new();
    let mut s = 0.0;
    while s <= total {
        out.push(point_at(line, &acc, s));
        s += step;
    }
    out
}

/// Lay wall pieces along a polyline the way the game does: one piece per tile of the isometric
/// grid, on a chain of neighbouring tiles, each piece chosen by the step to the next tile so that
/// neighbouring pieces always join. Towers hide some of the joints where the direction changes.
/// Returns (x, y, frame) hotspots.
fn lay_wall(line: &[(f32, f32)], ds: f32) -> Vec<(f32, f32, usize)> {
    if line.len() < 2 {
        return Vec::new();
    }
    let (tw, th) = (96.0 * ds, 48.0 * ds);
    let tile = |x: f32, y: f32| ((x / tw + y / th).round() as i32, (y / th - x / tw).round() as i32);
    let centre = |(u, v): (i32, i32)| ((u - v) as f32 * tw * 0.5, (u + v) as f32 * th * 0.5);
    let adjacent = |a: (i32, i32), b: (i32, i32)| (a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1;
    // chain of 8-connected tiles following the line
    let mut path: Vec<(i32, i32)> = Vec::new();
    for (x, y) in resample(line, tw / 8.0) {
        let t = tile(x, y);
        loop {
            let Some(&last) = path.last() else {
                path.push(t);
                break;
            };
            if last == t {
                break;
            }
            let next = if adjacent(last, t) { t } else { (last.0 + (t.0 - last.0).signum(), last.1 + (t.1 - last.1).signum()) };
            // cut corners: two axis steps that make a diagonal become one diagonal step
            while path.len() >= 2 && adjacent(path[path.len() - 2], next) {
                path.pop();
            }
            if path.last() != Some(&next) {
                path.push(next);
            }
            if next == t {
                break;
            }
        }
    }
    if path.len() < 2 {
        return Vec::new();
    }
    let class_of = |a: (i32, i32), b: (i32, i32)| match (b.0 - a.0, b.1 - a.1) {
        (_, 0) => WALL_DIAG_DOWN, // along u: down-right on screen
        (0, _) => WALL_DIAG_UP,   // along v: down-left on screen
        (du, dv) if du == dv => WALL_V,
        _ => WALL_H,
    };
    let n = path.len();
    let mut out = Vec::with_capacity(n);
    let mut last_tower = -100i32;
    for i in 0..n {
        let c_out = class_of(path[i.min(n - 2)], path[i.min(n - 2) + 1]);
        let c_in = if i > 0 { class_of(path[i - 1], path[i]) } else { c_out };
        let (x, y) = centre(path[i]);
        let frame = if c_in != c_out && i as i32 - last_tower >= 4 {
            last_tower = i as i32;
            WALL_TOWER
        } else {
            c_out
        };
        out.push((x, y, frame));
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
