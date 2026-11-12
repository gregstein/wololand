//! Icon Wars' damage on the real desktop icons, drawn in click-through overlay windows that sit
//! just above the icon layer: a health bar, the icon shaking and flashing red when it is struck,
//! fire that grows as it weakens, its collapse into falling pieces and dust, and the rubble that
//! burns out and smokes where it stood for the rest of the battle.
//!
//! To shake or break an icon the overlay hides the real one under a patch of the map's ground and
//! draws a copy of it. The copy is a screenshot of the icon taken the first time it is struck; its
//! background is cut away by comparing it with the wallpaper that was under it.
use crate::actors::World;
use crate::assets::{Img, BLUE};
use crate::battle::{Battle, Phase, FALL, ICON_HP};
use crate::overlay::{self, Overlay};
use crate::scene::{Rect, Scene};

/// Pixels of room around the icon picture for the bar, flying pieces, dust and rubble.
const PAD_X: i32 = 30;
const PAD_TOP: i32 = 44;
const PAD_BOTTOM: i32 = 40;

/// The icon picture with its background cut away: straight RGBA, `w`x`h`.
struct Cut {
    w: usize,
    h: usize,
    px: Vec<[u8; 4]>,
}

/// What the overlay currently shows, so it is only redrawn when that changes.
#[derive(Clone, Copy, PartialEq)]
struct Key {
    tag: u8,
    hp: u16,
    frame: u32,
    has_cut: bool,
}

struct Item {
    rect: Rect,
    /// The icon picture (no label), scene pixels: x0, y0, x1, y1.
    pic: (i32, i32, i32, i32),
    win: Option<Overlay>,
    cut: Option<Cut>,
    /// Failed screenshots, so a covered icon is not retried every frame.
    tries: u32,
    retry_at: f32,
    drawn: Option<Key>,
}

pub struct IconFx {
    origin: (i32, i32),
    ds: f32,
    items: Vec<Item>,
    clock: f32,
    since_raise: f32,
    logged_ok: bool,
    last_why: String,
}

fn hash(a: u32, b: u32) -> f32 {
    let mut h = a.wrapping_mul(0x9e37_79b9) ^ b.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    (h & 0xffff) as f32 / 65535.0
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A premultiplied BGRA canvas.
struct Canvas<'a> {
    px: &'a mut [u32],
    w: i32,
    h: i32,
}

impl Canvas<'_> {
    /// Blend a straight-alpha colour over pixel (x, y).
    fn put(&mut self, x: i32, y: i32, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let a = a.min(1.0);
        let p = &mut self.px[(y * self.w + x) as usize];
        let (db, dg, dr, da) = ((*p & 0xff) as f32, ((*p >> 8) & 0xff) as f32, ((*p >> 16) & 0xff) as f32, (*p >> 24) as f32);
        let k = 1.0 - a;
        let r = c[0] as f32 * a + dr * k;
        let g = c[1] as f32 * a + dg * k;
        let b = c[2] as f32 * a + db * k;
        let al = 255.0 * a + da * k;
        *p = ((al as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: [u8; 3], a: f32) {
        for y in y0..y1 {
            for x in x0..x1 {
                self.put(x, y, c, a);
            }
        }
    }

    /// A soft round puff.
    fn puff(&mut self, cx: f32, cy: f32, r: f32, c: [u8; 3], a: f32) {
        let (x0, x1, y0, y1) = ((cx - r) as i32, (cx + r) as i32 + 1, (cy - r * 0.7) as i32, (cy + r * 0.7) as i32 + 1);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = (((x as f32 - cx) / r).powi(2) + ((y as f32 - cy) / (r * 0.7)).powi(2)).sqrt();
                if d < 1.0 {
                    self.put(x, y, c, a * (1.0 - d * d));
                }
            }
        }
    }

    /// A game sprite with its hotspot at (x, y).
    fn sprite(&mut self, img: &Img, x: i32, y: i32, a: f32) {
        for sy in 0..img.h {
            for sx in 0..img.w {
                let p = img.px[sy * img.w + sx];
                if p[3] > 0 {
                    self.put(x + img.ox + sx as i32, y + img.oy + sy as i32, [p[0], p[1], p[2]], p[3] as f32 / 255.0 * a);
                }
            }
        }
    }
}

