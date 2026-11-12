//! Icon Wars: the desktop icons are buildings under attack. Waves of the red army march in from
//! the screen edges away from the icons, hack at the icons until they collapse into rubble and move
//! on to the next one. A fallen icon stays in ruins until another map is picked. Once the last
//! icon is down, villagers walk in and build a castle on the conquered desktop while the army
//! stands guard around it.
//!
//! The battle only keeps the state (icon hit points, who is falling, who is rubble); the overlay
//! windows in [`crate::iconfx`] draw the damage on top of the real icons.
use std::sync::Arc;

use anyhow::Result;
use gpui::RenderImage;

use crate::actors::{Anim, Frame, dir_of};
use crate::assets::{Game, Img, RED};
use crate::noise::Rng;
use crate::scene::{CELL, Rect, Scene};

/// Soldiers alive per side at most.
const CAP: usize = 14;
/// Seconds a body lies on the field before it is gone.
const ROT: f32 = 10.0;
/// Hit points of a desktop icon.
pub const ICON_HP: f32 = 220.0;
/// Seconds an icon takes to collapse.
pub const FALL: f32 = 1.4;
/// How far a soldier sees an enemy soldier worth turning aside for (unscaled px).
const SIGHT: f32 = 160.0;

/// Unit types: sprite stem, speed (px/s), hit points, damage range, reach (px), seconds per swing
/// and how often the type turns up in a wave.
const TROOPS: &[(&str, f32, f32, (f32, f32), f32, f32, f32)] = &[
    ("u_inf_manatarms", 26.0, 55.0, (6.0, 9.0), 22.0, 1.0, 3.0),
    ("u_cav_knight", 40.0, 100.0, (9.0, 12.0), 28.0, 1.0, 1.5),
    ("u_arc_archer", 24.0, 30.0, (4.0, 6.0), 170.0, 1.4, 2.0),
];

struct Troop {
    walk: Anim,
    idle: Anim,
    attack: Anim,
    death: Anim,
    decay: Anim,
    speed: f32,
    hp: f32,
    dmg: (f32, f32),
    reach: f32,
    cycle: f32,
    weight: f32,
}

pub struct Kit {
    /// Per side: the unit types in that player colour (the attackers are side 0).
    troops: [Vec<Troop>; 2],
    /// Left where an icon fell.
    rubble: Option<Img>,
    /// The game's building fire, small to large, each a looping animation.
    flames: Vec<Vec<Img>>,
    /// Villagers who build the castle at the end (a man and a woman).
    builders: Vec<Builder>,
    /// The castle at each stage of building, from its first course of stone to finished, as the
    /// directions of one animation.
    castle: Option<Anim>,
    ds: f32,
}

struct Builder {
    walk: Anim,
    task: Anim,
    idle: Anim,
}

/// Courses the castle goes up in.
const STAGES: usize = 14;
/// Seconds the whole crew takes to build it.
const BUILD: f32 = 32.0;
/// Villagers on the crew.
const CREW: usize = 6;

impl Kit {
    /// The building fire, small to large (developer previews).
    pub fn flames(&self) -> &[Vec<Img>] {
        &self.flames
    }

    /// The rubble sprite (developer previews).
    pub fn rubble_img(&self) -> Option<Img> {
        self.rubble.clone()
    }

