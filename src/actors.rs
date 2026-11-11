//! Animated units: grazing herds, wandering deer and wolves, birds overhead, ships at sea,
//! villagers at work and the occasional chase across the map. Each one is a few small
//! pre-decoded frames; the per-frame cost is a position update.
use std::f32::consts::TAU;
use std::sync::Arc;

use anyhow::Result;
use gpui::RenderImage;

use crate::assets::{Game, Img, BLUE, RED};
use crate::noise::Rng;
use crate::scene::{PostKind, Scene, Theme, CELL, DEEP, OPEN, WATER};

/// Frames per direction kept in memory (the game ships 30-60).
const MAX_PER_DIR: usize = 12;
/// Animation clocks wrap at this many seconds so they never lose float precision.
const CLOCK_WRAP: f32 = 3600.0;

pub struct Frame {
    pub img: Arc<RenderImage>,
    pub ox: f32,
    pub oy: f32,
    pub w: f32,
    pub h: f32,
}

pub struct Anim {
    pub dirs: usize,
    pub per_dir: usize,
    pub frames: Vec<Frame>,
    /// Playback speed in frames per second.
    pub fps: f32,
}

impl Anim {
    fn from_imgs(imgs: Vec<Img>, cycle_secs: f32) -> Anim {
        let dirs = if imgs.len() >= 16 { 16 } else { 1 };
        Anim::with_dirs(imgs, dirs, cycle_secs)
    }

    fn with_dirs(imgs: Vec<Img>, dirs: usize, cycle_secs: f32) -> Anim {
        let total = (imgs.len() / dirs).max(1);
        let per_dir = total.clamp(1, MAX_PER_DIR);
        let mut frames = Vec::with_capacity(dirs * per_dir);
        for d in 0..dirs {
            for k in 0..per_dir {
                let Some(img) = imgs.get(d * total + k * total / per_dir) else { continue };
                let buf = image::RgbaImage::from_raw(img.w.max(1) as u32, img.h.max(1) as u32, if img.w == 0 { vec![0; 4] } else { img.bgra() })
                    .expect("frame size");
                frames.push(Frame {
                    img: Arc::new(RenderImage::new(vec![image::Frame::new(buf)])),
                    ox: img.ox as f32,
                    oy: img.oy as f32,
                    w: img.w as f32,
                    h: img.h as f32,
                });
            }
        }
        let dirs = (frames.len() / per_dir).max(1);
        Anim { dirs, per_dir, frames, fps: per_dir as f32 / cycle_secs }
    }

