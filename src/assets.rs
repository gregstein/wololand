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

/// AoE2 DE's Steam app id.
const STEAM_APP: &str = "813780";

fn reg_string(root: &windows_registry::Key, path: &str, name: &str) -> Option<String> {
    root.open(path).and_then(|k| k.get_string(name)).ok().filter(|s| !s.is_empty())
}

/// Steam install folders from the registry (per-user and machine-wide keys).
fn steam_roots() -> Vec<PathBuf> {
    use windows_registry::{CURRENT_USER, LOCAL_MACHINE};
    [
        reg_string(CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
        reg_string(LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath"),
        reg_string(LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
    ]
    .into_iter()
    .flatten()
    .map(|s| PathBuf::from(s.replace('/', "\\")))
    .collect()
}

/// Every Steam library folder: the Steam folder itself plus those listed in `libraryfolders.vdf`.
fn steam_libraries() -> Vec<PathBuf> {
    let mut libs = Vec::new();
    for steam in steam_roots() {
        libs.push(steam.clone());
        for vdf in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
            let Ok(text) = std::fs::read_to_string(steam.join(vdf)) else { continue };
            for line in text.lines() {
                let parts: Vec<&str> = line.split('"').collect();
                if parts.len() >= 4 && parts[1] == "path" {
                    libs.push(PathBuf::from(parts[3].replace("\\\\", "\\")));
                }
            }
        }
    }
    libs
}

/// Candidates for the Steam version: each library's `appmanifest_813780.acf` names the install folder.
fn steam_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for lib in steam_libraries() {
        let apps = lib.join("steamapps");
        if let Ok(acf) = std::fs::read_to_string(apps.join(format!("appmanifest_{STEAM_APP}.acf"))) {
            for line in acf.lines() {
                let parts: Vec<&str> = line.split('"').collect();
                if parts.len() >= 4 && parts[1] == "installdir" {
                    out.push(apps.join("common").join(parts[3]));
                }
            }
        }
        out.push(apps.join("common/AoE2DE"));
    }
    use windows_registry::LOCAL_MACHINE;
    for base in [r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"] {
        if let Some(p) = reg_string(LOCAL_MACHINE, &format!(r"{base}\Steam App {STEAM_APP}"), "InstallLocation") {
            out.push(PathBuf::from(p));
        }
    }
    out
}

/// Fixed drives that exist, `C:\` to `Z:\`.
fn drives() -> Vec<PathBuf> {
    (b'C'..=b'Z').map(|c| PathBuf::from(format!("{}:\\", c as char))).filter(|d| d.is_dir()).collect()
}

/// Folders the Xbox app installs games into. Each drive may hold a `.GamingRoot` file
/// ("RGBX", a count, then NUL-terminated UTF-16 paths relative to the drive).
fn xbox_game_folders() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in drives() {
        if let Ok(bytes) = std::fs::read(d.join(".GamingRoot")) {
            if bytes.len() > 8 && &bytes[..4] == b"RGBX" {
                let words: Vec<u16> = bytes[8..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                for s in words.split(|&w| w == 0).filter(|s| !s.is_empty()) {
                    let rel = String::from_utf16_lossy(s);
                    out.push(d.join(rel.trim_start_matches(['\\', '/'])));
                }
            }
        }
        out.push(d.join("XboxGames"));
        out.push(d.join(r"Program Files\ModifiableWindowsApps"));
    }
    out
}

/// Candidates for the Microsoft Store / Xbox app version.
fn store_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for folder in xbox_game_folders() {
        let Ok(rd) = std::fs::read_dir(&folder) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.starts_with("age of empires ii") && !name.starts_with("age of empires iii") {
                out.push(e.path().join("Content"));
                out.push(e.path());
            }
        }
    }
    // Store package registration: AoE2 DE is the "Microsoft.MSPhoenix" package.
    let packages = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";
    if let Ok(k) = windows_registry::CURRENT_USER.open(packages) {
        if let Ok(keys) = k.keys() {
            for name in keys.filter(|n| n.starts_with("Microsoft.MSPhoenix")) {
                if let Some(p) = reg_string(windows_registry::CURRENT_USER, &format!(r"{packages}\{name}"), "PackageRootFolder") {
                    let p = PathBuf::from(p);
                    out.push(p.join("Content"));
                    out.push(p);
                }
            }
        }
    }
    out
}

/// Accepts the game folder itself, its parent `...\Age of Empires II- Definitive Edition` (Xbox),
/// or a folder inside it such as `resources`.
pub fn normalize_game_dir(p: &Path) -> Option<PathBuf> {
    let mut tries = vec![p.to_path_buf(), p.join("Content"), p.join("AoE2DE")];
    let mut up = p.parent();
    while let Some(u) = up {
        tries.push(u.to_path_buf());
        up = u.parent();
    }
    tries.into_iter().find(|t| has_graphics(t))
}

impl Game {
    /// The game at exactly this folder (after [`normalize_game_dir`]).
    pub fn at(dir: &Path) -> Option<Game> {
        normalize_game_dir(dir).map(|root| Game { root, uhd: false })
    }

    /// The custom folder if it holds the game, otherwise Steam, then the Microsoft Store / Xbox app.
    pub fn locate(custom: Option<&str>) -> Option<Game> {
        if let Some(g) = custom.filter(|c| !c.is_empty()).and_then(|c| Game::at(Path::new(c))) {
            return Some(g);
        }
        Game::detect()
    }

    /// Searches Steam and the Microsoft Store / Xbox app locations, ignoring any custom folder.
    pub fn detect() -> Option<Game> {
        let mut candidates = steam_candidates();
        candidates.extend(store_candidates());
        candidates.push(PathBuf::from(r"C:\Program Files (x86)\Steam\steamapps\common\AoE2DE"));
        candidates.push(PathBuf::from(r"C:\Program Files\Steam\steamapps\common\AoE2DE"));
        let found = candidates.into_iter().find(|p| has_graphics(p));
        crate::log(&format!("game search: {}", found.as_ref().map_or("not found".to_string(), |p| p.display().to_string())));
        found.map(|root| Game { root, uhd: false })
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