    pub fn load(game: &Game, ds: f32) -> Result<Kit> {
        let mut troops: [Vec<Troop>; 2] = [Vec::new(), Vec::new()];
        // only the red army fights; the icons are the other side
        for (side, pc) in [RED].into_iter().enumerate() {
            let anim = |name: String, cycle: f32, cap: usize| -> Result<Anim> {
                let imgs = game.sprite_pc(&name, ds, Some(pc))?;
                let dirs = if imgs.len() >= 16 { 16 } else { 1 };
                Ok(Anim::with_dirs_cap(imgs, dirs, cycle, cap))
            };
            for &(stem, speed, hp, dmg, reach, cycle, weight) in TROOPS {
                troops[side].push(Troop {
                    walk: anim(format!("{stem}_walkA"), 1.0, 10)?,
                    idle: anim(format!("{stem}_idleA"), 2.0, 10)?,
                    attack: anim(format!("{stem}_attackA"), cycle, 10)?,
                    death: anim(format!("{stem}_deathA"), 1.1, 8)?,
                    decay: anim(format!("{stem}_decayA"), ROT, 3)?,
                    speed: speed * ds,
                    hp,
                    dmg,
                    reach: reach * ds,
                    cycle,
                    weight,
                });
            }
        }
        let rubble = game.sprite("s_rubble_1x1", ds).ok().and_then(|v| v.into_iter().next());
        // a burning icon gets one fire, bigger as it weakens; without the flames it just doesn't burn
        let flames: Vec<Vec<Img>> = ["FLM1A", "FLM2A", "FLM3A"]
            .iter()
            .filter_map(|n| game.smp_sprite(n, ds).map_err(|e| crate::log(&format!("icon wars: no {n}: {e:#}"))).ok())
            .filter(|f| !f.is_empty())
            .collect();
        let mut builders = Vec::new();
        for sex in ["male", "female"] {
            let anim = |what: &str, cycle: f32| -> Result<Anim> {
                let imgs = game.sprite_pc(&format!("u_vil_{sex}_builder_{what}A"), ds, Some(RED))?;
                let dirs = if imgs.len() >= 16 { 16 } else { 1 };
                Ok(Anim::with_dirs_cap(imgs, dirs, cycle, 10))
            };
            builders.push(Builder { walk: anim("walk", 1.0)?, task: anim("task", 1.4)?, idle: anim("idle", 2.0)? });
        }
        // the castle rises from the ground up: each stage shows a little more of it
        let castle = match game.sprite_pc("b_west_castle_age3", ds * 0.8, Some(RED)) {
            Ok(v) if !v.is_empty() => {
                let full = v.into_iter().next().unwrap_or_else(|| unreachable!());
                let stages = (1..=STAGES)
                    .map(|k| {
                        let mut img = full.clone();
                        let from = img.h - img.h * k / STAGES;
                        for p in &mut img.px[..from * img.w] {
                            *p = [0, 0, 0, 0];
                        }
                        img
                    })
                    .collect();
                Some(Anim::with_dirs_cap(stages, STAGES, 1.0, 1))
            }
            other => {
                crate::log(&format!("icon wars: no castle sprite ({})", other.err().map_or("empty".into(), |e| format!("{e:#}"))));
                None
            }
        };
        Ok(Kit { troops, rubble, flames, builders, castle, ds })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Act {
    /// Walking towards an enemy or an icon; standing still when `moving` is false.
    March,
    Fight,
    /// Hacking at an enemy icon.
    Siege,
    /// Standing guard at the castle.
    Guard,
    Dying,
    Dead,
}

/// The castle built once every icon is down, and its crew.
struct Fort {
    /// Where it stands (the middle of its footprint) and the footprint's half axes.
    at: (f32, f32),
    rx: f32,
    ry: f32,
    /// 0..1: how far up it is.
    progress: f32,
    /// Where the guards stand and where the builders work.
    posts: Vec<(f32, f32)>,
    spots: Vec<(f32, f32)>,
    /// Routes to the castle around the trees.
    route: Flow,
}

struct Worker {
    kind: usize,
    x: f32,
    y: f32,
    dir: usize,
    t: f32,
    spot: usize,
    working: bool,
    moving: bool,
    lean: f32,
}

struct Soldier {
    side: usize,
    tr: usize,
    x: f32,
    y: f32,
    dir: usize,
    t: f32,
    hp: f32,
    act: Act,
    moving: bool,
    swing: f32,
    /// Which way this soldier sidesteps around obstacles (+1 or -1).
    lean: f32,
    /// After the last icon falls: which guard post around the castle is this soldier's.
    slot: Option<usize>,
}

/// What has become of an icon.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Phase {
    Standing,
    /// Collapsing, `t` seconds in.
    Falling(f32),
    /// Rubble, `t` seconds in. It stays rubble for the rest of the battle.
    Ruined(f32),
}

/// A desktop icon in the battle.
pub struct Icon {
    /// The icon's rectangle (picture and label), in scene pixels.
    pub rect: Rect,
    pub side: usize,
    pub hp: f32,
    pub phase: Phase,
    /// Seconds since it was last struck.
    pub since_hit: f32,
    /// Where its own soldiers come out and its attackers stand: open ground just outside it.
    spots: Vec<(f32, f32)>,
}

impl Icon {
    pub fn centre(&self) -> (f32, f32) {
        let r = &self.rect;
        ((r.x0 + r.x1) * 0.5, r.y0 + (r.y1 - r.y0) * 0.4)
    }
}

/// Where a route leads from a given point.
enum Way {
    Arrived,
    Next(f32, f32),
    /// Off the route (unreachable ground): head straight for the goal instead.
    Lost,
}

/// Steps to the nearest goal cell over open ground, per scene cell (a breadth-first flood).
struct Flow {
    gw: usize,
    gh: usize,
    d: Vec<u16>,
}

impl Flow {
    fn empty() -> Flow {
        Flow {
            gw: 0,
            gh: 0,
            d: Vec::new(),
        }
    }

