//! Wololand: Age of Empires II map themes that grow around your Windows desktop icons.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actors;
mod aoe;
mod app;
mod assets;
mod battle;
mod desktop;
mod iconfx;
mod noise;
mod overlay;
mod scene;
mod settings;
mod tray;

use anyhow::Result;
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::Threading::CreateMutexW;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    if !cmd.is_empty() && !cmd.starts_with("--") {
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
    match cmd {
        "dump" => return dump(&args[2], &args[3]),
        "montage" => return montage(&args[2], &args[3], args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1)),
        "texsheet" => return texsheet(&args[2], &args[3]),
        "render" => return render_cli(&args[2..]),
        "simulate" => return simulate(&args[2], args.get(3).and_then(|s| s.parse().ok()).unwrap_or(300.0)),
        "overlaytest" => return overlay::self_test(),
        "fxtest" => return iconfx::self_test(args.get(2).map_or("fx.png", |s| s.as_str())),
        "pcsheet" => return pcsheet(&args[2], &args[3], args.get(4).and_then(|s| s.parse().ok()).unwrap_or(30)),
        "--theme" => {
            let want = args.get(2).map(|s| s.to_lowercase().replace([' ', '-', '_'], "")).unwrap_or_default();
            if let Some(i) = scene::Theme::ALL.iter().position(|t| t.name().to_lowercase().replace(' ', "") == want) {
                tray::signal_running_with(tray::WM_SET_THEME, i);
            }
            return Ok(());
        }
        "--exit" => {
            tray::signal_running(tray::WM_EXIT);
            return Ok(());
        }
        _ => {}
    }
    unsafe {
        let _mutex = CreateMutexW(None, true, w!("Local\\Wololand.SingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            tray::signal_running(tray::WM_OPEN_SETTINGS);
            return Ok(());
        }
        let settings = settings::Settings::load();
        desktop::lower_process_priority();
        app::run(settings, cmd != "--autostart");
    }
    Ok(())
}

/// Append a line to `%APPDATA%\Wololand\wololand.log` (kept small).
pub fn log(msg: &str) {
    use std::io::Write;
    let path = settings::data_dir().join("wololand.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::create_dir_all(settings::data_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "[{t}] {msg}");
    }
}

/// Developer helper: `render <theme> <out.png> [w h]` builds a scene offline around sample icons.
fn render_cli(a: &[String]) -> Result<()> {
    let want = a.first().map(|s| s.to_lowercase().replace([' ', '-', '_'], "")).unwrap_or_default();
    let theme = scene::Theme::ALL.into_iter().find(|t| t.name().to_lowercase().replace(' ', "") == want).unwrap_or_default();
    let out = a.get(1).cloned().unwrap_or_else(|| "scene.png".into());
    let w: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(1920);
    let h: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(1080);
    let n: usize = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(14);
    let game = assets::Game::locate(None).ok_or_else(|| anyhow::anyhow!("AoE2 DE not found"))?;
    let t0 = std::time::Instant::now();
    let kit = scene::ThemeKit::load(&game, theme, dev_scale())?;
    let t1 = std::time::Instant::now();
    let icons = test_icons(w, h, n);
    let sc = scene::build(&kit, w, h, &icons, 7);
    let t2 = std::time::Instant::now();
    println!("load {:?}  build {:?}", t1 - t0, t2 - t1);
    let mut img = image::RgbaImage::new(w as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            let g = sc.ground[y * w + x];
            let under = sc.water.as_ref().map_or([30, 40, 25, 255], |(ww, wh, a, _)| a[(y % wh) * ww + x % ww]);
            let al = g[3] as f32 / 255.0;
            let mut p = [0u8; 4];
            for k in 0..3 {
                p[k] = (g[k] as f32 * al + under[k] as f32 * (1.0 - al)) as u8;
            }
            p[3] = 255;
            img.put_pixel(x as u32, y as u32, image::Rgba(p));
        }
    }
    // working villagers at their posts (first frame of the work animation, facing the job)
    let workers = [
        (scene::PostKind::Miner, "u_vil_male_miner_gold_taskA"),
        (scene::PostKind::Builder, "u_vil_male_builder_taskA"),
        (scene::PostKind::Lumber, "u_vil_male_lumberjack_taskA"),
    ];
    for (kind, name) in workers {
        let posts: Vec<_> = sc.posts.iter().filter(|p| p.kind == kind).collect();
        if posts.is_empty() {
            continue;
        }
        let frames = game.sprite_pc(name, 1.0, Some(assets::BLUE))?;
        let per = frames.len() / 16;
        println!("{kind:?}: {} posts", posts.len());
        for p in posts {
            let a = (p.fy - p.y).atan2(p.fx - p.x).rem_euclid(std::f32::consts::TAU);
            let dir = ((a / (std::f32::consts::TAU / 16.0)).round() as usize) % 16;
            let f = &frames[dir * per];
            for y in 0..f.h {
                for x in 0..f.w {
                    let c = f.px[y * f.w + x];
                    let (px, py) = (p.x as i32 + f.ox + x as i32, p.y as i32 + f.oy + y as i32);
                    if c[3] > 100 && px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                        img.put_pixel(px as u32, py as u32, image::Rgba([c[0], c[1], c[2], 255]));
                    }
                }
            }
        }
    }
    for r in &icons {
        for y in (r.y0 as u32 + 6)..(r.y0 as u32 + 54).min(h as u32) {
            for x in (r.x0 as u32 + 14)..(r.x0 as u32 + 62).min(w as u32) {
                img.put_pixel(x, y, image::Rgba([220, 220, 230, 255]));
            }
        }
    }
    img.save(out)?;
    Ok(())
}

