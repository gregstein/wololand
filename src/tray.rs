//! Notification-area icon and menu. Runs on GPUI's main thread (it pumps our window's
//! messages) and forwards choices to the app through a channel.
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::OnceLock;

use futures::channel::mpsc::UnboundedSender;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::scene::Theme;

#[derive(Debug, Clone, Copy)]
pub enum Cmd {
    OpenSettings,
    SetTheme(Theme),
    ToggleAnimate,
    TogglePause,
    Shuffle,
    Reattach,
    Exit,
}

pub const CLASS: PCWSTR = w!("WololandTrayWindow");
pub const WM_TRAY: u32 = WM_APP + 1;
/// Sent by a second instance.
pub const WM_OPEN_SETTINGS: u32 = WM_APP + 10;
pub const WM_EXIT: u32 = WM_APP + 11;
/// `wparam` = index into `Theme::ALL`.
pub const WM_SET_THEME: u32 = WM_APP + 12;

static TX: OnceLock<UnboundedSender<Cmd>> = OnceLock::new();
static THEME: AtomicU8 = AtomicU8::new(0);
static ANIMATE: AtomicBool = AtomicBool::new(true);
static PAUSED: AtomicBool = AtomicBool::new(false);
static mut TASKBAR_CREATED: u32 = 0;

/// Mirror app state for the menu check marks.
pub fn sync(theme: Theme, animate: bool, paused: bool) {
    THEME.store(Theme::ALL.iter().position(|t| *t == theme).unwrap_or(0) as u8, Ordering::Relaxed);
    ANIMATE.store(animate, Ordering::Relaxed);
    PAUSED.store(paused, Ordering::Relaxed);
}

fn send(c: Cmd) {
    if let Some(tx) = TX.get() {
        let _ = tx.unbounded_send(c);
    }
}

pub struct Tray {
    hwnd: HWND,
    nid: NOTIFYICONDATAW,
}

impl Tray {
    pub fn create(tx: UnboundedSender<Cmd>) -> Option<Tray> {
        let _ = TX.set(tx);
        unsafe {
            TASKBAR_CREATED = RegisterWindowMessageW(w!("TaskbarCreated"));
            let hinst = GetModuleHandleW(None).ok()?;
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: CLASS, ..Default::default() };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(Default::default(), CLASS, w!("Wololand"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None).ok()?;
            // the app icon embedded by build.rs, at the small-icon size the tray wants
            let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
            let icon = LoadImageW(Some(hinst.into()), PCWSTR(1 as _), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR)
                .ok()
                .map(|h| HICON(h.0))
                .or_else(|| LoadIconW(None, IDI_APPLICATION).ok());
            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: WM_TRAY,
                hIcon: icon.unwrap_or_default(),
                ..Default::default()
            };
            let tip: Vec<u16> = "Wololand".encode_utf16().collect();
            nid.szTip[..tip.len()].copy_from_slice(&tip);
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
            Some(Tray { hwnd, nid })
        }
    }

    /// Re-add the icon after Explorer restarted.
    pub fn readd(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &self.nid);
        }
    }

    pub fn remove(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.nid);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

const ID_THEME: usize = 100;
const ID_ANIMATE: usize = 200;
const ID_SHUFFLE: usize = 201;
const ID_PAUSE: usize = 202;
const ID_SETTINGS: usize = 203;
const ID_EXIT: usize = 204;

fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let cur = THEME.load(Ordering::Relaxed) as usize;
        for (i, t) in Theme::ALL.iter().enumerate() {
            let name: Vec<u16> = t.name().encode_utf16().chain(Some(0)).collect();
            let flags = if i == cur { MF_STRING | MF_CHECKED } else { MF_STRING };
            let _ = AppendMenuW(menu, flags, ID_THEME + i, PCWSTR(name.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let check = |b: bool| if b { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(menu, check(ANIMATE.load(Ordering::Relaxed)), ID_ANIMATE, w!("Animate units and water"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SHUFFLE, w!("Shuffle layout"));
        let _ = AppendMenuW(menu, check(PAUSED.load(Ordering::Relaxed)), ID_PAUSE, w!("Pause"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Settings..."));
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Exit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, Some(0), hwnd, None).0 as usize;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match id {
            x if (ID_THEME..ID_THEME + Theme::ALL.len()).contains(&x) => send(Cmd::SetTheme(Theme::ALL[x - ID_THEME])),
            ID_ANIMATE => send(Cmd::ToggleAnimate),
            ID_SHUFFLE => send(Cmd::Shuffle),
            ID_PAUSE => send(Cmd::TogglePause),
            ID_SETTINGS => send(Cmd::OpenSettings),
            ID_EXIT => send(Cmd::Exit),
            _ => {}
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lp.0 as u32 & 0xffff {
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                WM_LBUTTONDBLCLK | WM_LBUTTONUP => send(Cmd::OpenSettings),
                _ => {}
            }
            LRESULT(0)
        }
        WM_OPEN_SETTINGS => {
            send(Cmd::OpenSettings);
            LRESULT(0)
        }
        WM_EXIT => {
            send(Cmd::Exit);
            LRESULT(0)
        }
        WM_SET_THEME => {
            if let Some(&t) = Theme::ALL.get(wp.0) {
                send(Cmd::SetTheme(t));
            }
            LRESULT(0)
        }
        m if m != 0 && m == unsafe { TASKBAR_CREATED } => {
            send(Cmd::Reattach);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Tell a running instance to do something. Returns false when none is running.
pub fn signal_running(msg: u32) -> bool {
    signal_running_with(msg, 0)
}

pub fn signal_running_with(msg: u32, wparam: usize) -> bool {
    unsafe {
        match FindWindowW(CLASS, PCWSTR::null()) {
            Ok(h) if !h.is_invalid() => PostMessageW(Some(h), msg, WPARAM(wparam), LPARAM(0)).is_ok(),
            _ => false,
        }
    }
}