impl IconFx {
    pub fn new(origin: (i32, i32), ds: f32) -> IconFx {
        IconFx { origin, ds, items: Vec::new(), clock: 0.0, since_raise: 0.0, logged_ok: false, last_why: String::new() }
    }

    /// Match the overlays to the battle's icons (new layout: start over).
    fn sync(&mut self, battle: &Battle, pics: &[(i32, i32, i32, i32)]) {
        let icons = battle.icons();
        if self.items.len() == icons.len() && self.items.iter().zip(icons).all(|(a, b)| a.rect == b.rect) {
            return;
        }
        self.items = icons
            .iter()
            .map(|i| {
                let r = i.rect;
                let pic = pics
                    .iter()
                    .copied()
                    .find(|p| (p.0 - r.x0 as i32).abs() <= 3 && (p.1 - r.y0 as i32).abs() <= 3)
                    .unwrap_or((r.x0 as i32, r.y0 as i32, r.x1 as i32, (r.y0 + (r.x1 - r.x0) * 0.58) as i32));
                Item { rect: r, pic, win: None, cut: None, tries: 0, retry_at: 0.0, drawn: None }
            })
            .collect();
    }

    pub fn update(&mut self, world: &World, scene: &Scene, pics: &[(i32, i32, i32, i32)], dt: f32) {
        let Some(battle) = world.battle() else { return };
        self.sync(battle, pics);
        self.clock = (self.clock + dt) % 3600.0;
        self.since_raise += dt;
        let raise = self.since_raise > 1.0;
        if raise {
            self.since_raise = 0.0;
        }
        let ds = self.ds;
        for (k, icon) in battle.icons().iter().enumerate() {
            let item = &mut self.items[k];
            let hurt = icon.hp < ICON_HP || icon.phase != Phase::Standing;
            // the first blow: take the icon's picture while it is still whole
            if hurt && item.cut.is_none() && self.clock >= item.retry_at {
                match grab(item.pic, self.origin, scene, world) {
                    Ok(cut) => {
                        item.cut = Some(cut);
                        if !self.logged_ok {
                            self.logged_ok = true;
                            crate::log("icon wars: icon picture taken");
                        }
                    }
                    Err(why) => {
                        item.tries += 1;
                        item.retry_at = self.clock + 1.0;
                        if self.last_why != why {
                            crate::log(&format!("icon wars: icon picture not taken yet ({why})"));
                            self.last_why = why;
                        }
                    }
                }
            }
            let (tag, frame) = match icon.phase {
                Phase::Standing if icon.since_hit < 0.3 => (1, (self.clock * 30.0) as u32),
                // burning: the flames flicker
                Phase::Standing if icon.hp < ICON_HP => (1, (self.clock * 15.0) as u32),
                Phase::Standing => (0, 0),
                Phase::Falling(t) => (2, (t * 30.0) as u32),
                // the ruin burns out and stops smoking, then it is a still picture
                Phase::Ruined(t) if t < BURN_OUT => (3, (t * 12.0) as u32),
                Phase::Ruined(_) => (4, 0),
            };
            let key = Key { tag, hp: (icon.hp / ICON_HP * 40.0).ceil() as u16, frame, has_cut: item.cut.is_some() };
            let alive = item.win.as_ref().is_some_and(|w| w.alive());
            if alive && item.drawn == Some(key) {
                if raise {
                    item.win.as_ref().unwrap().raise();
                }
                continue;
            }
            let (x0, y0, x1, y1) = item.pic;
            let (wx, wy) = (x0 - PAD_X, y0 - PAD_TOP);
            let (ww, wh) = ((x1 - x0 + 2 * PAD_X) as usize, (y1 - y0 + PAD_TOP + PAD_BOTTOM) as usize);
            if !alive {
                item.win = Overlay::new(wx + self.origin.0, wy + self.origin.1, ww, wh);
            }
            let Some(win) = item.win.as_mut() else { continue };
            let (w, h) = win.size();
            let px = win.pixels();
            px.fill(0);
            let mut cv = Canvas { px, w: w as i32, h: h as i32 };
            draw(&mut cv, k as u32, icon.phase, icon.hp, icon.side, icon.since_hit, self.clock, item.pic, item.cut.as_ref(), scene, battle.rubble(), battle.flames(), (wx, wy), ds);
            win.present();
            item.drawn = Some(key);
        }
    }
}