    fn build(
        scene: &Scene,
        open: &dyn Fn(f32, f32) -> bool,
        goals: &[(f32, f32)],
        radius: f32,
    ) -> Flow {
        let (gw, gh) = (scene.gw, scene.gh);
        if goals.is_empty() {
            return Flow::empty();
        }
        let centre = |i: usize| {
            (
                ((i % gw) as f32 + 0.5) * CELL,
                ((i / gw) as f32 + 0.5) * CELL,
            )
        };
        let pass: Vec<bool> = (0..gw * gh)
            .map(|i| {
                let (x, y) = centre(i);
                open(x, y)
            })
            .collect();
        let mut d = vec![u16::MAX; gw * gh];
        let mut queue = std::collections::VecDeque::new();
        let radius = radius.max(CELL);
        for &(gx, gy) in goals {
            let r = (radius / CELL).ceil() as i32;
            let (cx, cy) = ((gx / CELL) as i32, (gy / CELL) as i32);
            for y in cy - r..=cy + r {
                for x in cx - r..=cx + r {
                    if x < 0 || y < 0 || x >= gw as i32 || y >= gh as i32 {
                        continue;
                    }
                    let i = y as usize * gw + x as usize;
                    let (px, py) = centre(i);
                    if pass[i]
                        && d[i] == u16::MAX
                        && (px - gx).powi(2) + (py - gy).powi(2) <= radius.powi(2)
                    {
                        d[i] = 0;
                        queue.push_back(i);
                    }
                }
            }
        }
        while let Some(i) = queue.pop_front() {
            let (x, y) = ((i % gw) as i32, (i / gw) as i32);
            for (dx, dy) in [
                (1i32, 0i32),
                (-1, 0),
                (0, 1),
                (0, -1),
                (1, 1),
                (1, -1),
                (-1, 1),
                (-1, -1),
            ] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= gw as i32 || ny >= gh as i32 {
                    continue;
                }
                let n = ny as usize * gw + nx as usize;
                // no cutting across a blocked corner
                let corner = dx != 0
                    && dy != 0
                    && (!pass[y as usize * gw + nx as usize]
                        || !pass[ny as usize * gw + x as usize]);
                if !pass[n] || d[n] != u16::MAX || corner {
                    continue;
                }
                d[n] = d[i] + 1;
                queue.push_back(n);
            }
        }
        Flow { gw, gh, d }
    }

    fn at(&self, x: f32, y: f32) -> u16 {
        let (cx, cy) = ((x / CELL).floor() as i64, (y / CELL).floor() as i64);
        if self.d.is_empty() || cx < 0 || cy < 0 || cx >= self.gw as i64 || cy >= self.gh as i64 {
            return u16::MAX;
        }
        self.d[cy as usize * self.gw + cx as usize]
    }

    fn way(&self, x: f32, y: f32) -> Way {
        let here = self.at(x, y);
        if here == 0 {
            return Way::Arrived;
        }
        if here == u16::MAX {
            return Way::Lost;
        }
        let (cx, cy) = ((x / CELL).floor() as i64, (y / CELL).floor() as i64);
        let mut best = (here, None);
        for (dx, dy) in [
            (1i64, 0i64),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ] {
            let (nx, ny) = (cx + dx, cy + dy);
            if nx < 0 || ny < 0 || nx >= self.gw as i64 || ny >= self.gh as i64 {
                continue;
            }
            let v = self.d[ny as usize * self.gw + nx as usize];
            if v < best.0 {
                best = (
                    v,
                    Some(((nx as f32 + 0.5) * CELL, (ny as f32 + 0.5) * CELL)),
                );
            }
        }
        match best.1 {
            Some((px, py)) => Way::Next(px, py),
            None => Way::Lost,
        }
    }
}

pub struct Battle {
    kit: Kit,
    icons: Vec<Icon>,
    /// Every icon is down: the castle going up, and the villagers building it.
    fort: Option<Fort>,
    workers: Vec<Worker>,
    /// Per side: stretches of screen edge its raiders come from (only for a side without icons).
    edges: [Vec<(f32, f32)>; 2],
    /// Seconds until each side's next wave.
    wave_in: [f32; 2],
    men: Vec<Soldier>,
    rng: Rng,
    /// Routes to the spots around each side's standing icons (followed by the other side).
    to_side: [Flow; 2],
    /// The standing icons changed: rebuild the routes.
    routes_stale: bool,
}

impl Battle {
    pub fn new(kit: Kit, scene: &Scene, seed: u64) -> Battle {
        let mut b = Battle {
            kit,
            icons: Vec::new(),
            fort: None,
            workers: Vec::new(),
            edges: [Vec::new(), Vec::new()],
            wave_in: [0.5, 99.0],
            men: Vec::new(),
            rng: Rng::new(seed ^ 0xba77),
            to_side: [Flow::empty(), Flow::empty()],
            routes_stale: true,
        };
        b.setup(scene);
        b
    }

    pub fn images(&self) -> Vec<Arc<RenderImage>> {
        let anims = self
            .kit
            .troops
            .iter()
            .flatten()
            .flat_map(|t| [&t.walk, &t.idle, &t.attack, &t.death, &t.decay])
            .chain(self.kit.builders.iter().flat_map(|b| [&b.walk, &b.task, &b.idle]))
            .chain(self.kit.castle.iter());
        anims
            .flat_map(|a| a.frames.iter().map(|f| f.img.clone()))
            .collect()
    }

    pub fn rubble(&self) -> Option<&Img> {
        self.kit.rubble.as_ref()
    }

    pub fn flames(&self) -> &[Vec<Img>] {
        &self.kit.flames
    }

    /// The icons and what has become of them, for the overlays.
    pub fn icons(&self) -> &[Icon] {
        &self.icons
    }

