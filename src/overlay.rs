//! Small click-through windows that sit just above the desktop icons, so Icon Wars can draw on
//! top of an icon (cracks, a health bar, its collapse into rubble) while every click still goes
//! to the real icon underneath. Each window is a per-pixel-alpha layered window filled on the CPU.
//!
//! They are top-level windows kept just above the window that holds the icons in the stacking
//! order (so every other app still covers them). They can't be children of it: on Windows 11 24H2
//! that window has no redirection surface, and layered children of it are never drawn.
#![allow(clippy::missing_safety_doc)]

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BitBlt,
    CAPTUREBLT, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::desktop;

unsafe extern "system" fn proc_(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    unsafe { DefWindowProcW(h, msg, wp, lp) }
}

fn register() -> PCWSTR {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let class = w!("WololandIconOverlay");
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc_),
            hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
    });
    class
}

/// A top-down 32-bit DIB with premultiplied BGRA pixels.
struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: usize,
    h: usize,
}

impl Dib {
    fn new(w: usize, h: usize) -> Option<Dib> {
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let old = SelectObject(dc, bmp.into());
            Some(Dib {
                dc,
                bmp,
                old,
                bits: bits as *mut u32,
                w,
                h,
            })
        }
    }

    fn px(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.w * self.h) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Copy a rectangle of the screen (physical pixels) as straight RGBA, or `None` if it failed.
pub fn capture(x: i32, y: i32, w: usize, h: usize) -> Option<Vec<[u8; 4]>> {
    let mut dib = Dib::new(w, h)?;
    unsafe {
        let screen = GetDC(None);
        let ok = BitBlt(
            dib.dc,
            0,
            0,
            w as i32,
            h as i32,
            Some(screen),
            x,
            y,
            SRCCOPY | CAPTUREBLT,
        )
        .is_ok();
        ReleaseDC(None, screen);
        if !ok {
            return None;
        }
    }
    Some(
        dib.px()
            .iter()
            .map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 255])
            .collect(),
    )
}

/// Class name of the window a click at screen point (`x`, `y`) would reach.
pub fn window_class_at(x: i32, y: i32) -> String {
    unsafe {
        let h = WindowFromPoint(POINT { x, y });
        let mut cls = [0u16; 64];
        let n = GetClassNameW(h, &mut cls) as usize;
        String::from_utf16_lossy(&cls[..n])
    }
}

/// The mouse position in screen pixels.
pub fn cursor() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok().map(|_| (p.x, p.y)) }
}

pub struct Overlay {
    hwnd: HWND,
    dib: Dib,
}

impl Overlay {
    /// A hidden overlay of `w`x`h` physical pixels at screen position (`x`, `y`).
    pub fn new(x: i32, y: i32, w: usize, h: usize) -> Option<Overlay> {
        desktop::icon_host()?;
        let dib = Dib::new(w, h)?;
        unsafe {
            let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
            let hwnd = match CreateWindowExW(
                ex,
                register(),
                w!(""),
                WS_POPUP,
                x,
                y,
                w as i32,
                h as i32,
                None,
                None,
                None,
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    crate::log(&format!("overlay window: {e}"));
                    eprintln!("overlay window: {e}");
                    return None;
                }
            };
            let o = Overlay { hwnd, dib };
            o.raise();
            Some(o)
        }
    }

    pub fn size(&self) -> (usize, usize) {
        (self.dib.w, self.dib.h)
    }

    /// Premultiplied BGRA pixels, row by row from the top.
    pub fn pixels(&mut self) -> &mut [u32] {
        self.dib.px()
    }

    /// False once the window is gone (Explorer restarted and took its parent with it).
    pub fn alive(&self) -> bool {
        unsafe { IsWindow(Some(self.hwnd)).as_bool() }
    }

    /// Put the window back just above the icon layer, under every other window (a window that
    /// is shown or closed can leave it anywhere in the stacking order).
    pub fn raise(&self) {
        let Some(host) = desktop::icon_host() else {
            return;
        };
        unsafe {
            // already in the run of overlays directly above the icons: nothing to do
            let mut above = GetWindow(host, GW_HWNDPREV).ok();
            while let Some(a) = above {
                if a == self.hwnd {
                    return;
                }
                let mut cls = [0u16; 64];
                let n = GetClassNameW(a, &mut cls) as usize;
                if String::from_utf16_lossy(&cls[..n]) != "WololandIconOverlay" {
                    break;
                }
                above = GetWindow(a, GW_HWNDPREV).ok();
            }
            // inserting after the window just above the icon host puts us between the two
            let after = GetWindow(host, GW_HWNDPREV).ok().unwrap_or(HWND_TOP);
            let _ = SetWindowPos(
                self.hwnd,
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    /// Show the pixels written with [`Overlay::pixels`].
    pub fn present(&self) {
        unsafe {
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let size = SIZE {
                cx: self.dib.w as i32,
                cy: self.dib.h as i32,
            };
            let src = POINT { x: 0, y: 0 };
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                None,
                Some(&size),
                Some(self.dib.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            if !IsWindowVisible(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Developer check: cover the first desktop icon with a translucent red box for a few seconds and
/// report which window a click at its centre would reach.
pub fn self_test() -> anyhow::Result<()> {
    let mut reader = desktop::IconReader::new();
    let icons = reader
        .icon_rects()
        .ok_or_else(|| anyhow::anyhow!("desktop icons not readable"))?;
    let r = icons
        .first()
        .ok_or_else(|| anyhow::anyhow!("no desktop icons"))?;
    let (w, h) = ((r.right - r.left) as usize, (r.bottom - r.top) as usize);
    println!("icon picture at {},{} size {w}x{h}", r.left, r.top);
    let mut o = Overlay::new(r.left, r.top, w, h)
        .ok_or_else(|| anyhow::anyhow!("overlay window failed"))?;
    for p in o.pixels() {
        *p = 0x9070_0000; // ~56% red, premultiplied
    }
    o.present();
    let t0 = std::time::Instant::now();
    while t0.elapsed().as_secs() < 8 {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let hit = WindowFromPoint(POINT {
                x: (r.left + r.right) / 2,
                y: (r.top + r.bottom) / 2,
            });
            let mut cls = [0u16; 64];
            let n = GetClassNameW(hit, &mut cls) as usize;
            if t0.elapsed().as_millis() % 2000 < 20 {
                println!(
                    "click at the icon reaches: {}",
                    String::from_utf16_lossy(&cls[..n])
                );
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    Ok(())
}