    pub fn frame(&self, dir: usize, t: f32) -> &Frame {
        let k = (t * self.fps) as usize % self.per_dir;
        &self.frames[((dir % self.dirs) * self.per_dir + k).min(self.frames.len() - 1)]
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Walks around open ground near its home, idles in between.
    Herd,
    /// Roams open ground anywhere.
    Roamer,
    Bird,
    Ship,
    /// Stands at a work post from the scene and plays its work animation.
    Worker(PostKind),
    /// Fishing ship at work in the shallows.
    Fisher,
    /// Only appears in the chase across the map.
    Parade,
    /// Gold Rush's skirmish: wolves (true) attacking villagers (false) who fight back.
    Fighter(bool),
}

struct Species {
    kind: Kind,
    walk: Anim,
    idle: Option<Anim>,
    speed: f32,
    count: usize,
}

enum State {
    Idle(f32),
    Walk { tx: f32, ty: f32 },
}

struct Actor {
    sp: usize,
    x: f32,
    y: f32,
    home: (f32, f32),
    dir: usize,
    state: State,
    t: f32,
    /// Birds: velocity; others: unused.
    vx: f32,
    vy: f32,
}

/// One pass of the chase: units following a path across the screen, `lag` px apart.
struct Run {
    line: Vec<(f32, f32)>,
    acc: Vec<f32>,
    /// (species, distance behind the leader, sideways offset)
    members: Vec<(usize, f32, f32)>,
    s: f32,
    speed: f32,
    t: f32,
}

/// Arabia's chase: a red scout fleeing a blue one, then three militia running from a wolf.
struct Parade {
    /// red scout, blue scout, militia, wolf
    sp: [usize; 4],
    stage: u8,
    wait: f32,
    run: Option<Run>,
}

pub struct World {
    species: Vec<Species>,
    actors: Vec<Actor>,
    rng: Rng,
    ds: f32,
    parade: Option<Parade>,
}

pub struct Kit {
    species: Vec<Species>,
    parade: Option<[usize; 4]>,
    ds: f32,
}

impl Kit {
    pub fn load(game: &Game, theme: Theme, ds: f32) -> Result<Kit> {
        let anim = |name: &str, pc: Option<[u8; 3]>, cycle: f32| -> Result<Anim> { Ok(Anim::from_imgs(game.sprite_pc(name, ds, pc)?, cycle)) };
        let mut sp: Vec<Species> = Vec::new();
        let mut add = |kind: Kind, walk: Anim, idle: Option<Anim>, speed: f32, count: usize| -> usize {
            sp.push(Species { kind, walk, idle, speed: speed * ds, count });
            sp.len() - 1
        };
        // animals and birds
        let herd = |name: &str, idle: &str| -> Result<(Anim, Option<Anim>)> { Ok((anim(name, None, 1.0)?, Some(anim(idle, None, 4.0)?))) };
        // player units: walk + idle in a player colour
        let unit = |name: &str, idle: &str, pc: [u8; 3]| -> Result<(Anim, Option<Anim>)> { Ok((anim(name, Some(pc), 1.0)?, Some(anim(idle, Some(pc), 4.0)?))) };
        let work = |name: &str| anim(name, Some(BLUE), 1.6);
        let mut parade = None;
        let sheep = || herd("a_herd_sheep_walkA", "a_herd_sheep_idleA");
        let deer = || herd("a_hunt_deer_walkA", "a_hunt_deer_idleA");
        let wolf = || herd("a_pred_wolf_walkA", "a_pred_wolf_idleA");
        let miners = |add: &mut dyn FnMut(Kind, Anim, Option<Anim>, f32, usize) -> usize, male: usize, female: usize| -> Result<()> {
            add(Kind::Worker(PostKind::Miner), work("u_vil_male_miner_gold_taskA")?, None, 0.0, male);
            add(Kind::Worker(PostKind::Miner), work("u_vil_female_miner_gold_taskA")?, None, 0.0, female);
            Ok(())
        };
        let bird = |name: &str| anim(name, None, 1.2);
        match theme {
            Theme::Arabia => {
                let (w, i) = sheep()?;
                add(Kind::Herd, w, i, 18.0, 5);
                let (w, i) = deer()?;
                add(Kind::Roamer, w, i, 26.0, 3);
                add(Kind::Bird, bird("a_bird_hawk_flyA")?, None, 55.0, 1);
                miners(&mut add, 3, 2)?;
                let red = add(Kind::Parade, anim("u_cav_scout_walkA", Some(RED), 0.7)?, None, 0.0, 0);
                let blue = add(Kind::Parade, anim("u_cav_scout_walkA", Some(BLUE), 0.7)?, None, 0.0, 0);
                let militia = add(Kind::Parade, anim("u_inf_militia_walkA", Some(BLUE), 0.55)?, None, 0.0, 0);
                let wolf = add(Kind::Parade, anim("a_pred_wolf_runA", None, 0.6)?, None, 0.0, 0);
                parade = Some([red, blue, militia, wolf]);
            }
            Theme::Arena => {
                let (w, i) = sheep()?;
                add(Kind::Herd, w, i, 18.0, 6);
                add(Kind::Bird, bird("a_bird_hawk_flyA")?, None, 55.0, 1);
                miners(&mut add, 3, 2)?;
            }
            Theme::Hideout => {
                let (w, i) = sheep()?;
                add(Kind::Herd, w, i, 18.0, 5);
                let (w, i) = deer()?;
                add(Kind::Roamer, w, i, 26.0, 2);
                add(Kind::Bird, bird("a_bird_hawk_flyA")?, None, 55.0, 1);
                miners(&mut add, 3, 2)?;
            }
            Theme::BlackForest => {
                let (w, i) = deer()?;
                add(Kind::Roamer, w, i, 26.0, 4);
                let (w, i) = wolf()?;
                add(Kind::Roamer, w, i, 30.0, 2);
                add(Kind::Bird, anim("a_bird_raven_flyA", None, 1.0)?, None, 60.0, 1);
            }
            Theme::ForestNothing => {
                add(Kind::Worker(PostKind::Lumber), work("u_vil_male_lumberjack_taskA")?, None, 0.0, 4);
                add(Kind::Worker(PostKind::Lumber), work("u_vil_female_lumberjack_taskA")?, None, 0.0, 3);
            }
            Theme::Islands => {
                let (w, i) = sheep()?;
                add(Kind::Herd, w, i, 18.0, 3);
                for (name, n) in [("u_shp_fishing_ship", 1), ("u_shp_trade_cog", 1), ("u_shp_transport_ship", 1)] {
                    add(Kind::Ship, anim(name, Some(BLUE), 1.0)?, None, 22.0, n);
                }
                add(Kind::Fisher, anim("u_shp_fishing_ship_gather", Some(BLUE), 1.0)?, None, 0.0, 4);
                add(Kind::Bird, bird("a_bird_stork_flyA")?, None, 50.0, 1);
            }
            Theme::Nomad => {
                // two at the Town Center, one at the dock
                add(Kind::Worker(PostKind::Builder), work("u_vil_male_builder_taskA")?, None, 0.0, 2);
                add(Kind::Worker(PostKind::Builder), work("u_vil_female_builder_taskA")?, None, 0.0, 1);
                let (w, i) = sheep()?;
                add(Kind::Herd, w, i, 18.0, 4);
                let (w, i) = deer()?;
                add(Kind::Roamer, w, i, 26.0, 2);
                add(Kind::Bird, bird("a_bird_hawk_flyA")?, None, 55.0, 1);
            }
            Theme::GoldRush => {
                miners(&mut add, 9, 7)?;
                // up to eight wolves; the villagers they jumped swing back
                add(Kind::Fighter(true), anim("a_pred_wolf_attackA", None, 0.9)?, None, 0.0, 8);
                add(Kind::Fighter(false), anim("u_vil_male_villager_attackA", Some(BLUE), 1.1)?, None, 0.0, 3);
                add(Kind::Fighter(false), anim("u_vil_female_villager_attackA", Some(BLUE), 1.1)?, None, 0.0, 2);
                let (w, i) = sheep()?;
                add(Kind::Herd, w, i, 18.0, 3);
                let (w, i) = deer()?;
                add(Kind::Roamer, w, i, 26.0, 2);
                add(Kind::Bird, bird("a_bird_hawk_flyA")?, None, 55.0, 1);
            }
            Theme::Fortress => {
                let (w, i) = unit("u_vil_male_villager_walkA", "u_vil_male_villager_idleA", BLUE)?;
                add(Kind::Herd, w, i, 22.0, 5);
                let (w, i) = unit("u_vil_female_villager_walkA", "u_vil_female_villager_idleA", BLUE)?;
                add(Kind::Herd, w, i, 22.0, 4);
                let (w, i) = unit("u_inf_manatarms_walkA", "u_inf_manatarms_idleA", BLUE)?;
                add(Kind::Roamer, w, i, 22.0, 3);
                let (w, i) = unit("u_inf_longswordsman_walkA", "u_inf_longswordsman_idleA", BLUE)?;
                add(Kind::Roamer, w, i, 22.0, 2);
                let (w, i) = unit("u_cav_knight_walkA", "u_cav_knight_idleA", BLUE)?;
                add(Kind::Roamer, w, i, 34.0, 2);
                let (w, i) = unit("u_monk_west_walkA", "u_monk_west_idleA", BLUE)?;
                add(Kind::Herd, w, i, 16.0, 2);
                let (w, i) = unit("u_trade_cart_west_walkA", "u_trade_cart_west_idleA", BLUE)?;
                add(Kind::Roamer, w, i, 20.0, 2);
                miners(&mut add, 2, 2)?;
                let (w, i) = sheep()?;
                add(Kind::Herd, w, i, 18.0, 3);
                add(Kind::Bird, bird("a_bird_hawk_flyA")?, None, 55.0, 1);
            }
        }
        Ok(Kit { species: sp, parade, ds })
    }
}

fn dir_of(dx: f32, dy: f32) -> usize {
    let a = dy.atan2(dx).rem_euclid(TAU);
    ((a / (TAU / 16.0)).round() as usize) % 16
}

/// Straight path from a to b only crosses cells of class `want`.
fn clear_path(scene: &Scene, ax: f32, ay: f32, bx: f32, by: f32, want: u8) -> bool {
    let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
    let steps = (len / 6.0).ceil().max(1.0) as usize;
    (0..=steps).all(|k| {
        let t = k as f32 / steps as f32;
        scene.cell(ax + (bx - ax) * t, ay + (by - ay) * t) == want
    })
}

/// Straight path from a to b only crosses free land (or leaves the screen).
fn free_path(scene: &Scene, a: (f32, f32), b: (f32, f32)) -> bool {
    let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let steps = (len / 6.0).ceil().max(1.0) as usize;
    (0..=steps).all(|k| {
        let t = k as f32 / steps as f32;
        scene.is_free(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    })
}

/// A path from beyond one edge of the screen to beyond another that stays on open land, around
/// trees, resources and icons: a breadth-first search over the scene's free cells, smoothed into a
/// few straight runs. It prefers the opposite edge, and falls back to any other edge when a column
/// of icons closes one side.
fn crossing(scene: &Scene, rng: &mut Rng) -> Option<Vec<(f32, f32)>> {
    let (gw, gh) = (scene.gw, scene.gh);
    let free = |x: usize, y: usize| scene.free[y * gw + x];
    let centre = |x: usize, y: usize| ((x as f32 + 0.5) * CELL, (y as f32 + 0.5) * CELL);
    // edges: 0 left, 1 right, 2 top, 3 bottom
    let on_edge = |e: usize, x: usize, y: usize| match e {
        0 => x == 0,
        1 => x == gw - 1,
        2 => y == 0,
        _ => y == gh - 1,
    };
    for attempt in 0..8 {
        // mostly across the width, sometimes from top to bottom
        let start = if rng.f() < 0.75 { rng.below(2) } else { 2 + rng.below(2) };
        let opposite = start ^ 1;
        let goal = |x: usize, y: usize| {
            if attempt < 4 {
                on_edge(opposite, x, y)
            } else {
                (0..4).any(|e| e != start && on_edge(e, x, y))
            }
        };
        // enter somewhere in a random band of the edge, so the route changes from one chase to the next
        let len = if start < 2 { gh } else { gw };
        let mid = rng.range(0.1, 0.9) * len as f32;
        let band = len as f32 * if attempt < 2 { 0.15 } else { 0.5 };
        let mut prev = vec![u32::MAX; gw * gh];
        let mut queue = std::collections::VecDeque::new();
        for k in 0..len {
            let (x, y) = match start {
                0 => (0, k),
                1 => (gw - 1, k),
                2 => (k, 0),
                _ => (k, gh - 1),
            };
            if (k as f32 - mid).abs() <= band && free(x, y) {
                prev[y * gw + x] = (y * gw + x) as u32;
                queue.push_back((x, y));
            }
        }
        let mut end = None;
        while let Some((x, y)) = queue.pop_front() {
            if goal(x, y) && !on_edge(start, x, y) {
                end = Some((x, y));
                break;
            }
            for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= gw as i32 || ny >= gh as i32 {
                    continue;
                }
                let (nx, ny) = (nx as usize, ny as usize);
                // diagonal steps may not cut a blocked corner
                if prev[ny * gw + nx] != u32::MAX || !free(nx, ny) || (dx != 0 && dy != 0 && (!free(nx, y) || !free(x, ny))) {
                    continue;
                }
                prev[ny * gw + nx] = (y * gw + x) as u32;
                queue.push_back((nx, ny));
            }
        }
        let Some((mut x, mut y)) = end else { continue };
        let mut cells = vec![centre(x, y)];
        while prev[y * gw + x] as usize != y * gw + x {
            let p = prev[y * gw + x] as usize;
            (x, y) = (p % gw, p / gw);
            cells.push(centre(x, y));
        }
        cells.reverse();
        if cells.len() < gw.min(gh) / 2 {
            continue; // cut across a corner: too short to read as a chase
        }
        // string pulling: keep only the points needed to stay on open land
        let mut pts = vec![cells[0]];
        let mut i = 0;
        while i < cells.len() - 1 {
            let mut j = cells.len() - 1;
            while j > i + 1 && !free_path(scene, cells[i], cells[j]) {
                j -= 1;
            }
            pts.push(cells[j]);
            i = j;
        }
        // run on straight out past the edge the path touches
        let (w, h) = (scene.w as f32, scene.h as f32);
        let off = |p: (f32, f32)| {
            let m = CELL * 1.5;
            if p.0 <= m {
                (-120.0, p.1)
            } else if p.0 >= w - m {
                (w + 120.0, p.1)
            } else if p.1 <= m {
                (p.0, -120.0)
            } else {
                (p.0, h + 120.0)
            }
        };
        let (first, last) = (pts[0], *pts.last().unwrap());
        pts.insert(0, off(first));
        pts.push(off(last));
        return Some(pts);
    }
    None
}

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

impl World {
    pub fn new(kit: Kit, scene: &Scene, seed: u64) -> World {
        let rng = Rng::new(seed ^ 0xa11);
        let parade = kit.parade.map(|sp| Parade { sp, stage: 0, wait: 0.5, run: None });
        let mut w = World { species: kit.species, actors: Vec::new(), rng, ds: kit.ds, parade };
        w.populate(scene);
        w
    }