    /// Split the icons into the two sides and find where soldiers can stand around them. Icons
    /// that were already in the battle at the same place keep their damage.
    pub fn setup(&mut self, scene: &Scene) {
        let ds = self.kit.ds;
        let (w, h) = (scene.w as f32, scene.h as f32);
        let on_field = |x: f32, y: f32| {
            x > 14.0 && y > 40.0 && x < w - 14.0 && y < h - 10.0 && scene.is_free(x, y)
        };
        let rects: Vec<Rect> = scene
            .icons
            .iter()
            .copied()
            .filter(|r| r.x1 > 0.0 && r.y1 > 0.0 && r.x0 < w && r.y0 < h)
            .collect();
        let centres: Vec<(f32, f32)> = rects
            .iter()
            .map(|r| ((r.x0 + r.x1) * 0.5, r.y0 + (r.y1 - r.y0) * 0.4))
            .collect();
        // the icons all hold the field together (side 1); only the red army attacks them
        let team = vec![1usize; rects.len()];
        // free spots just outside each icon: beside it, below its label and above it
        let spots_of = |r: &Rect| -> Vec<(f32, f32)> {
            let (cx, hgt) = ((r.x0 + r.x1) * 0.5, r.y1 - r.y0);
            let cand = [
                (r.x1 + 24.0 * ds, r.y0 + hgt * 0.35),
                (r.x1 + 24.0 * ds, r.y0 + hgt * 0.8),
                (r.x0 - 24.0 * ds, r.y0 + hgt * 0.35),
                (r.x0 - 24.0 * ds, r.y0 + hgt * 0.8),
                (cx - 14.0 * ds, r.y1 + 44.0 * ds),
                (cx + 14.0 * ds, r.y1 + 44.0 * ds),
                (cx, r.y0 - 12.0 * ds),
            ];
            cand.into_iter().filter(|&(x, y)| on_field(x, y)).collect()
        };
        let old = std::mem::take(&mut self.icons);
        for (k, r) in rects.iter().enumerate() {
            let mut icon = Icon {
                rect: *r,
                side: team[k],
                hp: ICON_HP,
                phase: Phase::Standing,
                since_hit: 99.0,
                spots: spots_of(r),
            };
            if let Some(o) = old.iter().find(|o| o.rect == *r) {
                (icon.hp, icon.phase, icon.since_hit) = (o.hp, o.phase, o.since_hit);
            }
            self.icons.push(icon);
        }
        // the red army marches in from the screen edges away from the icons: the far side and the
        // bottom (icons usually line the left of the screen)
        let mean_x = if centres.is_empty() {
            w * 0.5
        } else {
            centres.iter().map(|c| c.0).sum::<f32>() / centres.len() as f32
        };
        let far = if mean_x < w * 0.5 { w - 18.0 * ds } else { 18.0 * ds };
        let side_edge = (1..12).map(|k| (far, h * k as f32 / 12.0));
        let bottom = (1..10).map(|k| {
            let u = k as f32 / 10.0;
            (if mean_x < w * 0.5 { w * (0.08 + 0.5 * u) } else { w * (0.42 + 0.5 * u) }, h - 22.0 * ds)
        });
        self.edges[0] = side_edge.chain(bottom).filter(|&(x, y)| on_field(x, y)).collect();
        self.edges[1].clear();
        self.routes_stale = true;
        // soldiers caught on ground that is no longer open (the icons moved) leave the field
        self.men
            .retain(|m| matches!(m.act, Act::Dying | Act::Dead) || scene.is_free(m.x, m.y));
    }

    fn rebuild_routes(&mut self, scene: &Scene) {
        let ds = self.kit.ds;
        let (w, h) = (scene.w as f32, scene.h as f32);
        let walkable = |x: f32, y: f32| {
            x > 6.0 && y > 30.0 && x < w - 6.0 && y < h - 4.0 && scene.is_free(x, y)
        };
        for s in 0..2 {
            let spots: Vec<(f32, f32)> = self
                .icons
                .iter()
                .filter(|i| i.side == s && i.phase == Phase::Standing)
                .flat_map(|i| i.spots.iter().copied())
                .collect();
            self.to_side[s] = Flow::build(scene, &walkable, &spots, 6.0 * ds);
        }
        self.routes_stale = false;
    }

    pub fn stats(&self) -> String {
        let alive = |s: usize| {
            self.men
                .iter()
                .filter(|m| m.side == s && Self::alive(m))
                .count()
        };
        let fallen = self
            .men
            .iter()
            .filter(|m| matches!(m.act, Act::Dying | Act::Dead))
            .count();
        let siege = self.men.iter().filter(|m| m.act == Act::Siege).count();
        let standing = |s: usize| {
            self.icons
                .iter()
                .filter(|i| i.side == s && i.phase == Phase::Standing)
                .count()
        };
        let total = |s: usize| self.icons.iter().filter(|i| i.side == s).count();
        let hurt = self
            .icons
            .iter()
            .filter(|i| i.phase == Phase::Standing && i.hp < ICON_HP)
            .count();
        format!(
            "red {} men | {}/{} icons up | {hurt} damaged | fallen men {fallen} | besieging {siege}",
            alive(0),
            standing(1),
            total(1)
        )
    }