/// Developer helper: run the units of a theme for `secs` simulated seconds around sample icons.
fn simulate(theme: &str, secs: f32) -> Result<()> {
    let want = theme.to_lowercase().replace([' ', '-', '_'], "");
    let theme = scene::Theme::ALL.into_iter().find(|t| t.name().to_lowercase().replace(' ', "") == want).unwrap_or_default();
    let game = assets::Game::locate(None).ok_or_else(|| anyhow::anyhow!("AoE2 DE not found"))?;
    let kit = scene::ThemeKit::load(&game, theme, 1.0)?;
    let (w, h) = (1920, 1080);
    let sc = scene::build(&kit, w, h, &test_icons(w, h, 14), 7);
    let mut world = actors::World::new(actors::Kit::load(&game, theme, 1.0)?, &sc, 7);
    let open = sc.cells.iter().filter(|&&c| c == scene::OPEN).count();
    let open_far = (0..sc.cells.len()).filter(|&i| sc.cells[i] == scene::OPEN && sc.dist[i] > 12.0).count();
    println!("{}: {} posts, {open} open cells ({open_far} outside the icons)", theme.name(), sc.posts.len());
    let dt = 1.0 / 15.0;
    let t0 = std::time::Instant::now();
    let mut t = 0.0;
    let mut next = 0.0;
    while t < secs {
        world.step(&sc, dt);
        let _ = world.visible();
        t += dt;
        if t >= next {
            println!("t={t:6.1}s  {}", world.stats());
            next += 10.0;
        }
    }
    println!("{} steps in {:?}", (secs / dt) as usize, t0.elapsed());
    // optional snapshot of the scene with every visible unit: WOLOLAND_SNAPSHOT=out.png
    if let Ok(out) = std::env::var("WOLOLAND_SNAPSHOT") {
        let mut img = image::RgbaImage::new(w as u32, h as u32);
        for y in 0..h {
            for x in 0..w {
                let g = sc.ground[y * w + x];
                let under = sc.water.as_ref().map_or([30, 40, 25, 255], |(ww, wh, a, _)| a[(y % wh) * ww + x % ww]);
                let al = g[3] as f32 / 255.0;
                let mut p = [0u8; 4];
                for k in 0..3 {
                    p[k] = (g[k] as f32 * al + under[k] as f32 * (1.0 - al)) as u8;
                }
                p[3] = 255;
                img.put_pixel(x as u32, y as u32, image::Rgba(p));
            }
        }
        for (ux, uy, f) in world.visible() {
            let Some(bytes) = f.img.as_bytes(0) else { continue };
            let (fw, fh) = (f.w as i32, f.h as i32);
            for y in 0..fh {
                for x in 0..fw {
                    let i = ((y * fw + x) * 4) as usize;
                    let (b, g, r, a) = (bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]);
                    let (px, py) = (ux as i32 + f.ox as i32 + x, uy as i32 + f.oy as i32 + y);
                    if a > 0 && px >= 0 && py >= 0 && px < w as i32 && py < h as i32 {
                        let d = img.get_pixel_mut(px as u32, py as u32);
                        let t = a as f32 / 255.0;
                        for (k, c) in [r, g, b].into_iter().enumerate() {
                            d.0[k] = (c as f32 * t + d.0[k] as f32 * (1.0 - t)) as u8;
                        }
                    }
                }
            }
        }
        img.save(out)?;
    }
    Ok(())
}

