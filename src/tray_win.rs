//! The notification-area (tray) icon on Windows, whose color shows the
//! connection; its menu; pop-ups (shown by Windows as notifications); and
//! starting at sign-in, through the registry's Run key.
use crate::monitor::{Core, Status};
use crate::win::wide;
use chrono::{Duration as Span, Local};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "NetWatchTray";
/// The icon's own messages, from the shell.
const WM_TRAY: u32 = WM_APP + 1;
/// Redraw the icon and tooltip.
const WM_REFRESH: u32 = WM_APP + 2;
/// Open the window (another copy of Net Watch asked).
const WM_SHOW: u32 = WM_APP + 3;
/// Show the pop-ups waiting in `POPUPS`.
const WM_POPUP: u32 = WM_APP + 4;
/// The icon chosen with the keyboard (NIN_SELECT | NINF_KEY).
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

static HWND_TRAY: AtomicIsize = AtomicIsize::new(0);
static CORE: OnceLock<Core> = OnceLock::new();
static POPUPS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// Explorer's "the taskbar is back" message, to put the icon back after
/// it restarts.
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

fn post(msg: u32) {
    let h = HWND_TRAY.load(Ordering::Relaxed);
    if h != 0 {
        unsafe { PostMessageW(h as HWND, msg, 0, 0) };
    }
}

/// Show the tray icon, on a thread of its own with its own message loop.
pub fn start(core: Core) -> bool {
    let _ = CORE.set(core);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || unsafe {
        let instance = GetModuleHandleW(null());
        let class = wide(CLASS);
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&wc);
        let title = wide("Net Watch");
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), WS_OVERLAPPED, 0, 0, 0, 0, null_mut(), null_mut(), instance, null());
        if hwnd.is_null() {
            let _ = tx.send(false);
            return;
        }
        HWND_TRAY.store(hwnd as isize, Ordering::Relaxed);
        let _ = TASKBAR_CREATED.set(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()));
        let added = add_icon(hwnd);
        let _ = tx.send(added);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
    let ok = rx.recv().unwrap_or(false);
    if !ok {
        eprintln!("net-watch: no notification area available; closing the window minimizes it");
    }
    ok
}

/// Ask a copy of Net Watch that's already running to show its window.
/// False when there's none to ask.
pub fn wake_other() -> bool {
    let class = wide(CLASS);
    let h = unsafe { FindWindowW(class.as_ptr(), null()) };
    !h.is_null() && unsafe { PostMessageW(h, WM_SHOW, 0, 0) } != 0
}

/// Redraw the icon and tooltip from the shared state.
pub fn refresh() {
    post(WM_REFRESH);
}

/// A pop-up, shown by Windows as a notification from the tray icon.
pub fn notify(title: &str, body: &str) {
    POPUPS.lock().unwrap().push((title.to_string(), body.to_string()));
    post(WM_POPUP);
}

fn copy(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    d.hWnd = hwnd;
    d.uID = 1;
    d
}

/// An icon from RGBA pixels.
fn make_icon(size: u32, status: Status) -> HICON {
    let rgba = crate::icon::status_rgba(size, status);
    unsafe {
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = size as i32;
        bmi.bmiHeader.biHeight = -(size as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits = null_mut();
        let color = CreateDIBSection(null_mut(), &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if color.is_null() || bits.is_null() {
            return null_mut();
        }
        let out = std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize);
        for (o, p) in out.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            o.copy_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, null());
        let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color as HGDIOBJ);
        DeleteObject(mask as HGDIOBJ);
        icon
    }
}

/// The icon for each status, made once.
fn icon(status: Status) -> HICON {
    static ICONS: OnceLock<Vec<(Status, isize)>> = OnceLock::new();
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
    let icons = ICONS.get_or_init(|| {
        [Status::Starting, Status::Up, Status::Shaky, Status::Down, Status::Paused].into_iter().map(|s| (s, make_icon(size, s) as isize)).collect()
    });
    icons.iter().find(|(s, _)| *s == status).map_or(null_mut(), |(_, h)| *h as HICON)
}

fn tooltip() -> (Status, String) {
    let Some(core) = CORE.get() else { return (Status::Starting, "Net Watch".into()) };
    let st = core.state.lock().unwrap();
    let mut tip = format!("Net Watch\n{}", crate::headline(&st));
    if let Some(line) = live_line(&st) {
        tip += &format!("\n{line}");
    }
    (st.status(), tip)
}

unsafe fn add_icon(hwnd: HWND) -> bool {
    let (status, tip) = tooltip();
    let mut d = data(hwnd);
    d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    d.uCallbackMessage = WM_TRAY;
    d.hIcon = icon(status);
    copy(&mut d.szTip, &tip);
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &d) } != 0;
    d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    unsafe { Shell_NotifyIconW(NIM_SETVERSION, &d) };
    added
}

unsafe fn update_icon(hwnd: HWND) {
    let (status, tip) = tooltip();
    let mut d = data(hwnd);
    d.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    d.hIcon = icon(status);
    copy(&mut d.szTip, &tip);
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
}

unsafe fn show_popups(hwnd: HWND) {
    let popups = std::mem::take(&mut *POPUPS.lock().unwrap());
    for (title, body) in popups {
        let mut d = data(hwnd);
        d.uFlags = NIF_INFO;
        copy(&mut d.szInfoTitle, &title);
        copy(&mut d.szInfo, &body);
        d.dwInfoFlags = NIIF_INFO;
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }
}