/// Screenshot the icon picture and cut its background away against the wallpaper under it.
fn grab(pic: (i32, i32, i32, i32), origin: (i32, i32), scene: &Scene, world: &World) -> Result<Cut, String> {
    let (x0, y0, x1, y1) = pic;
    let (w, h) = ((x1 - x0).max(1) as usize, (y1 - y0).max(1) as usize);
    let (sx, sy) = (x0 + origin.0, y0 + origin.1);
    // the picture must be in plain view: no window over it, no mouse hovering it
    for (px, py) in [(sx + 2, sy + 2), (sx + w as i32 - 3, sy + 2), (sx + w as i32 / 2, sy + h as i32 / 2), (sx + 2, sy + h as i32 - 3), (sx + w as i32 - 3, sy + h as i32 - 3)] {
        let c = overlay::window_class_at(px, py);
        // our own overlay sits over every icon (it only draws the bar above the picture)
        if c != "SysListView32" && c != "WololandIconOverlay" {
            return Err(format!("{c} is over the icon"));
        }
    }
    if let Some((cx, cy)) = overlay::cursor() {
        if cx >= sx - 4 && cy >= sy - 4 && cx < sx + w as i32 + 4 && cy < sy + h as i32 + 30 {
            return Err("the mouse is over it".into());
        }
    }
    let shot = overlay::capture(sx, sy, w, h).ok_or("screen capture failed")?;
    // what the wallpaper shows there: ground plus any unit painted over it
    let mut under: Vec<[f32; 3]> = (0..w * h)
        .map(|i| {
            let (x, y) = (x0 + (i % w) as i32, y0 + (i / w) as i32);
            let g = scene.patch_px(x, y).unwrap_or([0, 0, 0, 255]);
            [g[0] as f32, g[1] as f32, g[2] as f32]
        })
        .collect();
    for (ux, uy, f) in world.visible() {
        let Some(bytes) = f.img.as_bytes(0) else { continue };
        let (fw, fh) = (f.w as i32, f.h as i32);
        let (fx, fy) = ((ux + f.ox) as i32, (uy + f.oy) as i32);
        if fx > x1 || fy > y1 || fx + fw < x0 || fy + fh < y0 {
            continue;
        }
        for y in 0..fh {
            for x in 0..fw {
                let (px, py) = (fx + x - x0, fy + y - y0);
                if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                    continue;
                }
                let i = ((y * fw + x) * 4) as usize;
                let a = bytes[i + 3] as f32 / 255.0;
                let d = &mut under[py as usize * w + px as usize];
                for (k, c) in [bytes[i + 2], bytes[i + 1], bytes[i]].into_iter().enumerate() {
                    d[k] = c as f32 * a + d[k] * (1.0 - a);
                }
            }
        }
    }
    let px = shot
        .iter()
        .zip(&under)
        .map(|(s, u)| {
            let diff = (0..3).map(|k| (s[k] as f32 - u[k]).abs()).fold(0.0, f32::max);
            [s[0], s[1], s[2], (smooth(10.0, 46.0, diff) * 255.0) as u8]
        })
        .collect::<Vec<_>>();
    let solid = px.iter().filter(|p| p[3] > 128).count();
    if solid < w * h / 20 {
        return Err("no icon found in the screenshot".into());
    }
    Ok(Cut { w, h, px })
}