    fn is_static(kind: Kind) -> bool {
        matches!(kind, Kind::Worker(_) | Kind::Fisher | Kind::Fighter(_))
    }

    /// Re-place every unit for a freshly built scene.
    pub fn populate(&mut self, scene: &Scene) {
        self.actors.clear();
        for si in 0..self.species.len() {
            let (kind, count) = (self.species[si].kind, self.species[si].count);
            if Self::is_static(kind) || kind == Kind::Parade {
                continue;
            }
            let mut placed = 0;
            for attempt in 0..400 {
                if placed >= count {
                    break;
                }
                let (x, y) = (self.rng.f() * scene.w as f32, self.rng.f() * scene.h as f32);
                let ok = match kind {
                    // near the icons: outside them on land maps, inland on the islands
                    Kind::Herd => {
                        let d = scene.dist_at(x, y).abs() / self.ds;
                        scene.cell(x, y) == OPEN && d < 160.0 && (d > 20.0 || attempt > 200)
                    }
                    // small clearings may lie mostly under the icons: settle for any open ground
                    Kind::Roamer => scene.cell(x, y) == OPEN && (scene.dist_at(x, y) > 12.0 * self.ds || attempt > 250),
                    Kind::Ship => scene.cell(x, y) == DEEP,
                    _ => true,
                };
                if !ok {
                    continue;
                }
                let t = self.rng.f() * 10.0;
                let mut a = Actor { sp: si, x, y, home: (x, y), dir: self.rng.below(16), state: State::Idle(self.rng.range(0.5, 6.0)), t, vx: 0.0, vy: 0.0 };
                if kind == Kind::Bird {
                    // start off-screen, fly in after a while
                    a.state = State::Idle(self.rng.range(3.0, 20.0));
                    a.x = -1e4;
                }
                self.actors.push(a);
                placed += 1;
            }
        }
        self.place_static(scene);
    }