    /// Once the last icon is down a crew of villagers comes to build a castle on open ground near
    /// the middle of the screen, and the army takes up guard posts around it. Any icon back up
    /// calls it off.
    fn victory(&mut self, scene: &Scene) {
        let won = !self.icons.is_empty() && self.icons.iter().all(|i| i.phase != Phase::Standing);
        if !won {
            if self.fort.take().is_some() {
                self.workers.clear();
                for m in &mut self.men {
                    m.slot = None;
                    if m.act == Act::Guard {
                        m.act = Act::March;
                    }
                }
            }
            return;
        }
        if self.fort.is_some() {
            return;
        }
        let Some(castle) = &self.kit.castle else { return };
        let ds = self.kit.ds;
        let (w, h) = (scene.w as f32, scene.h as f32);
        let full = &castle.frames[castle.frames.len() - 1];
        let (rx, ry) = (full.w * 0.42, full.w * 0.21);
        let ellipse = |cx: f32, cy: f32, k: f32, n: usize| -> Vec<(f32, f32)> {
            (0..n)
                .map(|i| {
                    let a = i as f32 / n as f32 * std::f32::consts::TAU;
                    (cx + a.cos() * rx * k, cy + a.sin() * ry * k)
                })
                .collect()
        };
        // the site: its footprint on open ground, clear of the icons, room for the whole castle
        // above it, and as near the middle as that allows
        let mut best: Option<(f32, (f32, f32))> = None;
        for gy in 0..9 {
            for gx in 0..11 {
                let (cx, cy) = (w * (0.15 + 0.7 * gx as f32 / 10.0), h * (0.35 + 0.5 * gy as f32 / 8.0));
                if cy + full.oy < 0.0 || cx - rx * 1.7 < 0.0 || cx + rx * 1.7 > w || cy + ry * 1.7 > h {
                    continue;
                }
                let foot: Vec<(f32, f32)> = (0..5).flat_map(|r| ellipse(cx, cy, r as f32 / 4.0, 12)).collect();
                let open = foot.iter().filter(|&&(x, y)| scene.is_free(x, y)).count() as f32 / foot.len() as f32;
                let near_icon = self.icons.iter().any(|i| {
                    let r = i.rect;
                    cx + rx * 1.5 > r.x0 && cx - rx * 1.5 < r.x1 && cy + ry * 1.5 > r.y0 && cy + full.oy < r.y1
                });
                let off = ((cx - w * 0.5) / w).abs() + ((cy - h * 0.55) / h).abs();
                let score = open * 4.0 - off - if near_icon { 3.0 } else { 0.0 };
                if open > 0.75 && best.is_none_or(|b| score > b.0) {
                    best = Some((score, (cx, cy)));
                }
            }
        }
        let Some((_, (cx, cy))) = best else { return };
        let walkable = |x: f32, y: f32| x > 6.0 && y > 30.0 && x < w - 6.0 && y < h - 4.0 && scene.is_free(x, y);
        // guards in a ring around it, builders along its front
        let alive = self.men.iter().filter(|m| Self::alive(m)).count().max(8);
        let posts: Vec<(f32, f32)> = ellipse(cx, cy, 1.75, alive.min(24)).into_iter().filter(|&(x, y)| walkable(x, y)).collect();
        let spots: Vec<(f32, f32)> = (0..CREW)
            .map(|i| {
                let a = 0.15 + (std::f32::consts::PI - 0.3) * i as f32 / (CREW - 1) as f32;
                (cx + a.cos() * rx * 1.12, cy + a.sin() * ry * 1.12)
            })
            .collect();
        let mut targets = posts.clone();
        targets.extend(&spots);
        let route = Flow::build(scene, &walkable, &targets, 6.0 * ds);
        // each soldier takes the nearest free post
        let mut free: Vec<usize> = (0..posts.len()).collect();
        for m in self.men.iter_mut().filter(|m| Self::alive(m)) {
            let Some(pos) = (0..free.len()).min_by(|&a, &b| {
                let (pa, pb) = (posts[free[a]], posts[free[b]]);
                ((pa.0 - m.x).powi(2) + (pa.1 - m.y).powi(2)).total_cmp(&((pb.0 - m.x).powi(2) + (pb.1 - m.y).powi(2)))
            }) else {
                break;
            };
            m.slot = Some(free.swap_remove(pos));
        }
        // the crew comes in from the stretch of edge nearest the site
        let edge = &self.edges[0];
        let door = edge
            .iter()
            .copied()
            .min_by(|a, b| ((a.0 - cx).powi(2) + (a.1 - cy).powi(2)).total_cmp(&((b.0 - cx).powi(2) + (b.1 - cy).powi(2))))
            .unwrap_or((w - 20.0 * ds, cy));
        if !self.kit.builders.is_empty() {
            for k in 0..CREW {
                self.workers.push(Worker {
                    kind: k % self.kit.builders.len(),
                    x: door.0 + self.rng.range(-10.0, 10.0) * ds,
                    y: door.1 + self.rng.range(-10.0, 10.0) * ds,
                    dir: 0,
                    t: self.rng.f() * 2.0,
                    spot: k,
                    working: false,
                    moving: false,
                    lean: if k % 2 == 0 { 1.0 } else { -1.0 },
                });
            }
        }
        crate::log(&format!("icon wars: every icon is down, building a castle at {cx:.0},{cy:.0}"));
        self.fort = Some(Fort { at: (cx, cy), rx, ry, progress: 0.0, posts, spots, route });
    }