#[allow(clippy::too_many_arguments)]
fn draw(cv: &mut Canvas, seed: u32, phase: Phase, hp: f32, side: usize, since_hit: f32, clock: f32, pic: (i32, i32, i32, i32), cut: Option<&Cut>, scene: &Scene, rubble: Option<&Img>, flames: &[Vec<Img>], win: (i32, i32), ds: f32) {
    let (x0, y0, x1, y1) = pic;
    let (pw, ph) = (x1 - x0, y1 - y0);
    // picture position inside the overlay
    let (ox, oy) = (x0 - win.0, y0 - win.1);
    let floor = oy + ph + (12.0 * ds) as i32;
    let ground = |cv: &mut Canvas, scorch: f32| {
        for y in 0..ph {
            for x in 0..pw {
                let g = scene.patch_px(x0 + x, y0 + y).unwrap_or([60, 80, 40, 255]);
                // burnt earth under the rubble
                let (dx, dy) = ((x as f32 / pw as f32 - 0.5) * 2.0, (y as f32 / ph as f32 - 0.75) * 2.4);
                let k = 1.0 - scorch * (1.0 - (dx * dx + dy * dy).min(1.0)) * 0.55;
                cv.put(ox + x, oy + y, [(g[0] as f32 * k) as u8, (g[1] as f32 * k) as u8, (g[2] as f32 * k) as u8], 1.0);
            }
        }
    };
    let team = if side == 0 { BLUE } else { [52, 196, 64] };
    let bar = |cv: &mut Canvas, frac: f32| {
        let bw = (pw as f32 * 0.62) as i32;
        let bh = (4.0 * ds).round().max(3.0) as i32;
        let bx = ox + (pw - bw) / 2;
        let by = oy - bh - (3.0 * ds) as i32;
        cv.rect(bx - 1, by - 1, bx + bw + 1, by + bh + 1, [10, 8, 6], 0.85);
        cv.rect(bx, by, bx + bw, by + bh, [60, 18, 14], 0.95);
        let fill = (bw as f32 * frac.clamp(0.0, 1.0)).round() as i32;
        cv.rect(bx, by, bx + fill, by + bh, team, 1.0);
        // a lighter top edge, like the game's bars
        let hi = [team[0].saturating_add(70), team[1].saturating_add(70), team[2].saturating_add(70)];
        cv.rect(bx, by, bx + fill, by + 1, hi, 0.8);
    };
    let Some(cut) = cut else {
        // no picture of the icon (it was covered when struck): it still disappears under the
        // dust and is gone while it lies in ruins, it just doesn't break into pieces
        let hide = |cv: &mut Canvas, a: f32, scorch: f32| {
            if a <= 0.0 {
                return;
            }
            for y in 0..ph {
                for x in 0..pw {
                    let g = scene.patch_px(x0 + x, y0 + y).unwrap_or([60, 80, 40, 255]);
                    let k = 1.0 - scorch * 0.3;
                    cv.put(ox + x, oy + y, [(g[0] as f32 * k) as u8, (g[1] as f32 * k) as u8, (g[2] as f32 * k) as u8], a);
                }
            }
        };
        match phase {
            Phase::Standing => {
                fire(cv, flames, ox, oy, pw, ph, 1.0 - hp / ICON_HP, clock, seed);
                bar(cv, hp / ICON_HP);
            }
            Phase::Falling(t) => {
                let u = t / FALL;
                hide(cv, smooth(0.05, 0.45, u), u);
                if let Some(r) = rubble {
                    cv.sprite(r, ox + pw / 2, floor - (6.0 * ds) as i32, smooth(0.35, 0.9, u));
                }
                dust(cv, ox, pw, floor, u, ds);
            }
            Phase::Ruined(t) => {
                hide(cv, 1.0, 1.0);
                if let Some(r) = rubble {
                    cv.sprite(r, ox + pw / 2, floor - (6.0 * ds) as i32, 1.0);
                }
                ruin_fire(cv, flames, ox, pw, floor, t, seed, ds);
            }
        }
        return;
    };
    let shard = (5.0 * ds).round().max(3.0) as usize;
    // where each piece of the icon lands when it falls
    let fall_at = |bx: usize, by: usize, n: u32, t: f32| -> (f32, f32, f32) {
        let vx = ((bx as f32 - cut.w as f32 * 0.5) * 2.2 + (hash(seed, n * 3) - 0.5) * 70.0) * ds;
        let vy = -(40.0 + hash(seed, n * 3 + 1) * 120.0) * ds;
        let g = 520.0 * ds;
        let land = (floor - oy) as f32 - 2.0 - hash(seed, n * 3 + 2) * 10.0 * ds - by as f32;
        // time at which the piece reaches its landing height
        let tl = (-vy + (vy * vy + 2.0 * g * land.max(0.0)).sqrt()) / g;
        let tt = t.min(tl);
        (bx as f32 + vx * tt, by as f32 + vy * tt + 0.5 * g * tt * tt, tl)
    };
    let pieces = |cv: &mut Canvas, t_of: &dyn Fn(f32) -> (f32, f32)| {
        let mut n = 0u32;
        for by in (0..cut.h).step_by(shard) {
            for bx in (0..cut.w).step_by(shard) {
                n += 1;
                let (t, a) = t_of(hash(seed, n * 5 + 9));
                let (fx, fy, _) = fall_at(bx, by, n, t);
                let (dx, dy) = ((fx - bx as f32).round() as i32, (fy - by as f32).round() as i32);
                for y in by..(by + shard).min(cut.h) {
                    for x in bx..(bx + shard).min(cut.w) {
                        let p = cut.px[y * cut.w + x];
                        if p[3] > 0 {
                            cv.put(ox + x as i32 + dx, oy + y as i32 + dy, [p[0], p[1], p[2]], p[3] as f32 / 255.0 * a);
                        }
                    }
                }
            }
        }
    };
    match phase {
        Phase::Standing => {
            if hp >= ICON_HP {
                bar(cv, 1.0);
                return;
            }
            ground(cv, 0.0);
            // struck: a jolt and a red flash; weakened: it cracks and burns
            let jolt = if since_hit < 0.2 { (2.0 * ds).round() as i32 } else { 0 };
            let f = (clock * 30.0) as u32;
            let (dx, dy) = if jolt > 0 { (((hash(seed, f) - 0.5) * 2.0 * jolt as f32) as i32, ((hash(seed, f + 77) - 0.5) * 2.0 * jolt as f32) as i32) } else { (0, 0) };
            let flash = if since_hit < 0.25 { 0.55 * (1.0 - since_hit / 0.25) } else { 0.0 };
            let n = ((1.0 - hp / ICON_HP) * 7.0).ceil() as usize;
            let crack: std::collections::HashSet<(i32, i32)> = cracks(seed, cut.w, cut.h, n).into_iter().collect();
            for y in 0..cut.h {
                for x in 0..cut.w {
                    let p = cut.px[y * cut.w + x];
                    if p[3] == 0 {
                        continue;
                    }
                    // scorched as the fire takes hold
                    let burnt = 1.0 - (1.0 - hp / ICON_HP) * 0.45;
                    let mut c = [p[0] as f32 * burnt, p[1] as f32 * burnt * 0.92, p[2] as f32 * burnt * 0.85];
                    if crack.contains(&(x as i32, y as i32)) {
                        c = [c[0] * 0.18, c[1] * 0.16, c[2] * 0.14];
                    }
                    let c = [c[0] + (255.0 - c[0]) * flash, c[1] * (1.0 - flash * 0.75), c[2] * (1.0 - flash * 0.8)];
                    cv.put(ox + x as i32 + dx, oy + y as i32 + dy, [c[0] as u8, c[1] as u8, c[2] as u8], p[3] as f32 / 255.0);
                }
            }
            fire(cv, flames, ox, oy, pw, ph, 1.0 - hp / ICON_HP, clock, seed);
            bar(cv, hp / ICON_HP);
        }
        Phase::Falling(t) => {
            let u = t / FALL;
            ground(cv, smooth(0.2, 0.8, u));
            if let Some(r) = rubble {
                cv.sprite(r, ox + pw / 2, floor - (6.0 * ds) as i32, smooth(0.35, 0.9, u));
            }
            pieces(cv, &|_| (t, 1.0 - smooth(0.55, 1.0, u)));
            // the fire collapses with it
            fire(cv, flames, ox, oy, pw, ph, 1.0 - smooth(0.0, 0.5, u), clock, seed);
            dust(cv, ox, pw, floor, u, ds);
        }
        Phase::Ruined(t) => {
            ground(cv, 1.0);
            if let Some(r) = rubble {
                cv.sprite(r, ox + pw / 2, floor - (6.0 * ds) as i32, 1.0);
            }
            ruin_fire(cv, flames, ox, pw, floor, t, seed, ds);
        }
    }
}