    /// Villagers at their work posts, shoals of fish and the fishing ships working them.
    fn place_static(&mut self, scene: &Scene) {
        let species = &self.species;
        self.actors.retain(|a| !Self::is_static(species[a.sp].kind));
        let ds = self.ds;
        // shuffle the posts once so workers spread over the whole map
        let mut order: Vec<usize> = (0..scene.posts.len()).collect();
        for i in (1..order.len()).rev() {
            order.swap(i, self.rng.below(i + 1));
        }
        let mut used = vec![false; scene.posts.len()];
        let mut spots: Vec<(f32, f32)> = Vec::new();
        for si in 0..self.species.len() {
            let (kind, count) = (self.species[si].kind, self.species[si].count);
            let t = self.rng.f() * 10.0;
            match kind {
                Kind::Worker(pk) => {
                    let mut n = 0;
                    for &pi in &order {
                        if n >= count {
                            break;
                        }
                        let p = scene.posts[pi];
                        if used[pi] || p.kind != pk {
                            continue;
                        }
                        used[pi] = true;
                        n += 1;
                        let dir = dir_of(p.fx - p.x, p.fy - p.y);
                        self.actors.push(Actor { sp: si, x: p.x, y: p.y, home: (p.x, p.y), dir, state: State::Idle(f32::MAX), t: t + n as f32 * 0.37, vx: 0.0, vy: 0.0 });
                    }
                }
                Kind::Fisher => {
                    // fishing ships at work in the shallows near the islands, well apart
                    let mut n = 0;
                    for _ in 0..800 {
                        if n >= count {
                            break;
                        }
                        let (x, y) = (self.rng.range(0.04, 0.96) * scene.w as f32, self.rng.range(0.06, 0.96) * scene.h as f32);
                        let d = scene.dist_at(x, y) / ds;
                        if !matches!(scene.cell(x, y), WATER | DEEP) || !(60.0..130.0).contains(&d) || spots.iter().any(|f| (f.0 - x).powi(2) + (f.1 - y).powi(2) < (220.0 * ds).powi(2)) {
                            continue;
                        }
                        spots.push((x, y));
                        n += 1;
                        let dir = self.rng.below(16);
                        self.actors.push(Actor { sp: si, x, y, home: (x, y), dir, state: State::Idle(f32::MAX), t: t + n as f32 * 0.53, vx: 0.0, vy: 0.0 });
                    }
                }
                _ => {}
            }
        }
        self.place_fight(scene);
    }