    /// The crew walks to the castle and builds it.
    fn step_workers(&mut self, scene: &Scene, dt: f32) {
        let Some(fort) = &mut self.fort else { return };
        let ds = self.kit.ds;
        let (w, h) = (scene.w as f32, scene.h as f32);
        let ok = |x: f32, y: f32| x > 6.0 && y > 30.0 && x < w - 6.0 && y < h - 4.0 && scene.is_free(x, y);
        let mut busy = 0;
        for v in &mut self.workers {
            v.t = (v.t + dt) % 3600.0;
            let (sx, sy) = fort.spots[v.spot % fort.spots.len()];
            let d = ((sx - v.x).powi(2) + (sy - v.y).powi(2)).sqrt();
            if d < 3.0 * ds {
                v.working = fort.progress < 1.0;
                v.moving = false;
                v.dir = dir_of(fort.at.0 - v.x, fort.at.1 - v.y);
                busy += usize::from(v.working);
                continue;
            }
            let far = ((v.x - fort.at.0) / (fort.rx * 2.4)).powi(2) + ((v.y - fort.at.1) / (fort.ry * 2.4)).powi(2) > 1.0;
            let (tx, ty) = match (far, fort.route.way(v.x, v.y)) {
                (true, Way::Next(nx, ny)) => (nx, ny),
                _ => (sx, sy),
            };
            let step = (24.0 * ds * dt).min(d);
            let base = (ty - v.y).atan2(tx - v.x);
            v.moving = false;
            for off in [0.0f32, 0.45, -0.45, 0.9, -0.9, 1.35, -1.35, 1.8, -1.8] {
                let a = base + off * v.lean;
                let (c, s) = (a.cos(), a.sin());
                // the last stretch to its spot may cut across the castle's own ground
                if !far || (ok(v.x + c * 12.0 * ds, v.y + s * 12.0 * ds) && ok(v.x + c * step, v.y + s * step)) {
                    v.x += c * step;
                    v.y += s * step;
                    v.dir = dir_of(c, s);
                    v.moving = true;
                    break;
                }
            }
            if !v.moving {
                v.lean = -v.lean;
            }
        }
        if fort.progress < 1.0 {
            fort.progress = (fort.progress + dt / BUILD * busy as f32 / CREW as f32).min(1.0);
            if fort.progress >= 1.0 {
                crate::log("icon wars: the castle is built");
            }
        }
    }

    fn alive(m: &Soldier) -> bool {
        matches!(m.act, Act::March | Act::Fight | Act::Siege | Act::Guard)
    }

    fn spawn_wave(&mut self, side: usize) {
        let alive = self
            .men
            .iter()
            .filter(|m| m.side == side && Self::alive(m))
            .count();
        // the icons don't send anyone out: the attackers come in from the screen edge
        let edge = &self.edges[side];
        if side != 0 || edge.is_empty() || alive >= CAP {
            return;
        }
        let n = (3 + self.rng.below(3)).min(CAP - alive);
        let k0 = self.rng.below(edge.len());
        let spots: Vec<(f32, f32)> = (0..3).map(|j| edge[(k0 + j).min(edge.len() - 1)]).collect();
        let troops = &self.kit.troops[side];
        let total: f32 = troops.iter().map(|t| t.weight).sum();
        for k in 0..n {
            let mut pick = self.rng.f() * total;
            let tr = troops
                .iter()
                .position(|t| {
                    pick -= t.weight;
                    pick <= 0.0
                })
                .unwrap_or(0);
            let (sx, sy) = spots[k % spots.len()];
            let (x, y) = (
                sx + self.rng.range(-6.0, 6.0) * self.kit.ds,
                sy + self.rng.range(-4.0, 4.0) * self.kit.ds,
            );
            let lean = if self.rng.f() < 0.5 { -1.0 } else { 1.0 };
            self.men.push(Soldier {
                side,
                tr,
                x,
                y,
                dir: self.rng.below(16),
                t: self.rng.f() * 3.0,
                hp: troops[tr].hp,
                act: Act::March,
                moving: false,
                swing: 0.0,
                lean,
                slot: None,
            });
        }
    }

