//! Tray icon for the settings window (Windows): a notification-area icon with a small menu,
//! plus helpers to hide the settings window to the tray and bring it back.
//!
//! The icon lives on its own thread with a message-only window, so it keeps working while
//! the settings window is hidden.

use std::cell::RefCell;
use std::sync::Mutex;
use std::thread;

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, SetWindowSubclass, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// What the user picked in the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// left click or "Öffnen"
    Open,
    /// "Verbinden" / "Trennen"
    ToggleConnection,
    /// "Beenden"
    Quit,
}

const WM_TRAY: u32 = WM_APP + 20;
const WM_TRAY_UPDATE: u32 = WM_APP + 21;
const CMD_OPEN: usize = 1;
const CMD_TOGGLE: usize = 2;
const CMD_QUIT: usize = 3;
const ICON_ID: u32 = 1;

/// Tooltip and menu state shared with the tray thread.
struct Shown {
    tip: String,
    connected: bool,
}

static SHOWN: Mutex<Shown> = Mutex::new(Shown {
    tip: String::new(),
    connected: false,
});

type Handler = Box<dyn Fn(TrayEvent)>;

thread_local! {
    static ON_EVENT: RefCell<Option<Handler>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: RefCell<u32> = const { RefCell::new(0) };
}

/// The running tray icon. Dropping it removes the icon.
pub struct Tray {
    hwnd: usize,
}

impl Tray {
    /// Shows the icon. `on_event` runs on the tray thread.
    pub fn spawn(tip: &str, on_event: impl Fn(TrayEvent) + Send + 'static) -> Result<Tray> {
        SHOWN.lock().unwrap_or_else(|e| e.into_inner()).tip = tip.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        thread::Builder::new().name("tray".into()).spawn(move || {
            ON_EVENT.with(|f| *f.borrow_mut() = Some(Box::new(on_event)));
            match create_window() {
                Ok(hwnd) => {
                    let _ = tx.send(Ok(hwnd as usize));
                    unsafe {
                        let mut msg: MSG = std::mem::zeroed();
                        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        })?;
        let hwnd = rx.recv()??;
        Ok(Tray { hwnd })
    }

    /// Updates the tooltip (connection status) and the menu's connect / disconnect entry.
    pub fn set_status(&self, tip: &str, connected: bool) {
        {
            let mut s = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
            if s.tip == tip && s.connected == connected {
                return;
            }
            s.tip = tip.to_string();
            s.connected = connected;
        }
        unsafe { PostMessageW(self.hwnd as HWND, WM_TRAY_UPDATE, 0, 0) };
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn create_window() -> Result<HWND> {
    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let class_name = wide("Tast2ControllerTray");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        if RegisterClassExW(&wc) == 0 && windows_sys::Win32::Foundation::GetLastError() != 1410 {
            bail!(
                "RegisterClassExW failed: {}",
                std::io::Error::last_os_error()
            );
        }
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            bail!(
                "CreateWindowExW failed: {}",
                std::io::Error::last_os_error()
            );
        }
        let msg = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        TASKBAR_CREATED.with(|m| *m.borrow_mut() = msg);
        if !notify(hwnd, NIM_ADD) {
            DestroyWindow(hwnd);
            bail!("Shell_NotifyIconW failed");
        }
        Ok(hwnd)
    }
}

/// The program's own icon (resource 1, if the EXE has one), else the default one.
fn app_icon() -> HICON {
    unsafe {
        let own = LoadIconW(GetModuleHandleW(std::ptr::null()), 1 as _);
        if own.is_null() {
            LoadIconW(std::ptr::null_mut(), IDI_APPLICATION)
        } else {
            own
        }
    }
}

fn notify(hwnd: HWND, op: u32) -> bool {
    unsafe {
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = ICON_ID;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = app_icon();
        let tip = SHOWN.lock().unwrap_or_else(|e| e.into_inner()).tip.clone();
        for (dst, src) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *dst = src;
        }
        Shell_NotifyIconW(op, &data) != 0
    }
}

fn fire(ev: TrayEvent) {
    ON_EVENT.with(|f| {
        if let Some(f) = f.borrow().as_ref() {
            f(ev);
        }
    });
}

unsafe fn show_menu(hwnd: HWND) {
    let connected = SHOWN.lock().unwrap_or_else(|e| e.into_inner()).connected;
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let open = wide("Öffnen");
    let toggle = wide(if connected { "Trennen" } else { "Verbinden" });
    let quit = wide("Beenden");
    AppendMenuW(menu, MF_STRING, CMD_OPEN, open.as_ptr());
    AppendMenuW(menu, MF_STRING, CMD_TOGGLE, toggle.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, CMD_QUIT, quit.as_ptr());
    SetMenuDefaultItem(menu, CMD_OPEN as u32, 0);
    let mut p = POINT { x: 0, y: 0 };
    GetCursorPos(&mut p);
    // without this the menu doesn't close when clicking elsewhere
    SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        p.x,
        p.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
    match cmd as usize {
        CMD_OPEN => fire(TrayEvent::Open),
        CMD_TOGGLE => fire(TrayEvent::ToggleConnection),
        CMD_QUIT => fire(TrayEvent::Quit),
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match (lparam & 0xffff) as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => fire(TrayEvent::Open),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_TRAY_UPDATE => {
            notify(hwnd, NIM_MODIFY);
            0
        }
        WM_CLOSE => {
            notify(hwnd, NIM_DELETE);
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => {
            // Explorer restarted: the icon is gone, add it again
            if msg != 0 && TASKBAR_CREATED.with(|m| *m.borrow()) == msg {
                notify(hwnd, NIM_ADD);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Settings window: hide to the tray

unsafe extern "system" fn hide_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_SYSCOMMAND && (wparam & 0xfff0) as u32 == SC_MINIMIZE {
        ShowWindow(hwnd, SW_HIDE);
        return 0;
    }
    let r = DefSubclassProc(hwnd, msg, wparam, lparam);
    // minimized some other way (own title bar, Win+Down): hide it as well
    if msg == WM_SIZE && wparam as u32 == SIZE_MINIMIZED {
        ShowWindow(hwnd, SW_HIDE);
    }
    r
}

/// Makes minimizing the window hide it to the tray. Call once, on the window's thread.
pub fn hide_on_minimize(hwnd: isize) -> Result<()> {
    if unsafe { SetWindowSubclass(hwnd as HWND, Some(hide_proc), 0xC41C, 0) } == 0 {
        bail!(
            "SetWindowSubclass failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Hides the window (it stays reachable through the tray icon).
pub fn hide_window(hwnd: isize) {
    unsafe { ShowWindow(hwnd as HWND, SW_HIDE) };
}

/// Shows the window again and brings it to the front.
pub fn show_window(hwnd: isize) {
    unsafe {
        let hwnd = hwnd as HWND;
        ShowWindow(
            hwnd,
            if IsIconic(hwnd) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        SetForegroundWindow(hwnd);
    }
}