    /// Gold Rush: a pack of six to eight wolves set on a few villagers, all on one side of the
    /// map, on open ground away from the icons. Each villager faces a wolf; the wolves circle in.
    fn place_fight(&mut self, scene: &Scene) {
        let wolves: Vec<usize> = (0..self.species.len()).filter(|&i| self.species[i].kind == Kind::Fighter(true)).collect();
        let vils: Vec<usize> = (0..self.species.len()).filter(|&i| self.species[i].kind == Kind::Fighter(false)).collect();
        let (Some(&wolf), false) = (wolves.first(), vils.is_empty()) else { return };
        let ds = self.ds;
        let (sw, sh) = (scene.w as f32, scene.h as f32);
        let ok = |x: f32, y: f32| x > 20.0 && y > 40.0 && x < sw - 20.0 && y < sh - 10.0 && scene.is_free(x, y);
        // villagers in a loose knot, the pack around them
        let n_vil: usize = vils.iter().map(|&v| self.species[v].count).sum::<usize>().min(5);
        let n_wolf = (6 + self.rng.below(3)).min(self.species[wolf].count);
        let right = self.rng.f() < 0.5;
        for attempt in 0..600 {
            // one side of the map; the other side once the first has had its chance
            let side = if attempt < 400 { right } else { !right };
            let x = if side { self.rng.range(0.55, 0.95) } else { self.rng.range(0.05, 0.45) } * sw;
            let y = self.rng.range(0.15, 0.9) * sh;
            if scene.dist_at(x, y) < 70.0 * ds {
                continue;
            }
            let vil_at: Vec<(f32, f32)> = (0..n_vil)
                .map(|k| {
                    let a = k as f32 / n_vil as f32 * TAU + 0.4;
                    let r = if k == 0 { 0.0 } else { 34.0 * ds };
                    (x + a.cos() * r, y + a.sin() * r * 0.5)
                })
                .collect();
            if !vil_at.iter().all(|&(vx, vy)| ok(vx, vy)) {
                continue;
            }
            // each wolf lunges at a villager from outside the knot
            let mut wolf_at: Vec<(f32, f32, usize)> = Vec::new();
            for k in 0..n_wolf {
                let a = k as f32 / n_wolf as f32 * TAU + self.rng.range(-0.2, 0.2);
                let (wx, wy) = (x + a.cos() * 82.0 * ds, y + a.sin() * 41.0 * ds);
                // nearest villager is its target
                let tgt = (0..n_vil).min_by(|&i, &j| {
                    let di = (vil_at[i].0 - wx).powi(2) + (vil_at[i].1 - wy).powi(2);
                    let dj = (vil_at[j].0 - wx).powi(2) + (vil_at[j].1 - wy).powi(2);
                    di.total_cmp(&dj)
                });
                let Some(tgt) = tgt else { continue };
                if ok(wx, wy) {
                    wolf_at.push((wx, wy, tgt));
                }
            }
            if wolf_at.len() < 6.min(n_wolf) {
                continue;
            }
            for &(wx, wy, tgt) in &wolf_at {
                let (vx, vy) = vil_at[tgt];
                let t = self.rng.f() * 10.0;
                self.actors.push(Actor { sp: wolf, x: wx, y: wy, home: (wx, wy), dir: dir_of(vx - wx, vy - wy), state: State::Idle(f32::MAX), t, vx: 0.0, vy: 0.0 });
            }
            let mut which = vils.iter().flat_map(|&v| std::iter::repeat(v).take(self.species[v].count)).collect::<Vec<_>>();
            for i in (1..which.len()).rev() {
                which.swap(i, self.rng.below(i + 1));
            }
            for (k, &(vx, vy)) in vil_at.iter().enumerate() {
                // face the closest wolf
                let foe = wolf_at.iter().min_by(|p, q| ((p.0 - vx).powi(2) + (p.1 - vy).powi(2)).total_cmp(&((q.0 - vx).powi(2) + (q.1 - vy).powi(2))));
                let dir = foe.map_or(0, |f| dir_of(f.0 - vx, f.1 - vy));
                let t = self.rng.f() * 10.0;
                self.actors.push(Actor { sp: which[k], x: vx, y: vy, home: (vx, vy), dir, state: State::Idle(f32::MAX), t, vx: 0.0, vy: 0.0 });
            }
            return;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.actors.is_empty() && self.parade.is_none()
    }

    /// After the icons moved: keep units that are still on valid ground, re-place the rest.
    /// Workers, fish and fishing ships follow the new scene's work posts and shores.
    pub fn revalidate(&mut self, scene: &Scene) {
        let species = &self.species;
        let stale = self.actors.iter().any(|a| {
            let want = match species[a.sp].kind {
                Kind::Ship => DEEP,
                Kind::Herd | Kind::Roamer => OPEN,
                _ => return false,
            };
            scene.cell(a.x, a.y) != want
        });
        if stale {
            self.populate(scene);
        } else {
            self.place_static(scene);
        }
        if let Some(p) = &mut self.parade {
            // a chase in progress may now cross a new tree line: let it finish off-screen and restart
            if p.run.as_ref().is_some_and(|r| !r.line.windows(2).all(|q| free_path(scene, q[0], q[1]))) {
                p.run = None;
                p.stage = 0;
                p.wait = 1.0;
            }
        }
    }

    /// All decoded frames, so the caller can release their GPU copies.
    pub fn images(&self) -> Vec<Arc<RenderImage>> {
        self.species
            .iter()
            .flat_map(|s| s.walk.frames.iter().chain(s.idle.iter().flat_map(|i| i.frames.iter())))
            .map(|f| f.img.clone())
            .collect()
    }

    fn step_parade(&mut self, scene: &Scene, dt: f32) {
        let ds = self.ds;
        let Some(p) = &mut self.parade else { return };
        if let Some(run) = &mut p.run {
            run.s += run.speed * dt;
            run.t = (run.t + dt) % CLOCK_WRAP;
            let last = run.members.iter().map(|m| m.1).fold(0.0, f32::max);
            if run.s - last > *run.acc.last().unwrap() {
                p.run = None;
                if p.stage == 0 {
                    // the scouts are gone: the militia come running a moment later
                    p.stage = 1;
                    p.wait = 2.5;
                } else {
                    // and round again: the scouts come back after a short breather
                    p.stage = 0;
                    p.wait = self.rng.range(3.0, 6.0);
                }
            }
            return;
        }
        p.wait -= dt;
        if p.wait > 0.0 {
            return;
        }
        let Some(line) = crossing(scene, &mut self.rng) else {
            p.wait = 2.0;
            return;
        };
        let [red, blue, militia, wolf] = p.sp;
        let (members, speed) = if p.stage == 0 {
            (vec![(red, 0.0, 0.0), (blue, 75.0 * ds, 0.0)], 92.0 * ds)
        } else {
            (vec![(militia, 0.0, -12.0 * ds), (militia, 24.0 * ds, 12.0 * ds), (militia, 46.0 * ds, -3.0 * ds), (wolf, 120.0 * ds, 0.0)], 62.0 * ds)
        };
        let acc = arc(&line);
        p.run = Some(Run { line, acc, members, s: 0.0, speed, t: 0.0 });
    }

    pub fn step(&mut self, scene: &Scene, dt: f32) {
        self.step_parade(scene, dt);
        let (sw, sh) = (scene.w as f32, scene.h as f32);
        for a in &mut self.actors {
            let sp = &self.species[a.sp];
            a.t = (a.t + dt) % CLOCK_WRAP;
            match sp.kind {
                Kind::Worker(_) | Kind::Fisher | Kind::Fighter(_) | Kind::Parade => {}
                Kind::Bird => match a.state {
                    State::Idle(ref mut left) => {
                        *left -= dt;
                        if *left <= 0.0 {
                            // enter from a random edge, heading across the screen
                            let dir = self.rng.below(16);
                            let ang = dir as f32 * TAU / 16.0;
                            let (vx, vy) = (ang.cos(), ang.sin());
                            let (cx, cy) = (sw * self.rng.range(0.3, 0.7), sh * self.rng.range(0.3, 0.7));
                            let back = (sw + sh) * 0.75;
                            a.x = cx - vx * back;
                            a.y = cy - vy * back;
                            a.vx = vx * sp.speed;
                            a.vy = vy * sp.speed;
                            a.dir = dir;
                            a.state = State::Walk { tx: 0.0, ty: 0.0 };
                        }
                    }
                    State::Walk { .. } => {
                        a.x += a.vx * dt;
                        a.y += a.vy * dt;
                        let out = a.x < -300.0 || a.y < -300.0 || a.x > sw + 300.0 || a.y > sh + 300.0;
                        let heading_out = (a.x < -300.0 && a.vx < 0.0) || (a.x > sw + 300.0 && a.vx > 0.0) || (a.y < -300.0 && a.vy < 0.0) || (a.y > sh + 300.0 && a.vy > 0.0);
                        if out && heading_out {
                            a.state = State::Idle(self.rng.range(15.0, 45.0));
                            a.x = -1e4;
                        }
                    }
                },
                _ => match a.state {
                    State::Idle(ref mut left) => {
                        *left -= dt;
                        if *left <= 0.0 {
                            let (want, radius) = match sp.kind {
                                Kind::Herd => (OPEN, 110.0),
                                Kind::Roamer => (OPEN, 260.0),
                                _ => (DEEP, 420.0),
                            };
                            let radius = radius * self.ds;
                            let mut next = State::Idle(self.rng.range(1.0, 3.0));
                            for _ in 0..12 {
                                let (bx, by) = if sp.kind == Kind::Herd { a.home } else { (a.x, a.y) };
                                let tx = bx + self.rng.range(-radius, radius);
                                let ty = by + self.rng.range(-radius, radius) * 0.6;
                                if tx < 8.0 || ty < 8.0 || tx > sw - 8.0 || ty > sh - 8.0 {
                                    continue;
                                }
                                if clear_path(scene, a.x, a.y, tx, ty, want) {
                                    next = State::Walk { tx, ty };
                                    break;
                                }
                            }
                            a.state = next;
                        }
                    }
                    State::Walk { tx, ty } => {
                        let (dx, dy) = (tx - a.x, ty - a.y);
                        let dist = (dx * dx + dy * dy).sqrt();
                        let stepd = sp.speed * dt;
                        if dist <= stepd {
                            a.x = tx;
                            a.y = ty;
                            let rest = if sp.kind == Kind::Ship { self.rng.range(4.0, 14.0) } else { self.rng.range(2.0, 9.0) };
                            a.state = State::Idle(rest);
                        } else {
                            a.x += dx / dist * stepd;
                            a.y += dy / dist * stepd;
                            a.dir = dir_of(dx, dy);
                        }
                    }
                },
            }
        }
    }

    /// Developer summary: units per kind and the chase state.
    pub fn stats(&self) -> String {
        let mut n = [0usize; 8];
        for a in &self.actors {
            let k = match self.species[a.sp].kind {
                Kind::Herd => 0,
                Kind::Roamer => 1,
                Kind::Bird => 2,
                Kind::Ship => 3,
                Kind::Worker(_) => 4,
                Kind::Fisher => 6,
                Kind::Parade | Kind::Fighter(_) => 7,
            };
            n[k] += 1;
        }
        let chase = match &self.parade {
            None => "none".to_string(),
            Some(p) => match &p.run {
                Some(r) => format!("stage {} running at {:.0}/{:.0} px, {} units visible", p.stage, r.s, r.acc.last().unwrap(), self.visible().len()),
                None => format!("stage {} waiting {:.1}s", p.stage, p.wait),
            },
        };
        format!("herd {} roam {} bird {} ship {} workers {} fishers {} fighters {} | chase: {chase}", n[0], n[1], n[2], n[3], n[4], n[6], n[7])
    }

    /// Frames to paint this tick, sorted back to front: (x, y) is the hotspot.
    pub fn visible(&self) -> Vec<(f32, f32, &Frame)> {
        let mut out: Vec<(bool, f32, f32, &Frame)> = Vec::with_capacity(self.actors.len() + 4);
        for a in &self.actors {
            if a.x < -5e3 {
                continue;
            }
            let sp = &self.species[a.sp];
            let anim = match (&a.state, &sp.idle) {
                (State::Idle(_), Some(idle)) => idle,
                _ => &sp.walk,
            };
            out.push((sp.kind == Kind::Bird, a.x, a.y, anim.frame(a.dir, a.t)));
        }
        if let Some(run) = self.parade.as_ref().and_then(|p| p.run.as_ref()) {
            let total = *run.acc.last().unwrap();
            for (i, &(si, lag, side)) in run.members.iter().enumerate() {
                let s = run.s - lag;
                if s < 0.0 || s > total {
                    continue;
                }
                let (a, b) = (point_at(&run.line, &run.acc, (s - 6.0).max(0.0)), point_at(&run.line, &run.acc, (s + 6.0).min(total)));
                let (tx, ty) = (b.0 - a.0, b.1 - a.1);
                let len = (tx * tx + ty * ty).sqrt().max(1e-3);
                let (x, y) = point_at(&run.line, &run.acc, s);
                let (x, y) = (x - ty / len * side, y + tx / len * side);
                out.push((false, x, y, self.species[si].walk.frame(dir_of(tx, ty), run.t + i as f32 * 0.13)));
            }
        }
        // ground units back to front, birds above everything
        out.sort_by(|p, q| p.0.cmp(&q.0).then(p.2.total_cmp(&q.2)));
        out.into_iter().map(|(_, x, y, f)| (x, y, f)).collect()
    }
}