const OPEN: usize = 1;
const TEST: usize = 2;
const PAUSE: usize = 3;
const AUTOSTART: usize = 4;
const QUIT: usize = 5;

unsafe fn menu(hwnd: HWND, x: i32, y: i32) {
    let Some(core) = CORE.get() else { return };
    let mut info = Vec::new();
    let (paused, testing, ready) = {
        let st = core.state.lock().unwrap();
        info.push(crate::headline(&st));
        if let Some(line) = live_line(&st) {
            info.push(line);
        }
        match st.speeds.last() {
            Some(s) if s.error.is_some() => info.push(format!("Last speed test failed ({})", crate::when(s.t))),
            Some(s) => info.push(format!("Last speed test: {:.0} ↓ / {:.0} ↑ Mbps", s.down, s.up)),
            None => {}
        }
        if let Some(o) = st.outages.last() {
            info.push(format!("Last outage: {} ({})", crate::when(o.start), crate::span(o.secs(Local::now()))));
        }
        (st.paused_until.is_some(), st.testing, crate::ookla::ready(&st.prefs))
    };
    unsafe {
        let m = CreatePopupMenu();
        let add = |flags: u32, id: usize, text: &str| {
            let w = wide(text);
            AppendMenuW(m, flags, id, w.as_ptr());
        };
        for line in &info {
            add(MF_STRING | MF_GRAYED, 0, line);
        }
        add(MF_SEPARATOR, 0, "");
        add(MF_STRING, OPEN, if core.window_open() { "Hide Net Watch" } else { "Open Net Watch" });
        match (testing, ready) {
            (true, _) => add(MF_STRING | MF_GRAYED, TEST, "Testing speed…"),
            (false, true) => add(MF_STRING | if paused { MF_GRAYED } else { 0 }, TEST, "Test Speed Now"),
            (false, false) => add(MF_STRING, TEST, "Set Up Speed Tests…"),
        }
        add(MF_STRING, PAUSE, if paused { "Resume Checks" } else { "Pause Checks for 1 Hour" });
        add(MF_SEPARATOR, 0, "");
        add(MF_STRING | if autostart() { MF_CHECKED } else { 0 }, AUTOSTART, "Start When I Sign In");
        add(MF_STRING, QUIT, "Quit");
        // So the menu goes away when clicking elsewhere.
        SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, x, y, 0, hwnd, null()) as usize;
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(m);
        match chosen {
            OPEN => core.toggle_window(),
            TEST => core.test_or_set_up(),
            PAUSE if paused => core.pause(None),
            PAUSE => core.pause(Some(Local::now() + Span::hours(1))),
            AUTOSTART => {
                let _ = set_autostart(!autostart());
            }
            QUIT => core.quit(),
            _ => {}
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAY => {
                // Version 4: the event in the low word, the place in wparam.
                match (lparam & 0xffff) as u32 {
                    NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONDBLCLK => {
                        if let Some(core) = CORE.get() {
                            core.toggle_window();
                        }
                    }
                    WM_CONTEXTMENU | WM_RBUTTONUP => {
                        let (x, y) = ((wparam & 0xffff) as i16 as i32, ((wparam >> 16) & 0xffff) as i16 as i32);
                        menu(hwnd, x, y);
                    }
                    _ => {}
                }
                0
            }
            WM_REFRESH => {
                update_icon(hwnd);
                0
            }
            WM_POPUP => {
                show_popups(hwnd);
                0
            }
            WM_SHOW => {
                if let Some(core) = CORE.get() {
                    core.show_window();
                }
                0
            }
            m if Some(&m) == TASKBAR_CREATED.get() => {
                add_icon(hwnd);
                0
            }
            WM_DESTROY => {
                let d = data(hwnd);
                Shell_NotifyIconW(NIM_DELETE, &d);
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Take the icon away, so it doesn't linger after quitting.
pub fn remove() {
    let h = HWND_TRAY.load(Ordering::Relaxed);
    if h != 0 {
        let d = data(h as HWND);
        unsafe { Shell_NotifyIconW(NIM_DELETE, &d) };
    }
}

/// "Speed test: download 812 Mbps · 0:12 left" while one runs.
fn live_line(st: &crate::monitor::Shared) -> Option<String> {
    let live = st.live.as_ref().filter(|_| st.testing)?;
    let now = match live.step {
        "download" => live.down.last().map(|s| format!("download {:.0} Mbps", s.1)),
        "upload" => live.up.last().map(|s| format!("upload {:.0} Mbps", s.1)),
        _ => None,
    };
    Some(format!("Speed test: {} · {} left", now.unwrap_or_else(|| "starting".into()), crate::test_left(st)))
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "Net Watch";

pub fn autostart() -> bool {
    crate::win::reg_string(windows_sys::Win32::System::Registry::HKEY_CURRENT_USER, RUN_KEY, RUN_NAME).is_some()
}

/// Add or remove the sign-in entry, which starts this program in the tray.
pub fn set_autostart(on: bool) -> std::io::Result<()> {
    use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;
    if !on {
        return crate::win::reg_delete(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME);
    }
    let exe = std::env::current_exe()?;
    crate::win::reg_set_string(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME, &format!("\"{}\" --hidden", exe.display()))
}
