//! Animated units: grazing herds, wandering deer and wolves, birds overhead and ships at sea.
//! Each one is a few small pre-decoded frames; the per-frame cost is a position update.
use std::f32::consts::TAU;
use std::sync::Arc;

use anyhow::Result;
use gpui::RenderImage;

use crate::assets::{Game, Img};
use crate::noise::Rng;
use crate::scene::{Scene, Theme, DEEP, OPEN};

/// Frames per direction kept in memory (the game ships 30-60).
const MAX_PER_DIR: usize = 12;

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
        let total = imgs.len() / dirs;
        let per_dir = total.clamp(1, MAX_PER_DIR);
        let mut frames = Vec::with_capacity(dirs * per_dir);
        for d in 0..dirs {
            for k in 0..per_dir {
                let img = &imgs[d * total + k * total / per_dir];
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
        Anim { dirs, per_dir, frames, fps: per_dir as f32 / cycle_secs }
    }

    pub fn frame(&self, dir: usize, t: f32) -> &Frame {
        let k = (t * self.fps) as usize % self.per_dir;
        &self.frames[(dir % self.dirs) * self.per_dir + k]
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

pub struct World {
    species: Vec<Species>,
    actors: Vec<Actor>,
    rng: Rng,
    ds: f32,
}

pub struct Kit {
    species: Vec<Species>,
    ds: f32,
}

impl Kit {
    pub fn load(game: &Game, theme: Theme, ds: f32) -> Result<Kit> {
        let s = |name: &str, cycle: f32| -> Result<Anim> { Ok(Anim::from_imgs(game.sprite(name, ds)?, cycle)) };
        let mut species = Vec::new();
        let herd = |species: &mut Vec<Species>, walk: &str, idle: &str, kind: Kind, speed: f32, count: usize| -> Result<()> {
            species.push(Species { kind, walk: s(walk, 1.0)?, idle: Some(s(idle, 4.0)?), speed: speed * ds, count });
            Ok(())
        };
        match theme {
            Theme::Arabia => {
                herd(&mut species, "a_herd_sheep_walkA", "a_herd_sheep_idleA", Kind::Herd, 18.0, 5)?;
                herd(&mut species, "a_hunt_deer_walkA", "a_hunt_deer_idleA", Kind::Roamer, 26.0, 3)?;
                species.push(Species { kind: Kind::Bird, walk: s("a_bird_hawk_flyA", 1.2)?, idle: None, speed: 55.0 * ds, count: 1 });
            }
            Theme::Arena => {
                herd(&mut species, "a_herd_sheep_walkA", "a_herd_sheep_idleA", Kind::Herd, 18.0, 6)?;
                species.push(Species { kind: Kind::Bird, walk: s("a_bird_hawk_flyA", 1.2)?, idle: None, speed: 55.0 * ds, count: 1 });
            }
            Theme::BlackForest => {
                herd(&mut species, "a_hunt_deer_walkA", "a_hunt_deer_idleA", Kind::Roamer, 26.0, 4)?;
                herd(&mut species, "a_pred_wolf_walkA", "a_pred_wolf_idleA", Kind::Roamer, 30.0, 2)?;
                species.push(Species { kind: Kind::Bird, walk: s("a_bird_raven_flyA", 1.0)?, idle: None, speed: 60.0 * ds, count: 1 });
            }
            Theme::Islands => {
                herd(&mut species, "a_herd_sheep_walkA", "a_herd_sheep_idleA", Kind::Herd, 18.0, 3)?;
                for (name, n) in [("u_shp_fishing_ship", 2), ("u_shp_trade_cog", 1), ("u_shp_transport_ship", 1)] {
                    species.push(Species { kind: Kind::Ship, walk: s(name, 1.0)?, idle: None, speed: 22.0 * ds, count: n });
                }
                species.push(Species { kind: Kind::Bird, walk: s("a_bird_stork_flyA", 1.2)?, idle: None, speed: 50.0 * ds, count: 1 });
            }
        }
        Ok(Kit { species, ds })
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

impl World {
    pub fn new(kit: Kit, scene: &Scene, seed: u64) -> World {
        let mut w = World { species: kit.species, actors: Vec::new(), rng: Rng::new(seed ^ 0xa11), ds: kit.ds };
        w.populate(scene);
        w
    }

    /// Re-place every unit for a freshly built scene.
    pub fn populate(&mut self, scene: &Scene) {
        self.actors.clear();
        for si in 0..self.species.len() {
            let (kind, count) = (self.species[si].kind, self.species[si].count);
            let mut placed = 0;
            for _ in 0..400 {
                if placed >= count {
                    break;
                }
                let (x, y) = (self.rng.f() * scene.w as f32, self.rng.f() * scene.h as f32);
                let ok = match kind {
                    Kind::Herd => scene.cell(x, y) == OPEN && (20.0..160.0).contains(&(scene.dist_at(x, y) / self.ds)),
                    Kind::Roamer => scene.cell(x, y) == OPEN && scene.dist_at(x, y) > 60.0 * self.ds,
                    Kind::Ship => scene.cell(x, y) == DEEP,
                    Kind::Bird => true,
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
    }

    pub fn is_empty(&self) -> bool {
        self.actors.is_empty()
    }

    /// After the icons moved: keep units that are still on valid ground, re-place the rest.
    pub fn revalidate(&mut self, scene: &Scene) {
        let species = &self.species;
        let stale = self.actors.iter().any(|a| {
            let want = match species[a.sp].kind {
                Kind::Bird => return false,
                Kind::Ship => DEEP,
                _ => OPEN,
            };
            scene.cell(a.x, a.y) != want
        });
        if stale {
            self.populate(scene);
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

    pub fn step(&mut self, scene: &Scene, dt: f32) {
        let (sw, sh) = (scene.w as f32, scene.h as f32);
        for a in &mut self.actors {
            let sp = &self.species[a.sp];
            a.t += dt;
            match sp.kind {
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

    /// Frames to paint this tick, sorted back to front: (x, y) is the hotspot.
    pub fn visible(&self) -> Vec<(f32, f32, &Frame)> {
        let mut out: Vec<(bool, f32, f32, &Frame)> = Vec::with_capacity(self.actors.len());
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
        // ground units back to front, birds above everything
        out.sort_by(|p, q| p.0.cmp(&q.0).then(p.2.total_cmp(&q.2)));
        out.into_iter().map(|(_, x, y, f)| (x, y, f)).collect()
    }
}