/// Seconds a ruin keeps burning and smoking before it lies still.
const BURN_OUT: f32 = 40.0;

/// The fire on a burning icon: one of the game's building fires standing on it, bigger the
/// weaker it is (`heat` 0..1), and smoke above it once it burns properly.
#[allow(clippy::too_many_arguments)]
fn fire(cv: &mut Canvas, flames: &[Vec<Img>], ox: i32, oy: i32, pw: i32, ph: i32, heat: f32, clock: f32, seed: u32) {
    if heat <= 0.12 || flames.is_empty() {
        return;
    }
    let size = ((heat - 0.12) / 0.88 * flames.len() as f32).floor().min(flames.len() as f32 - 1.0) as usize;
    let anim = &flames[size];
    // each icon's fire runs at its own point in the loop
    let f = ((clock * 12.0) as usize + seed as usize * 7) % anim.len();
    cv.sprite(&anim[f], ox + pw / 2, oy + ph - ph / 8, 1.0);
}

/// The ruin after the collapse: the fire burns down to nothing, then the ruin lies still.
#[allow(clippy::too_many_arguments)]
fn ruin_fire(cv: &mut Canvas, flames: &[Vec<Img>], ox: i32, pw: i32, floor: i32, t: f32, seed: u32, ds: f32) {
    if t >= BURN_OUT {
        return;
    }
    // biggest fire first, one size smaller every few seconds
    let n = flames.len();
    let step = BURN_OUT * 0.75 / n.max(1) as f32;
    let size = n as i32 - 1 - (t / step) as i32;
    if size >= 0 {
        let anim = &flames[size as usize];
        let f = ((t * 12.0) as usize + seed as usize * 7) % anim.len();
        cv.sprite(&anim[f], ox + pw / 2, floor - (6.0 * ds) as i32, 1.0);
    }
    // and smoke thinning out
    let fade = 1.0 - smooth(BURN_OUT * 0.5, BURN_OUT, t);
    let base = floor as f32 - 6.0 * ds;
    for k in 0..3 {
        let p = (t * 0.3 + k as f32 / 3.0).fract();
        let x = ox as f32 + pw as f32 * 0.5 + ((t * 0.7 + k as f32 * 2.1).sin() * 5.0 + (k as f32 - 1.0) * 9.0) * ds;
        let y = base - 30.0 * ds - p * 40.0 * ds;
        cv.puff(x, y, (5.0 + 12.0 * p) * ds, [80, 76, 72], 0.45 * fade * (1.0 - p) * smooth(0.0, 0.15, p));
    }
}

