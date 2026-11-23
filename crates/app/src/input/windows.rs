//! Windows front end: grabs keyboard and mouse through Raw Input, confines and hides the
//! cursor while capturing, and turns what it reads into controller state.
//!
//! The capture logic (the "host") can live in two places:
//! - its own small window with a status banner (command line `run`), or
//! - the settings window itself (GUI): a subclass of that window feeds it.
//!
//! Either way the host only turns window messages into mapper events; a separate pump thread
//! owns the mapper, runs its ticks and pushes the controller state to the stream.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    FillRect, GetSysColorBrush, InvalidateRect, SelectObject, SetBkMode, SetTextColor,
    COLOR_WINDOW, DT_LEFT, DT_NOPREFIX, DT_WORDBREAK, HDC, PAINTSTRUCT, TRANSPARENT,
};
use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_LWIN, VK_MENU, VK_RETURN, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::keys::{self, InputId};
use super::mapper::{Event, Mapper};
use crate::stream::ControllerLink;

const WM_APP_STATUS: u32 = WM_APP + 1;
/// wparam 1: start capturing (if the window is in front), 0: stop
const WM_APP_SETCAPTURE: u32 = WM_APP + 2;
/// restores the cursor after the host was detached from another thread
const WM_APP_UNCAPTURE: u32 = WM_APP + 3;
const MOUSE_MOVE_ABSOLUTE: u32 = 1;

static CAPTURING: AtomicBool = AtomicBool::new(false);

/// The one active host (there is at most one session at a time).
static HOST: Mutex<Option<Host>> = Mutex::new(None);

/// Whether keyboard and mouse currently go to the PS5.
pub fn is_capturing() -> bool {
    CAPTURING.load(Ordering::Relaxed)
}

/// Lets other threads repaint the status, toggle capture and end capturing.
#[derive(Clone, Copy)]
pub struct WindowHandle {
    hwnd: usize,
    embedded: bool,
}

impl WindowHandle {
    pub fn refresh(&self) {
        if !self.embedded {
            unsafe { PostMessageW(self.hwnd as HWND, WM_APP_STATUS, 0, 0) };
        }
    }

    pub fn set_capture(&self, on: bool) {
        unsafe { PostMessageW(self.hwnd as HWND, WM_APP_SETCAPTURE, on as usize, 0) };
    }

    /// Own window: closes it. Settings window: stops feeding input (the window stays).
    pub fn close(&self) {
        if self.embedded {
            detach(self.hwnd);
        } else {
            unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
        }
    }
}

/// What the host sends to the pump thread.
pub enum Msg {
    Input(Event, Instant),
    /// capture changed: release everything that is held
    Release,
}

pub struct HostOptions {
    pub capture_key: InputId,
    pub quit_key: InputId,
    pub capture_on_start: bool,
    /// set when the quit key is pressed in the settings window
    pub stop: Arc<AtomicBool>,
    /// shown by the own window
    pub status: Arc<Mutex<String>>,
    pub active_profile: Arc<Mutex<String>>,
}