/// Developer helper: every `step`-th frame of a sprite in blue and red player colour, side by side.
fn pcsheet(name: &str, out: &str, step: usize) -> Result<()> {
    let game = assets::Game::locate(None).ok_or_else(|| anyhow::anyhow!("AoE2 DE not found"))?;
    let rows = [game.sprite(name, 1.0)?, game.sprite_pc(name, 1.0, Some(assets::BLUE))?, game.sprite_pc(name, 1.0, Some(assets::RED))?];
    let picked: Vec<usize> = (0..rows[0].len()).step_by(step.max(1)).take(16).collect();
    let cw = rows[0].iter().map(|f| f.w).max().unwrap_or(1) as u32 + 4;
    let ch = rows[0].iter().map(|f| f.h).max().unwrap_or(1) as u32 + 4;
    let mut img = image::RgbaImage::from_pixel(cw * picked.len() as u32, ch * 3, image::Rgba([60, 60, 70, 255]));
    for (r, frames) in rows.iter().enumerate() {
        for (c, &i) in picked.iter().enumerate() {
            let f = &frames[i];
            for y in 0..f.h {
                for x in 0..f.w {
                    let p = f.px[y * f.w + x];
                    if p[3] > 128 {
                        img.put_pixel(c as u32 * cw + 2 + x as u32, r as u32 * ch + 2 + y as u32, image::Rgba(p));
                    }
                }
            }
        }
    }
    let img = image::imageops::resize(&img, img.width() * 2, img.height() * 2, image::imageops::FilterType::Nearest);
    img.save(out)?;
    Ok(())
}

/// Developer helper: every `step`-th frame of an SLD on one sheet, anchored at the hotspot.
fn montage(src: &str, out: &str, step: usize) -> Result<()> {
    let frames = aoe::sld::parse(&std::fs::read(src)?)?;
    let picked: Vec<_> = frames.iter().step_by(step.max(1)).collect();
    let (mut minx, mut miny, mut maxx, mut maxy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for f in &picked {
        if let Some(m) = &f.main {
            minx = minx.min(m.x - f.hotspot_x);
            miny = miny.min(m.y - f.hotspot_y);
            maxx = maxx.max(m.x + m.w as i32 - f.hotspot_x);
            maxy = maxy.max(m.y + m.h as i32 - f.hotspot_y);
        }
    }
    let (cw, ch) = ((maxx - minx + 4) as u32, (maxy - miny + 4) as u32);
    let cols = (picked.len() as f32).sqrt().ceil() as u32;
    let rows = (picked.len() as u32).div_ceil(cols);
    let mut img = image::RgbaImage::from_pixel(cols * cw, rows * ch, image::Rgba([60, 60, 70, 255]));
    for (i, f) in picked.iter().enumerate() {
        let (ox, oy) = ((i as u32 % cols) * cw, (i as u32 / cols) * ch);
        if let Some(m) = &f.main {
            for y in 0..m.h {
                for x in 0..m.w {
                    let p = m.px[y * m.w + x];
                    if p[3] > 0 {
                        let px = ox as i32 + m.x - f.hotspot_x - minx + 2 + x as i32;
                        let py = oy as i32 + m.y - f.hotspot_y - miny + 2 + y as i32;
                        img.put_pixel(px as u32, py as u32, image::Rgba(p));
                    }
                }
            }
        }
        let (hx, hy) = (ox as i32 - minx + 2, oy as i32 - miny + 2);
        img.put_pixel(hx as u32, hy as u32, image::Rgba([255, 0, 0, 255]));
    }
    println!("{} frames, showing {} ({} cols), cell {}x{}", frames.len(), picked.len(), cols, cw, ch);
    let img = image::imageops::resize(&img, img.width() * 3, img.height() * 3, image::imageops::FilterType::Nearest);
    img.save(out)?;
    Ok(())
}

/// Developer helper: thumbnails of every terrain texture in a folder.
fn texsheet(dir: &str, out: &str) -> Result<()> {
    let mut names: Vec<_> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "dds")).collect();
    names.sort();
    let t = 96u32;
    let cols = 10u32;
    let rows = (names.len() as u32).div_ceil(cols);
    let mut img = image::RgbaImage::new(cols * t, rows * t);
    for (i, p) in names.iter().enumerate() {
        let Ok(tex) = aoe::dds::read_dxt1(&std::fs::read(p)?) else {
            println!("{i:2} {} (skipped)", p.display());
            continue;
        };
        let (ox, oy) = ((i as u32 % cols) * t, (i as u32 / cols) * t);
        for y in 0..t {
            for x in 0..t {
                let sx = x as usize * tex.w / t as usize / 4;
                let sy = y as usize * tex.h / t as usize / 4;
                img.put_pixel(ox + x, oy + y, image::Rgba(tex.px[sy * tex.w + sx]));
            }
        }
        println!("{i:2} {}", p.file_name().unwrap().to_string_lossy());
    }
    img.save(out)?;
    Ok(())
}