    pub fn step(&mut self, scene: &Scene, dt: f32) {
        let ds = self.kit.ds;
        // icons fall and lie in ruins
        for i in &mut self.icons {
            i.since_hit = (i.since_hit + dt).min(99.0);
            i.phase = match i.phase {
                Phase::Standing => Phase::Standing,
                Phase::Falling(t) if t + dt >= FALL => Phase::Ruined(0.0),
                Phase::Falling(t) => Phase::Falling(t + dt),
                Phase::Ruined(t) => Phase::Ruined(t + dt),
            };
        }
        if self.routes_stale {
            self.rebuild_routes(scene);
        }
        self.wave_in[0] -= dt;
        if self.wave_in[0] <= 0.0 {
            self.spawn_wave(0);
            self.wave_in[0] = self.rng.range(8.0, 14.0);
        }
        self.victory(scene);
        let (w, h) = (scene.w as f32, scene.h as f32);
        let ok = |x: f32, y: f32| {
            x > 6.0 && y > 30.0 && x < w - 6.0 && y < h - 4.0 && scene.is_free(x, y)
        };
        // positions this tick, so everyone decides on the same picture of the field
        let snap: Vec<(usize, f32, f32, bool)> = self
            .men
            .iter()
            .map(|m| (m.side, m.x, m.y, Self::alive(m)))
            .collect();
        let mut hits: Vec<(usize, f32)> = Vec::new();
        let mut icon_hits: Vec<(usize, f32)> = Vec::new();
        for i in 0..self.men.len() {
            let m = &mut self.men[i];
            m.t = (m.t + dt) % 3600.0;
            let tr = &self.kit.troops[m.side][m.tr];
            match m.act {
                Act::Dying => {
                    if m.t * tr.death.fps >= tr.death.per_dir as f32 {
                        m.act = Act::Dead;
                        m.t = 0.0;
                    }
                    continue;
                }
                Act::Dead => continue,
                _ => {}
            }
            // nearest enemy soldier still standing
            let foe = snap
                .iter()
                .enumerate()
                .filter(|(_, e)| e.3 && e.0 != m.side)
                .map(|(j, e)| {
                    (
                        j,
                        ((e.1 - m.x).powi(2) + ((e.2 - m.y) * 1.4).powi(2)).sqrt(),
                    )
                })
                .min_by(|a, b| a.1.total_cmp(&b.1));
            let swing = |m: &mut Soldier, rng: &mut Rng| -> Option<f32> {
                m.swing += dt;
                if m.swing >= tr.cycle {
                    m.swing -= tr.cycle;
                    return Some(rng.range(tr.dmg.0, tr.dmg.1));
                }
                None
            };
            let (tx, ty) = match foe {
                Some((j, d)) if d <= tr.reach => {
                    if m.act != Act::Fight {
                        m.act = Act::Fight;
                        m.t = 0.0;
                        m.swing = tr.cycle * 0.45;
                    }
                    m.dir = dir_of(snap[j].1 - m.x, snap[j].2 - m.y);
                    m.moving = false;
                    if let Some(dmg) = swing(m, &mut self.rng) {
                        hits.push((j, dmg));
                    }
                    continue;
                }
                Some((j, d)) if d <= SIGHT * ds => (snap[j].1, snap[j].2),
                _ => {
                    // no one in the way: march on the nearest standing enemy icon and tear it down
                    let target = self
                        .icons
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.side != m.side && c.phase == Phase::Standing)
                        .map(|(k, c)| (k, c.centre()))
                        .min_by(|a, b| {
                            ((a.1.0 - m.x).powi(2) + (a.1.1 - m.y).powi(2))
                                .total_cmp(&((b.1.0 - m.x).powi(2) + (b.1.1 - m.y).powi(2)))
                        });
                    let post = self.fort.as_ref().and_then(|f| Some((f, *f.posts.get(m.slot?)?)));
                    match target {
                        // nothing left standing: stand guard at the castle, facing out
                        None if post.is_some() => {
                            let Some((fort, (px, py))) = post else { continue };
                            let d2 = (px - m.x).powi(2) + (py - m.y).powi(2);
                            let far = ((m.x - fort.at.0) / (fort.rx * 2.6)).powi(2) + ((m.y - fort.at.1) / (fort.ry * 2.6)).powi(2) > 1.0;
                            if far {
                                // around the trees on the way, then straight to its own post
                                match fort.route.way(m.x, m.y) {
                                    Way::Next(nx, ny) => (nx, ny),
                                    _ => (px, py),
                                }
                            } else if d2 > (3.0 * ds).powi(2) {
                                (px, py)
                            } else {
                                if m.act != Act::Guard {
                                    m.act = Act::Guard;
                                    m.t = self.rng.f() * 2.0;
                                }
                                m.dir = dir_of(px - fort.at.0, py - fort.at.1);
                                m.moving = false;
                                continue;
                            }
                        }
                        None => match foe {
                            Some((j, _)) => (snap[j].1, snap[j].2),
                            None => {
                                m.act = Act::March;
                                m.moving = false;
                                continue;
                            }
                        },
                        Some((k, (ix, iy))) => {
                            // close enough to strike (archers shoot from range)
                            let near = self.icons[k].spots.iter().any(|s| {
                                (s.0 - m.x).powi(2) + (s.1 - m.y).powi(2) < (8.0 * ds).powi(2)
                            });
                            let in_range = tr.reach > 60.0 * ds
                                && ((ix - m.x).powi(2) + (iy - m.y).powi(2)).sqrt() < tr.reach;
                            // anyone up against the icon swings at it, wherever they ended up
                            let r = self.icons[k].rect;
                            let gx = (r.x0 - m.x).max(m.x - r.x1).max(0.0);
                            let gy = (r.y0 - m.y).max(m.y - r.y1).max(0.0);
                            let touching = (gx * gx + gy * gy).sqrt() < 34.0 * ds;
                            let arrived = near
                                || in_range
                                || touching
                                || matches!(self.to_side[1 - m.side].way(m.x, m.y), Way::Arrived);
                            if arrived {
                                // strike whichever standing enemy icon is nearest from here
                                let k = self
                                    .icons
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, c)| c.side != m.side && c.phase == Phase::Standing)
                                    .min_by(|a, b| {
                                        let (pa, pb) = (a.1.centre(), b.1.centre());
                                        ((pa.0 - m.x).powi(2) + (pa.1 - m.y).powi(2)).total_cmp(
                                            &((pb.0 - m.x).powi(2) + (pb.1 - m.y).powi(2)),
                                        )
                                    })
                                    .map_or(k, |(k, _)| k);
                                let (ix, iy) = self.icons[k].centre();
                                if m.act != Act::Siege {
                                    m.act = Act::Siege;
                                    m.t = 0.0;
                                    m.swing = tr.cycle * 0.45;
                                }
                                m.dir = dir_of(ix - m.x, iy - m.y);
                                m.moving = false;
                                if let Some(dmg) = swing(m, &mut self.rng) {
                                    icon_hits.push((k, dmg));
                                }
                                continue;
                            }
                            match self.to_side[1 - m.side].way(m.x, m.y) {
                                Way::Next(px, py) => (px, py),
                                _ => (ix, iy),
                            }
                        }
                    }
                }
            };
            m.act = Act::March;
            let dist = ((tx - m.x).powi(2) + (ty - m.y).powi(2)).sqrt();
            // walk towards the target, sidestepping trees, icons and the screen edge
            let step = (tr.speed * dt).min(dist);
            let probe = 12.0 * ds;
            let base = (ty - m.y).atan2(tx - m.x);
            m.moving = false;
            for off in [
                0.0f32, 0.45, -0.45, 0.9, -0.9, 1.35, -1.35, 1.8, -1.8, 2.4, -2.4,
            ] {
                let a = base + off * m.lean;
                let (c, s) = (a.cos(), a.sin());
                if ok(m.x + c * probe, m.y + s * probe) && ok(m.x + c * step, m.y + s * step) {
                    m.x += c * step;
                    m.y += s * step;
                    m.dir = dir_of(c, s);
                    m.moving = true;
                    break;
                }
            }
            if !m.moving {
                m.lean = -m.lean;
            }
        }
        for (j, dmg) in hits {
            let m = &mut self.men[j];
            if !Self::alive(m) {
                continue;
            }
            m.hp -= dmg;
            if m.hp <= 0.0 {
                m.act = Act::Dying;
                m.t = 0.0;
                m.moving = false;
            }
        }
        for (k, dmg) in icon_hits {
            let icon = &mut self.icons[k];
            if icon.phase != Phase::Standing {
                continue;
            }
            icon.hp -= dmg;
            icon.since_hit = 0.0;
            if icon.hp <= 0.0 {
                icon.hp = 0.0;
                icon.phase = Phase::Falling(0.0);
                self.routes_stale = true;
            }
        }
        // soldiers on the march spread a little instead of walking in single file; once they are
        // hacking at an icon they stand where they are and may crowd into each other
        let gap = 15.0 * ds;
        for i in 0..self.men.len() {
            for j in i + 1..self.men.len() {
                let (a, b) = (&self.men[i], &self.men[j]);
                if a.act != Act::March || b.act != Act::March || !a.moving || !b.moving {
                    continue;
                }
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let d = (dx * dx + dy * dy).sqrt();
                if d >= gap || d < 1e-3 {
                    continue;
                }
                let push = (gap - d) * 0.25;
                let (ux, uy) = (dx / d * push, dy / d * push);
                let (ax, ay, bx, by) = (a.x - ux, a.y - uy, b.x + ux, b.y + uy);
                if ok(ax, ay) {
                    (self.men[i].x, self.men[i].y) = (ax, ay);
                }
                if ok(bx, by) {
                    (self.men[j].x, self.men[j].y) = (bx, by);
                }
            }
        }
        self.men.retain(|m| m.act != Act::Dead || m.t < ROT);
        self.step_workers(scene, dt);
    }

    /// Frames to paint: (layer, x, y, frame); layer 0 lies on the ground under everyone else.
    pub fn visible(&self) -> Vec<(u8, f32, f32, &Frame)> {
        let mut out = Vec::with_capacity(self.men.len());
        for m in &self.men {
            let tr = &self.kit.troops[m.side][m.tr];
            let (layer, f) = match m.act {
                Act::Fight | Act::Siege => (1, tr.attack.frame(m.dir, m.t)),
                Act::Guard => (1, tr.idle.frame(m.dir, m.t)),
                Act::March if m.moving => (1, tr.walk.frame(m.dir, m.t)),
                Act::March => (1, tr.walk.frame(m.dir, 0.0)),
                Act::Dying => (0, tr.death.once(m.dir, m.t)),
                Act::Dead => (0, tr.decay.once(m.dir, m.t)),
            };
            out.push((layer, m.x, m.y, f));
        }
        if let (Some(fort), Some(castle)) = (&self.fort, &self.kit.castle) {
            // nothing shows until the crew has started on it
            if fort.progress > 0.0 {
                let stage = ((fort.progress * STAGES as f32).ceil() as usize).clamp(1, STAGES) - 1;
                out.push((1, fort.at.0, fort.at.1, castle.frame(stage, 0.0)));
            }
        }
        for v in &self.workers {
            let b = &self.kit.builders[v.kind];
            let f = if v.moving {
                b.walk.frame(v.dir, v.t)
            } else if v.working {
                b.task.frame(v.dir, v.t)
            } else {
                b.idle.frame(v.dir, v.t)
            };
            out.push((1, v.x, v.y, f));
        }
        out
    }
}