/// (Re)claims Raw Input for this window. Registration is per process and may have been
/// taken by another window in the meantime.
fn register_raw_input(hwnd: HWND) -> bool {
    let devices = [
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: 0,
            hwndTarget: hwnd,
        }, // mouse
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x06,
            dwFlags: 0,
            hwndTarget: hwnd,
        }, // keyboard
    ];
    unsafe {
        RegisterRawInputDevices(
            devices.as_ptr(),
            devices.len() as u32,
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        ) != 0
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn on_window_thread(hwnd: usize) -> bool {
    unsafe { GetWindowThreadProcessId(hwnd as HWND, std::ptr::null_mut()) == GetCurrentThreadId() }
}

/// What to do once the host lock is released.
enum After {
    Nothing,
    Detach,
    Destroy,
}

struct Host {
    hwnd: usize,
    embedded: bool,
    tx: Sender<Msg>,
    capturing: bool,
    capture_key: InputId,
    quit_key: InputId,
    last_abs: Option<(i32, i32)>,
    last_input: String,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
    active_profile: Arc<Mutex<String>>,
}

impl Host {
    fn new(hwnd: usize, embedded: bool, tx: Sender<Msg>, opts: HostOptions) -> Host {
        Host {
            hwnd,
            embedded,
            tx,
            capturing: false,
            capture_key: opts.capture_key,
            quit_key: opts.quit_key,
            last_abs: None,
            last_input: String::new(),
            stop: opts.stop,
            status: opts.status,
            active_profile: opts.active_profile,
        }
    }

    fn hwnd(&self) -> HWND {
        self.hwnd as HWND
    }

    fn send(&self, ev: Event) {
        let _ = self.tx.send(Msg::Input(ev, Instant::now()));
    }

    fn set_capture(&mut self, on: bool) {
        if on == self.capturing {
            return;
        }
        self.capturing = on;
        CAPTURING.store(on, Ordering::Relaxed);
        unsafe {
            if on {
                register_raw_input(self.hwnd());
                self.clip();
                while ShowCursor(0) >= 0 {}
            } else {
                ClipCursor(std::ptr::null());
                while ShowCursor(1) < 0 {}
            }
        }
        let _ = self.tx.send(Msg::Release);
        self.last_abs = None;
        log::info!("input capture {}", if on { "on" } else { "off" });
        self.repaint();
    }

    /// Keeps the cursor inside the window while capturing.
    fn clip(&self) {
        unsafe {
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(self.hwnd(), &mut rc);
            let mut tl = POINT {
                x: rc.left,
                y: rc.top,
            };
            let mut br = POINT {
                x: rc.right,
                y: rc.bottom,
            };
            ClientToScreen(self.hwnd(), &mut tl);
            ClientToScreen(self.hwnd(), &mut br);
            let clip = RECT {
                left: tl.x,
                top: tl.y,
                right: br.x,
                bottom: br.y,
            };
            ClipCursor(&clip);
        }
    }

    fn repaint(&self) {
        if !self.embedded {
            unsafe { InvalidateRect(self.hwnd(), std::ptr::null(), 1) };
        }
    }

    fn quit(&mut self) -> After {
        log::info!("{} pressed: disconnecting", keys::name(self.quit_key));
        if self.embedded {
            self.set_capture(false);
            self.stop.store(true, Ordering::Relaxed);
            After::Detach
        } else {
            unsafe { PostMessageW(self.hwnd(), WM_CLOSE, 0, 0) };
            After::Nothing
        }
    }

    fn key_event(&mut self, id: InputId, down: bool) -> After {
        if down && id == self.quit_key {
            return self.quit();
        }
        if id == self.capture_key {
            if down {
                let c = !self.capturing;
                self.set_capture(c);
            }
            return After::Nothing;
        }
        if !self.capturing {
            return After::Nothing;
        }
        if down {
            self.last_input = keys::name(id);
            self.repaint();
        }
        self.send(if down { Event::Down(id) } else { Event::Up(id) });
        After::Nothing
    }

    fn raw_input(&mut self, lparam: LPARAM) -> After {
        let mut buf = [0u64; 16]; // 128 bytes, 8-aligned: enough for mouse and keyboard
        let mut size = std::mem::size_of_val(&buf) as u32;
        let got = unsafe {
            GetRawInputData(
                lparam as HRAWINPUT,
                RID_INPUT,
                buf.as_mut_ptr() as *mut _,
                &mut size,
                std::mem::size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if got == u32::MAX || got == 0 {
            return After::Nothing;
        }
        let raw = unsafe { &*(buf.as_ptr() as *const RAWINPUT) };
        match raw.header.dwType {
            RIM_TYPEKEYBOARD => {
                let kb = unsafe { raw.data.keyboard };
                let down = kb.Flags & RI_KEY_BREAK as u16 == 0;
                let e0 = kb.Flags & RI_KEY_E0 as u16 != 0;
                let vk = kb.VKey;
                if vk == 0xff {
                    return After::Nothing; // fake key from an escape sequence
                }
                let id: InputId = match vk {
                    VK_SHIFT => {
                        if kb.MakeCode == 0x36 {
                            keys::VK_RSHIFT
                        } else {
                            keys::VK_LSHIFT
                        }
                    }
                    VK_CONTROL => {
                        if e0 {
                            keys::VK_RCONTROL
                        } else {
                            keys::VK_LCONTROL
                        }
                    }
                    VK_MENU => {
                        if e0 {
                            keys::VK_RMENU
                        } else {
                            keys::VK_LMENU
                        }
                    }
                    VK_RETURN if e0 => keys::NUMPAD_ENTER,
                    v => v,
                };
                self.key_event(id, down)
            }
            RIM_TYPEMOUSE => {
                let m = unsafe { raw.data.mouse };
                let (flags, data) = unsafe {
                    (
                        m.Anonymous.Anonymous.usButtonFlags as u32,
                        m.Anonymous.Anonymous.usButtonData as i16,
                    )
                };
                if !self.capturing {
                    return After::Nothing;
                }
                let (dx, dy) = if m.usFlags as u32 & MOUSE_MOVE_ABSOLUTE != 0 {
                    // remote desktop / tablets report absolute positions
                    let d = self
                        .last_abs
                        .map(|(x, y)| (m.lLastX - x, m.lLastY - y))
                        .unwrap_or((0, 0));
                    self.last_abs = Some((m.lLastX, m.lLastY));
                    (d.0 / 32, d.1 / 32)
                } else {
                    (m.lLastX, m.lLastY)
                };
                if dx != 0 || dy != 0 {
                    self.send(Event::MouseMove(dx, dy));
                }
                const BUTTONS: [(u32, u32, InputId); 5] = [
                    (
                        RI_MOUSE_LEFT_BUTTON_DOWN,
                        RI_MOUSE_LEFT_BUTTON_UP,
                        keys::MOUSE_LEFT,
                    ),
                    (
                        RI_MOUSE_RIGHT_BUTTON_DOWN,
                        RI_MOUSE_RIGHT_BUTTON_UP,
                        keys::MOUSE_RIGHT,
                    ),
                    (
                        RI_MOUSE_MIDDLE_BUTTON_DOWN,
                        RI_MOUSE_MIDDLE_BUTTON_UP,
                        keys::MOUSE_MIDDLE,
                    ),
                    (RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, keys::MOUSE_4),
                    (RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP, keys::MOUSE_5),
                ];
                for (down_flag, up_flag, id) in BUTTONS {
                    if flags & down_flag != 0 {
                        if let a @ (After::Detach | After::Destroy) = self.key_event(id, true) {
                            return a;
                        }
                    }
                    if flags & up_flag != 0 {
                        self.key_event(id, false);
                    }
                }
                if flags & RI_MOUSE_WHEEL != 0 && data != 0 {
                    let id = if data > 0 {
                        keys::WHEEL_UP
                    } else {
                        keys::WHEEL_DOWN
                    };
                    return self.wheel(id);
                }
                if flags & RI_MOUSE_HWHEEL != 0 && data != 0 {
                    let id = if data > 0 {
                        keys::WHEEL_RIGHT
                    } else {
                        keys::WHEEL_LEFT
                    };
                    return self.wheel(id);
                }
                After::Nothing
            }
            _ => After::Nothing,
        }
    }

    fn wheel(&mut self, id: InputId) -> After {
        if id == self.capture_key || id == self.quit_key {
            return self.key_event(id, true);
        }
        self.last_input = keys::name(id);
        self.send(Event::Wheel(id));
        self.repaint();
        After::Nothing
    }

    /// Handles a message for the host's window; `None` passes it on.
    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> (Option<LRESULT>, After) {
        let r = match msg {
            WM_INPUT => {
                let after = self.raw_input(lparam);
                return (None, after); // DefWindowProc must still clean up
            }
            WM_APP_SETCAPTURE => {
                register_raw_input(self.hwnd());
                let on = wparam != 0 && unsafe { GetForegroundWindow() } == self.hwnd();
                self.set_capture(on);
                Some(0)
            }
            WM_ACTIVATE => {
                if (wparam & 0xffff) as u32 == WA_INACTIVE {
                    if self.capturing {
                        log::info!("window went to the background: releasing keyboard and mouse");
                    }
                    self.set_capture(false);
                } else {
                    register_raw_input(self.hwnd());
                }
                None
            }
            WM_KILLFOCUS => {
                if self.capturing {
                    log::info!("window lost the keyboard focus: releasing keyboard and mouse");
                }
                self.set_capture(false);
                None
            }
            WM_SIZE | WM_MOVE => {
                if self.capturing {
                    self.clip();
                }
                self.repaint();
                None
            }
            // keep Alt / F10 from opening the system menu while playing
            WM_SYSKEYDOWN | WM_SYSKEYUP | WM_SYSCHAR if self.capturing => Some(0),
            WM_SYSCOMMAND if self.capturing && (wparam & 0xfff0) as u32 == SC_KEYMENU => Some(0),
            // the settings window must not react to keys and clicks meant for the PS5
            WM_KEYDOWN | WM_KEYUP | WM_CHAR | WM_DEADCHAR | WM_MOUSEMOVE | WM_LBUTTONDOWN
            | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONUP
            | WM_RBUTTONDBLCLK | WM_MBUTTONDOWN | WM_MBUTTONUP | WM_MBUTTONDBLCLK
            | WM_XBUTTONDOWN | WM_XBUTTONUP | WM_XBUTTONDBLCLK | WM_MOUSEWHEEL | WM_MOUSEHWHEEL
                if self.embedded && self.capturing =>
            {
                Some(0)
            }
            // own window only
            WM_PAINT if !self.embedded => {
                self.paint();
                Some(0)
            }
            WM_APP_STATUS if !self.embedded => {
                self.repaint();
                Some(0)
            }
            WM_LBUTTONDOWN if !self.embedded && !self.capturing => {
                self.set_capture(true);
                Some(0)
            }
            WM_CLOSE if !self.embedded => {
                self.set_capture(false);
                return (Some(0), After::Destroy); // outside the lock: DestroyWindow sends messages
            }
            _ => None,
        };
        (r, After::Nothing)
    }

    fn paint(&self) {
        // colours are 0x00BBGGRR
        const GREEN: u32 = 0x004aa316;
        const AMBER: u32 = 0x000b9ef5;
        const SLATE: u32 = 0x00554b47;
        unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(self.hwnd(), &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(self.hwnd(), &mut rc);
            FillRect(hdc, &rc, GetSysColorBrush(COLOR_WINDOW));
            SetBkMode(hdc, TRANSPARENT as i32);

            let status = self.status.lock().map(|s| s.clone()).unwrap_or_default();
            let streaming = status.starts_with("Connected");
            let capture = keys::name(self.capture_key);
            let quit = keys::name(self.quit_key);
            let (band, headline, sub) = match (self.capturing, streaming) {
                (true, true) => (
                    GREEN,
                    "Keyboard and mouse control your PS5".to_string(),
                    format!("{capture} gives you your mouse back  ·  {quit} disconnects"),
                ),
                (true, false) => (
                    AMBER,
                    "Waiting for the PS5…".to_string(),
                    format!("Input is captured and goes to the PS5 once connected  ·  {capture} releases"),
                ),
                (false, _) => (
                    SLATE,
                    "Mouse released: the PS5 gets no input".to_string(),
                    format!("Press {capture} or click here to control the PS5 again  ·  {quit} disconnects"),
                ),
            };

            let band_rc = RECT {
                left: 0,
                top: 0,
                right: rc.right,
                bottom: 70,
            };
            let brush = CreateSolidBrush(band);
            FillRect(hdc, &band_rc, brush);
            DeleteObject(brush as _);
            SetTextColor(hdc, 0x00ffffff);
            draw(
                hdc,
                &headline,
                RECT {
                    left: 14,
                    top: 10,
                    right: rc.right - 14,
                    bottom: 40,
                },
                22,
                true,
            );
            draw(
                hdc,
                &sub,
                RECT {
                    left: 14,
                    top: 42,
                    right: rc.right - 14,
                    bottom: 68,
                },
                15,
                false,
            );

            SetTextColor(hdc, 0x00202020);
            let profile = self
                .active_profile
                .lock()
                .map(|p| p.clone())
                .unwrap_or_default();
            draw(
                hdc,
                &format!("Profile: {profile}"),
                RECT {
                    left: 14,
                    top: 82,
                    right: rc.right - 14,
                    bottom: 110,
                },
                18,
                true,
            );
            let mut body = status;
            if !self.last_input.is_empty() && self.capturing {
                body.push_str(&format!("\nLast key: {}", self.last_input));
            }
            draw(
                hdc,
                &body,
                RECT {
                    left: 14,
                    top: 114,
                    right: rc.right - 14,
                    bottom: rc.bottom - 6,
                },
                15,
                false,
            );
            EndPaint(self.hwnd(), &ps);
        }
    }
}

/// Draws wrapped text with Segoe UI at `px` pixels.
unsafe fn draw(hdc: HDC, text: &str, mut rc: RECT, px: i32, bold: bool) {
    let face = wide("Segoe UI");
    let font = CreateFontW(
        -px,
        0,
        0,
        0,
        if bold { 700 } else { 400 },
        0,
        0,
        0,
        1,
        0,
        0,
        5,
        0,
        face.as_ptr(),
    );
    let old = SelectObject(hdc, font as _);
    let mut w: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(
        hdc,
        w.as_mut_ptr(),
        w.len() as i32,
        &mut rc,
        DT_LEFT | DT_WORDBREAK | DT_NOPREFIX,
    );
    SelectObject(hdc, old);
    DeleteObject(font as _);
}

/// Runs a message through the host of `hwnd`, if there is one.
fn dispatch(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let (handled, after) = {
        // try_lock: never block the window thread (or deadlock on re-entry)
        let Ok(mut guard) = HOST.try_lock() else {
            return None;
        };
        match guard.as_mut() {
            Some(h) if h.hwnd == hwnd as usize => h.handle(msg, wparam, lparam),
            _ => return None,
        }
    };
    match after {
        After::Nothing => {}
        After::Detach => detach(hwnd as usize),
        After::Destroy => unsafe {
            DestroyWindow(hwnd);
        },
    }
    handled
}

/// Removes the host of `hwnd`: input stops, the pump thread ends, the cursor comes back.
fn detach(hwnd: usize) {
    let host = {
        let mut guard = HOST.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().map(|h| h.hwnd != hwnd).unwrap_or(true) {
            return;
        }
        guard.take()
    };
    if let Some(h) = host {
        if h.capturing {
            CAPTURING.store(false, Ordering::Relaxed);
            unsafe {
                ClipCursor(std::ptr::null());
                // the cursor's show count belongs to the window's thread
                if on_window_thread(hwnd) {
                    while ShowCursor(1) < 0 {}
                } else {
                    PostMessageW(hwnd as HWND, WM_APP_UNCAPTURE, 0, 0);
                }
            }
            log::info!("input capture off");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Settings window

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_APP_UNCAPTURE {
        while ShowCursor(1) < 0 {}
        return 0;
    }
    match dispatch(hwnd, msg, wparam, lparam) {
        Some(r) => r,
        None => DefSubclassProc(hwnd, msg, wparam, lparam),
    }
}

/// Lets sessions capture input in the settings window. Call once, on the window's thread.
pub fn install_in_window(hwnd: isize) -> Result<()> {
    if unsafe { SetWindowSubclass(hwnd as HWND, Some(subclass_proc), 0xC41B, 0) } == 0 {
        bail!(
            "SetWindowSubclass failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Starts capturing in the settings window (prepared with `install_in_window`).
pub fn attach_to_window(hwnd: isize, tx: Sender<Msg>, opts: HostOptions) -> Result<WindowHandle> {
    let capture_on_start = opts.capture_on_start;
    {
        let mut guard = HOST.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            bail!("another session is still capturing input");
        }
        *guard = Some(Host::new(hwnd as usize, true, tx, opts));
    }
    let handle = WindowHandle {
        hwnd: hwnd as usize,
        embedded: true,
    };
    // on the window's thread: claims Raw Input, and captures if asked to
    handle.set_capture(capture_on_start);
    Ok(handle)
}

// ---------------------------------------------------------------------------------------------
// Own window (command line)

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        PostQuitMessage(0);
        return 0;
    }
    match dispatch(hwnd, msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Creates a small capture window and runs its message loop until it is closed.
/// `on_created` receives a handle other threads can use to refresh or close the window.
pub fn run_window(
    tx: Sender<Msg>,
    opts: HostOptions,
    on_created: impl FnOnce(WindowHandle),
) -> Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let class_name = wide("ChiakiControllerWindow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        // a second session in the same process finds the class already registered
        if RegisterClassExW(&wc) == 0 && windows_sys::Win32::Foundation::GetLastError() != 1410 {
            bail!(
                "RegisterClassExW failed: {}",
                std::io::Error::last_os_error()
            );
        }
        let title = wide("chiaki-controller: controlling the PS5");
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            600,
            250,
            std::ptr::null_mut(),
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
        if !register_raw_input(hwnd) {
            DestroyWindow(hwnd);
            bail!(
                "RegisterRawInputDevices failed: {}",
                std::io::Error::last_os_error()
            );
        }
        let capture_on_start = opts.capture_on_start;
        {
            let mut guard = HOST.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_some() {
                drop(guard);
                DestroyWindow(hwnd);
                bail!("another session is still capturing input");
            }
            *guard = Some(Host::new(hwnd as usize, false, tx, opts));
        }
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        let handle = WindowHandle {
            hwnd: hwnd as usize,
            embedded: false,
        };
        if capture_on_start {
            handle.set_capture(true);
        }
        on_created(handle);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // the window is gone; drop the host (this ends the pump thread)
        let mut guard = HOST.lock().unwrap_or_else(|e| e.into_inner());
        if guard
            .as_ref()
            .map(|h| h.hwnd == hwnd as usize)
            .unwrap_or(false)
        {
            guard.take();
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Pump: mapper, ticks, controller state

/// Swallows the Windows keys while capturing so they don't open the Start menu.
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && CAPTURING.load(Ordering::Relaxed) {
        let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
        if kb.vkCode == VK_LWIN as u32 || kb.vkCode == VK_RWIN as u32 {
            return 1;
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

/// Runs the low level keyboard hook on its own thread (hooks need a message loop that
/// never stalls). Returns the thread id to end it with WM_QUIT.
fn start_keyboard_hook() -> Option<u32> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("keyboard-hook".into())
        .spawn(move || unsafe {
            let mut msg: MSG = std::mem::zeroed();
            PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE); // create the queue
            let hook = SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_hook),
                GetModuleHandleW(std::ptr::null()),
                0,
            );
            let _ = tx.send(GetCurrentThreadId());
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {}
            if !hook.is_null() {
                UnhookWindowsHookEx(hook);
            }
        })
        .ok()?;
    rx.recv_timeout(Duration::from_secs(2)).ok()
}

/// Applies the host's events to the mapper and sends the controller state, until the host
/// goes away. `on_profile` is called when the active profile changes.
pub fn pump(
    mut mapper: Mapper,
    link: Arc<ControllerLink>,
    rx: Receiver<Msg>,
    active_profile: Arc<Mutex<String>>,
    tick: Duration,
    on_profile: impl Fn(),
) {
    let hook = start_keyboard_hook();
    unsafe { timeBeginPeriod(1) };
    let apply = |mapper: &mut Mapper, m: Msg| match m {
        Msg::Input(ev, at) => mapper.handle(ev, at),
        Msg::Release => mapper.release_all(),
    };
    loop {
        let timeout = if mapper.needs_ticks() {
            tick
        } else {
            Duration::from_millis(100)
        };
        let mut dirty = false;
        match rx.recv_timeout(timeout) {
            Ok(m) => {
                apply(&mut mapper, m);
                // take everything that is queued before sending one state
                while let Ok(m) = rx.try_recv() {
                    apply(&mut mapper, m);
                }
                dirty = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let changed = active_profile.lock().map(|mut p| {
            let now = mapper.profile_name();
            if *p != now {
                *p = now.to_string();
                true
            } else {
                false
            }
        });
        if changed.unwrap_or(false) {
            on_profile();
        }
        if dirty || mapper.needs_ticks() {
            link.set_state(&mapper.state(Instant::now()));
        }
    }
    unsafe { timeEndPeriod(1) };
    if let Some(tid) = hook {
        unsafe { PostThreadMessageW(tid, WM_QUIT, 0, 0) };
    }
    link.set_state(&crate::controller::ControllerState::default());
}