/// Developer helper: write each frame of an .sld (or a .dds) as PNG.
fn dump(src: &str, out: &str) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let data = std::fs::read(src)?;
    if src.ends_with(".dds") {
        let t = aoe::dds::read_dxt1(&data)?;
        let flat: Vec<u8> = t.px.iter().flatten().copied().collect();
        image::RgbaImage::from_raw(t.w as u32, t.h as u32, flat).unwrap().save(format!("{out}/tex.png"))?;
        return Ok(());
    }
    let frames = aoe::sld::parse(&data)?;
    println!("{} frames", frames.len());
    for (i, f) in frames.iter().enumerate() {
        if let Some(m) = &f.main {
            if i < 3 {
                println!(
                    "frame {i}: hotspot ({},{}) main {}x{} at ({},{}) shadow {:?}",
                    f.hotspot_x,
                    f.hotspot_y,
                    m.w,
                    m.h,
                    m.x,
                    m.y,
                    f.shadow.as_ref().map(|s| (s.w, s.h, s.x, s.y))
                );
            }
            let flat: Vec<u8> = m.px.iter().flatten().copied().collect();
            image::RgbaImage::from_raw(m.w as u32, m.h as u32, flat).unwrap().save(format!("{out}/f{i:03}.png"))?;
        }
    }
    Ok(())
}

/// Sample icons for the dev commands, plus extra ones for testing a layout:
/// WOLOLAND_EXTRA="x,y;x,y" (top-left corners). WOLOLAND_ICONS=0 drops the sample columns.
fn test_icons(w: usize, h: usize, n: usize) -> Vec<scene::Rect> {
    let n = std::env::var("WOLOLAND_ICONS").ok().and_then(|v| v.parse().ok()).unwrap_or(n);
    let mut icons = scene::sample_icons(w, h, 1.0, n);
    for part in std::env::var("WOLOLAND_EXTRA").unwrap_or_default().split(';').filter(|p| !p.is_empty()) {
        let v: Vec<f32> = part.split(',').filter_map(|t| t.trim().parse().ok()).collect();
        if v.len() == 2 {
            icons.push(scene::Rect { x0: v[0], y0: v[1], x1: v[0] + 72.0, y1: v[1] + 84.0, pad: 0.0 });
        }
    }
    icons
}

/// Display scale for the dev commands (WOLOLAND_SCALE=1.25 previews a 125% desktop).
fn dev_scale() -> f32 {
    std::env::var("WOLOLAND_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0)
}