/// Dust thrown up by a collapse, `u` = 0..1 through it.
fn dust(cv: &mut Canvas, ox: i32, pw: i32, floor: i32, u: f32, ds: f32) {
    for k in 0..7 {
        let f = k as f32 / 6.0;
        let cx = ox as f32 + pw as f32 * (0.05 + 0.9 * f) + (f - 0.5) * 26.0 * ds * u;
        let cy = floor as f32 - 8.0 * ds - 14.0 * ds * u * (1.0 - (f - 0.5).abs());
        let r = (7.0 + 26.0 * u) * ds;
        cv.puff(cx, cy, r, [170, 152, 120], 0.6 * (1.0 - u).powf(1.5));
    }
}

/// Crack lines across an icon `w`x`h`: `n` jagged lines from its edge towards the middle, some
/// forking, as pixel positions.
fn cracks(seed: u32, w: usize, h: usize, n: usize) -> Vec<(i32, i32)> {
    let (w, h) = (w as f32, h as f32);
    let mut out = Vec::new();
    let walk = |mut x: f32, mut y: f32, mut a: f32, len: usize, k: u32, out: &mut Vec<(i32, i32)>| {
        for s in 0..len {
            if s % 3 == 0 {
                a += (hash(seed, k * 131 + s as u32) - 0.5) * 1.1;
            }
            x += a.cos();
            y += a.sin();
            if x < 0.0 || y < 0.0 || x >= w || y >= h {
                break;
            }
            out.push((x as i32, y as i32));
        }
        (x, y, a)
    };
    for k in 0..n as u32 {
        // start on the edge, aim roughly at the middle
        let u = hash(seed, k * 17 + 1);
        let (x, y) = match k % 4 {
            0 => (u * w, 0.0),
            1 => (w - 1.0, u * h),
            2 => (u * w, h - 1.0),
            _ => (0.0, u * h),
        };
        let a = (h * 0.5 - y).atan2(w * 0.5 - x) + (hash(seed, k * 17 + 2) - 0.5) * 0.8;
        let len = ((0.35 + 0.35 * hash(seed, k * 17 + 3)) * w.max(h)) as usize;
        let (fx, fy, fa) = walk(x, y, a, len, k, &mut out);
        // a fork off the end
        if hash(seed, k * 17 + 4) < 0.6 {
            walk(fx, fy, fa + 0.9, len / 3, k + 1000, &mut out);
        }
    }
    out
}

