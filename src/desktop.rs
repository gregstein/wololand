//! Win32 glue: parks a window under the desktop icons, reads icon rectangles from the
//! desktop list view, and detects when a maximised / full-screen window hides the wallpaper.
#![allow(clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromWindow, RedrawWindow, ScreenToClient, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY, RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows::Win32::System::Memory::{VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::*;

pub fn hwnd_of(window: &impl raw_window_handle::HasWindowHandle) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut c_void)),
        _ => None,
    }
}

fn find(parent: Option<HWND>, after: Option<HWND>, class: PCWSTR) -> Option<HWND> {
    unsafe { FindWindowExW(parent, after, class, PCWSTR::null()).ok().filter(|h| !h.is_invalid()) }
}

fn progman() -> Option<HWND> {
    unsafe { FindWindowW(w!("Progman"), PCWSTR::null()).ok().filter(|h| !h.is_invalid()) }
}

/// Top-level window that hosts SHELLDLL_DefView (pre-24H2 layout).
fn defview_host() -> Option<HWND> {
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        if find(Some(h), None, w!("SHELLDLL_DefView")).is_some() {
            unsafe { *(l.0 as *mut HWND) = h };
            return false.into();
        }
        true.into()
    }
    let mut host = HWND::default();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut host as *mut HWND as isize));
    }
    (!host.is_invalid()).then_some(host)
}

