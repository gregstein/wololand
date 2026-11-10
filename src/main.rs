//! Wololand: Age of Empires II map themes that grow around your Windows desktop icons.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actors;
mod aoe;
mod app;
mod assets;
mod desktop;
mod noise;
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
    let theme = match a.first().map(|s| s.to_lowercase()).as_deref() {
        Some("arena") => scene::Theme::Arena,
        Some("blackforest") | Some("bf") => scene::Theme::BlackForest,
        Some("islands") => scene::Theme::Islands,
        _ => scene::Theme::Arabia,
    };
    let out = a.get(1).cloned().unwrap_or_else(|| "scene.png".into());
    let w: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(1920);
    let h: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(1080);
    let n: usize = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(14);
    let game = assets::Game::locate(None).ok_or_else(|| anyhow::anyhow!("AoE2 DE not found"))?;
    let t0 = std::time::Instant::now();
    let kit = scene::ThemeKit::load(&game, theme, 1.0)?;
    let t1 = std::time::Instant::now();
    let icons = scene::sample_icons(w, h, 1.0, n);
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