/// Developer check, without touching the screen: a sample icon on Icon Wars ground drawn through
/// every stage (struck, burning, collapsing, burning out in ruins) into one strip, `out`.
pub fn self_test(out: &str) -> anyhow::Result<()> {
    let game = crate::assets::Game::locate(None).ok_or_else(|| anyhow::anyhow!("AoE2 DE not found"))?;
    let kit = crate::scene::ThemeKit::load(&game, crate::scene::Theme::IconWars, 1.0)?;
    let icons = crate::scene::sample_icons(400, 300, 1.0, 3);
    let scene = crate::scene::build(&kit, 400, 300, &icons, 7);
    let kit = crate::battle::Kit::load(&game, 1.0)?;
    let (rubble, flames) = (kit.rubble_img(), kit.flames().to_vec());
    let r = icons[1];
    let pic = (r.x0 as i32, r.y0 as i32, r.x1 as i32, r.y0 as i32 + 46);
    let (pw, ph) = ((pic.2 - pic.0) as usize, (pic.3 - pic.1) as usize);
    // a stand-in desktop icon: a rounded tile with a glossy top
    let icon_px = |x: usize, y: usize| -> [u8; 4] {
        let (cx, cy) = (x as f32 - pw as f32 / 2.0, y as f32 - ph as f32 / 2.0);
        let s = 19.0f32;
        let d = (cx.abs() - s + 6.0).max(0.0).hypot((cy.abs() - s + 6.0).max(0.0)) - 6.0;
        if d > 0.0 {
            return [0, 0, 0, 0];
        }
        let k = 1.0 - (cy + s) / (2.0 * s) * 0.45;
        [(70.0 * k) as u8, (140.0 * k) as u8, (235.0 * k) as u8, 255]
    };
    let cut = Cut { w: pw, h: ph, px: (0..pw * ph).map(|i| icon_px(i % pw, i / pw)).collect() };
    let stages: &[(Phase, f32, f32)] = &[
        (Phase::Standing, ICON_HP, 99.0),
        (Phase::Standing, ICON_HP * 0.75, 0.05),
        (Phase::Standing, ICON_HP * 0.45, 2.0),
        (Phase::Standing, ICON_HP * 0.15, 2.0),
        (Phase::Falling(0.15), 0.0, 0.0),
        (Phase::Falling(0.45), 0.0, 0.0),
        (Phase::Falling(0.9), 0.0, 0.0),
        (Phase::Falling(1.35), 0.0, 0.0),
        (Phase::Ruined(4.0), 0.0, 9.0),
        (Phase::Ruined(20.0), 0.0, 9.0),
        (Phase::Ruined(32.0), 0.0, 9.0),
        (Phase::Ruined(60.0), 0.0, 9.0),
    ];
    let (wx, wy) = (pic.0 - PAD_X, pic.1 - PAD_TOP);
    let (ww, wh) = (pw as i32 + 2 * PAD_X, ph as i32 + PAD_TOP + PAD_BOTTOM);
    let mut strip = image::RgbaImage::new((ww * stages.len() as i32) as u32 * 2, wh as u32 * 2);
    for (n, &(phase, hp, since)) in stages.iter().enumerate() {
        let mut buf = vec![0u32; (ww * wh) as usize];
        let mut cv = Canvas { px: &mut buf, w: ww, h: wh };
        draw(&mut cv, 3, phase, hp, 0, since, 0.4, pic, Some(&cut), &scene, rubble.as_ref(), &flames, (wx, wy), 1.0);
        for y in 0..wh {
            for x in 0..ww {
                let (sx, sy) = (wx + x, wy + y);
                // the desktop as Explorer draws it: ground, then the real icon
                let g = scene.patch_px(sx, sy).unwrap_or([40, 60, 30, 255]);
                let mut c = [g[0] as f32, g[1] as f32, g[2] as f32];
                let (ix, iy) = (sx - pic.0, sy - pic.1);
                if ix >= 0 && iy >= 0 && (ix as usize) < pw && (iy as usize) < ph {
                    let p = icon_px(ix as usize, iy as usize);
                    let a = p[3] as f32 / 255.0;
                    for k in 0..3 {
                        c[k] = p[k] as f32 * a + c[k] * (1.0 - a);
                    }
                }
                // the label
                if sy >= pic.3 + 4 && sy < pic.3 + 14 && sx >= pic.0 + 12 && sx < pic.2 - 12 && (sx + sy) % 3 != 0 {
                    c = [245.0, 245.0, 245.0];
                }
                let o = buf[(y * ww + x) as usize];
                let a = (o >> 24) as f32 / 255.0;
                let px = [((o >> 16) & 255) as f32 + c[0] * (1.0 - a), ((o >> 8) & 255) as f32 + c[1] * (1.0 - a), (o & 255) as f32 + c[2] * (1.0 - a)];
                let p = image::Rgba([px[0].min(255.0) as u8, px[1].min(255.0) as u8, px[2].min(255.0) as u8, 255]);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    strip.put_pixel(((n as i32 * ww + x) * 2 + dx) as u32, (y * 2 + dy) as u32, p);
                }
            }
        }
    }
    strip.save(out)?;
    Ok(())
}