/// Ask Explorer to create the wallpaper WorkerW (sent once; a second 0xD/0 would destroy it).
fn spawn_workerw(progman: HWND, ready: impl Fn() -> bool) {
    unsafe {
        let _ = SendMessageTimeoutW(progman, 0x052C, WPARAM(0xD), LPARAM(1), SMTO_NORMAL, 1000, None);
    }
    for _ in 0..20 {
        if ready() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The window that holds the desktop icons (Progman on Windows 11 24H2+, else a WorkerW).
/// Overlays become its children, stacked above the icon layer.
pub fn icon_host() -> Option<HWND> {
    let dv = progman()
        .and_then(|p| find(Some(p), None, w!("SHELLDLL_DefView")))
        .or_else(|| defview_host().and_then(|h| find(Some(h), None, w!("SHELLDLL_DefView"))))?;
    Some(unsafe { GetAncestor(dv, GA_PARENT) }).filter(|h| !h.is_invalid())
}

/// Primary monitor rectangle in physical pixels.
pub fn primary_rect() -> RECT {
    unsafe {
        let mon = MonitorFromWindow(HWND::default(), MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        mi.rcMonitor
    }
}

static OUR_HWND: AtomicIsize = AtomicIsize::new(0);
static ANCHOR: AtomicIsize = AtomicIsize::new(0);
static PARENT: AtomicIsize = AtomicIsize::new(0);

pub struct Attachment {
    pub hwnd: HWND,
    parent: HWND,
    raised: bool,
    hook: Option<HWINEVENTHOOK>,
}

/// Re-parent `hwnd` so it is drawn as the wallpaper, below the desktop icons.
pub fn attach(hwnd: HWND) -> Option<Attachment> {
    unsafe {
        let screen = primary_rect();
        let progman = progman()?;
        let defview = find(Some(progman), None, w!("SHELLDLL_DefView"));
        let raised = defview.is_some();
        let (parent, insert_after) = if let Some(dv) = defview {
            // Windows 11 24H2+: icons live directly in Progman. Become a layered child of
            // Progman placed right below the icon layer and above the system wallpaper WorkerW.
            if find(Some(progman), None, w!("WorkerW")).is_none() {
                spawn_workerw(progman, || find(Some(progman), None, w!("WorkerW")).is_some());
            }
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
            SetWindowLongW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as i32);
            let _ = SetLayeredWindowAttributes(hwnd, Default::default(), 255, LWA_ALPHA);
            (progman, Some(dv))
        } else {
            // Classic layout: draw inside the WorkerW that sits behind the icon host.
            let worker = || defview_host().and_then(|h| find(None, Some(h), w!("WorkerW")));
            if worker().is_none() {
                spawn_workerw(progman, || worker().is_some());
            }
            (worker()?, None)
        };
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let style = (style & !(WS_POPUP.0 | WS_CAPTION.0 | WS_THICKFRAME.0 | WS_SYSMENU.0)) | WS_CHILD.0 | WS_VISIBLE.0;
        SetWindowLongW(hwnd, GWL_STYLE, style as i32);
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        SetWindowLongW(hwnd, GWL_EXSTYLE, ((ex & !WS_EX_APPWINDOW.0) | WS_EX_TOOLWINDOW.0) as i32);
        SetParent(hwnd, Some(parent)).ok()?;
        let mut pt = POINT { x: screen.left, y: screen.top };
        let _ = ScreenToClient(parent, &mut pt);
        let _ = SetWindowPos(
            hwnd,
            insert_after,
            pt.x,
            pt.y,
            screen.right - screen.left,
            screen.bottom - screen.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED,
        );
        OUR_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
        PARENT.store(parent.0 as isize, Ordering::Relaxed);
        ANCHOR.store(insert_after.map_or(0, |h| h.0 as isize), Ordering::Relaxed);
        let mut hook = None;
        if raised {
            ensure_z_order();
            let mut pid = 0u32;
            GetWindowThreadProcessId(progman, Some(&mut pid));
            unsafe extern "system" fn on_event(_: HWINEVENTHOOK, _: u32, _: HWND, id_object: i32, _: i32, _: u32, _: u32) {
                // Explorer fires these for every window it creates or moves; at most one fix per
                // 50 ms (the 400 ms poll catches anything skipped).
                static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
                let now = START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1000;
                if id_object == 0 && now.saturating_sub(LAST.load(Ordering::Relaxed)) >= 50 {
                    LAST.store(now, Ordering::Relaxed);
                    ensure_z_order();
                }
            }
            // OBJECT_CREATE..OBJECT_REORDER in Explorer: fix the order as soon as it is disturbed.
            let h = SetWinEventHook(0x8000, 0x8004, None, Some(on_event), pid, 0, 0);
            hook = (!h.is_invalid()).then_some(h);
        }
        Some(Attachment { hwnd, parent, raised, hook })
    }
}

/// Keep our window directly below the icon layer, and the system wallpaper WorkerW at the bottom.
pub fn ensure_z_order() {
    let ours = HWND(OUR_HWND.load(Ordering::Relaxed) as *mut c_void);
    let anchor = HWND(ANCHOR.load(Ordering::Relaxed) as *mut c_void);
    let parent = HWND(PARENT.load(Ordering::Relaxed) as *mut c_void);
    if ours.is_invalid() || anchor.is_invalid() {
        return;
    }
    unsafe {
        if !IsWindow(Some(anchor)).as_bool() {
            return;
        }
        if GetWindow(anchor, GW_HWNDNEXT).ok() != Some(ours) {
            let _ = SetWindowPos(ours, Some(anchor), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        // Every WorkerW (the system wallpaper) must sit below us. Only move the ones that are
        // above: pushing each one to the very bottom on every call would make several WorkerWs
        // swap places forever, each move firing another reorder event in Explorer.
        let mut below: Vec<HWND> = Vec::new();
        let mut next = GetWindow(ours, GW_HWNDNEXT);
        while let Ok(h) = next {
            if h.is_invalid() || below.len() >= 256 {
                break;
            }
            below.push(h);
            next = GetWindow(h, GW_HWNDNEXT);
        }
        let mut wk = find(Some(parent), None, w!("WorkerW"));
        while let Some(h) = wk {
            if !below.contains(&h) {
                let _ = SetWindowPos(h, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            wk = find(Some(parent), Some(h), w!("WorkerW"));
        }
    }
}

pub enum AttachState {
    Ok,
    /// The window no longer exists (Explorer restarted and destroyed it with Progman).
    Gone,
    /// The window exists but is no longer a child of the desktop.
    Detached,
}

pub fn hide(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

pub fn attachment_state(hwnd: HWND) -> AttachState {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return AttachState::Gone;
        }
        let parent = HWND(PARENT.load(Ordering::Relaxed) as *mut c_void);
        let ok = !parent.is_invalid() && IsWindow(Some(parent)).as_bool() && GetParent(hwnd).ok() == Some(parent);
        if ok { AttachState::Ok } else { AttachState::Detached }
    }
}

impl Attachment {
    /// Drop the event hook only (used when the window itself is already gone).
    pub fn release(&mut self) {
        unsafe {
            if let Some(h) = self.hook.take() {
                let _ = UnhookWinEvent(h);
            }
        }
        OUR_HWND.store(0, Ordering::Relaxed);
    }

    /// Undo the attachment and let Explorer repaint the normal wallpaper.
    pub fn detach(&mut self) {
        unsafe {
            if let Some(h) = self.hook.take() {
                let _ = UnhookWinEvent(h);
            }
            OUR_HWND.store(0, Ordering::Relaxed);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            let _ = SetParent(self.hwnd, None);
            if IsWindow(Some(self.parent)).as_bool() {
                let _ = RedrawWindow(Some(self.parent), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW);
            }
        }
        let _ = self.raised;
    }
}

/// Reads icon rectangles (icon + label) from the desktop SysListView32 in Explorer's process.
pub struct IconReader {
    list: HWND,
    proc_: HANDLE,
    mem: *mut c_void,
}

const LVM_GETITEMCOUNT: u32 = 0x1004;
const LVM_GETITEMRECT: u32 = 0x100E;

impl IconReader {
    pub fn new() -> Self {
        Self { list: HWND::default(), proc_: HANDLE::default(), mem: std::ptr::null_mut() }
    }

    fn open(&mut self) -> bool {
        self.close();
        let dv = progman()
            .and_then(|p| find(Some(p), None, w!("SHELLDLL_DefView")))
            .or_else(|| defview_host().and_then(|h| find(Some(h), None, w!("SHELLDLL_DefView"))));
        let Some(list) = dv.and_then(|d| find(Some(d), None, w!("SysListView32"))) else { return false };
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(list, Some(&mut pid));
            let Ok(p) = OpenProcess(PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE, false, pid) else { return false };
            let mem = VirtualAllocEx(p, None, 64, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
            if mem.is_null() {
                let _ = CloseHandle(p);
                return false;
            }
            self.list = list;
            self.proc_ = p;
            self.mem = mem;
        }
        true
    }

    /// Icon rectangles in physical screen pixels. `None` when the list view is unavailable;
    /// an empty list when the user has hidden desktop icons.
    pub fn read(&mut self) -> Option<Vec<RECT>> {
        self.rects(0)
    }

    /// The picture part of each icon (no label), in physical screen pixels.
    pub fn icon_rects(&mut self) -> Option<Vec<RECT>> {
        self.rects(1)
    }

    /// `part`: LVIR_BOUNDS (0) or LVIR_ICON (1).
    fn rects(&mut self, part: i32) -> Option<Vec<RECT>> {
        unsafe {
            if self.mem.is_null() || !IsWindow(Some(self.list)).as_bool() {
                if !self.open() {
                    return None;
                }
            }
            if !IsWindowVisible(self.list).as_bool() {
                return Some(Vec::new());
            }
            let count = SendMessageW(self.list, LVM_GETITEMCOUNT, None, None).0.max(0) as usize;
            let mut origin = POINT::default();
            let _ = ClientToScreen(self.list, &mut origin);
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let mut r = RECT { left: part, ..Default::default() };
                let sz = std::mem::size_of::<RECT>();
                if WriteProcessMemory(self.proc_, self.mem, &r as *const RECT as *const c_void, sz, None).is_err() {
                    self.close();
                    return None;
                }
                SendMessageW(self.list, LVM_GETITEMRECT, Some(WPARAM(i)), Some(LPARAM(self.mem as isize)));
                if ReadProcessMemory(self.proc_, self.mem, &mut r as *mut RECT as *mut c_void, sz, None).is_err() {
                    self.close();
                    return None;
                }
                out.push(RECT {
                    left: r.left + origin.x,
                    top: r.top + origin.y,
                    right: r.right + origin.x,
                    bottom: r.bottom + origin.y,
                });
            }
            Some(out)
        }
    }

    fn close(&mut self) {
        unsafe {
            if !self.mem.is_null() {
                let _ = VirtualFreeEx(self.proc_, self.mem, 0, MEM_RELEASE);
            }
            if !self.proc_.is_invalid() {
                let _ = CloseHandle(self.proc_);
            }
        }
        self.mem = std::ptr::null_mut();
        self.proc_ = HANDLE::default();
    }
}

impl Drop for IconReader {
    fn drop(&mut self) {
        self.close();
    }
}

/// True when the foreground window covers the primary monitor or is maximised on it.
pub fn wallpaper_hidden(ours: HWND) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg == ours || !IsWindowVisible(fg).as_bool() {
            return false;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(fg, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut c_void, 4).is_ok() && cloaked != 0 {
            return false;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(fg, &mut cls) as usize;
        let c = String::from_utf16_lossy(&cls[..n]);
        if matches!(c.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return false;
        }
        let s = primary_rect();
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let covers = r.left <= s.left && r.top <= s.top && r.right >= s.right && r.bottom >= s.bottom;
        // a window covering nearly all of the screen hides the wallpaper too
        let iw = (r.right.min(s.right) - r.left.max(s.left)).max(0) as i64;
        let ih = (r.bottom.min(s.bottom) - r.top.max(s.top)).max(0) as i64;
        let area = ((s.right - s.left) as i64 * (s.bottom - s.top) as i64).max(1);
        let covers = covers || iw * ih * 100 >= area * 92;
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let on_primary = cx >= s.left && cx < s.right && cy >= s.top && cy < s.bottom;
        on_primary && (covers || IsZoomed(fg).as_bool())
    }
}

/// Run the calling thread at background priority (CPU, I/O and memory), for scene building.
pub fn background_thread() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
    }
}

/// One line of resource use for the log, to spot leaks on long runs.
pub fn health() -> String {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::{GetCurrentProcess, GetGuiResources, GetProcessHandleCount, GR_GDIOBJECTS, GR_USEROBJECTS};
    unsafe {
        let p = GetCurrentProcess();
        let mut m = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        let _ = GetProcessMemoryInfo(p, &mut m as *mut _ as *mut PROCESS_MEMORY_COUNTERS, m.cb);
        let mut handles = 0u32;
        let _ = GetProcessHandleCount(p, &mut handles);
        let gdi = GetGuiResources(p, GR_GDIOBJECTS);
        let user = GetGuiResources(p, GR_USEROBJECTS);
        format!("health: private {} MB, working set {} MB, handles {handles}, gdi {gdi}, user {user}", m.PrivateUsage >> 20, m.WorkingSetSize >> 20)
    }
}

/// Lower the whole process below normal priority: a wallpaper should never compete with apps.
pub fn lower_process_priority() {
    use windows::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS};
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
}
