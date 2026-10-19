use std::collections::HashMap;
use std::mem::size_of;
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, HRAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RIDEV_INPUTSINK, RIDEV_REMOVE, RIDI_DEVICENAME, RID_INPUT, RIM_TYPEMOUSE,
    RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG,
    RegisterClassW, TranslateMessage, WM_INPUT, WNDCLASSW,
};

const VENDOR_TAG: &str = "VID_046D";
const USAGE_PAGE_MOUSE: u16 = 0x01;
const USAGE_MOUSE: u16 = 0x02;

#[derive(Default)]
struct Shared {
    window: isize,
    target_pids: Vec<u16>,
    events: Option<Sender<()>>,
    names: HashMap<isize, Option<String>>,
    last_activity: Option<Instant>,
}

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();

fn shared() -> &'static Mutex<Shared> {
    SHARED.get_or_init(|| Mutex::new(Shared::default()))
}

fn lock_shared() -> MutexGuard<'static, Shared> {
    shared().lock().unwrap_or_else(|error| error.into_inner())
}

/// Receives one event when a movement report from the receiver currently in
/// use arrives while wake detection is registered.
pub struct RawInput {
    events: Receiver<()>,
}

impl RawInput {
    pub fn start() -> Self {
        let (event_tx, events) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        lock_shared().events = Some(event_tx);

        let spawned = thread::Builder::new()
            .name(String::from("raw-input"))
            .spawn(move || {
                let result = unsafe { create_window() };
                match &result {
                    Ok(()) => {
                        let _ = ready_tx.send(Ok(()));
                        unsafe { message_loop() };
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.clone()));
                    }
                }
            });
        if let Err(error) = spawned {
            gpx_tool::dlog!("raw input thread could not start: {error}");
        }

        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => gpx_tool::dlog!("raw input message window ready"),
            Ok(Err(error)) => gpx_tool::dlog!("raw input setup failed: {error}"),
            Err(_) => gpx_tool::dlog!("raw input setup timed out"),
        }

        Self { events }
    }

    /// Registers (or retargets) mouse input for the given device PIDs. The
    /// registration stays active so the worker can tell when the mouse was
    /// last moved, even while it is awake. Cheap: Windows raw input is passive.
    pub fn enable(&self, pids: &[u16]) {
        let pids: Vec<u16> = pids.iter().copied().filter(|pid| *pid != 0).collect();
        if pids.is_empty() {
            return;
        }
        while self.events.try_recv().is_ok() {}
        let mut state = lock_shared();
        let already = !state.target_pids.is_empty();
        if already && state.target_pids == pids {
            return;
        }
        if state.window == 0 {
            gpx_tool::dlog!("raw input enable skipped: no message window");
            return;
        }
        if !already {
            if let Err(error) = unsafe { set_registration(true, state.window as HWND) } {
                gpx_tool::dlog!("could not register raw input: {error}");
                return;
            }
        }
        state.names.clear();
        state.last_activity = None;
        state.target_pids = pids;
        gpx_tool::dlog!(
            "raw input activity tracking registered for PID(s) {:04X?}",
            state.target_pids
        );
    }

    /// Unregisters mouse input (device gone or app exiting).
    pub fn disable(&self) {
        let mut state = lock_shared();
        if state.target_pids.is_empty() {
            return;
        }
        state.target_pids.clear();
        state.names.clear();
        state.last_activity = None;
        let error = unsafe { set_registration(false, ptr::null_mut()) };
        match error {
            Ok(()) => gpx_tool::dlog!("raw input activity tracking unregistered"),
            Err(error) => gpx_tool::dlog!("could not unregister raw input: {error}"),
        }
    }

    /// True when a matching movement report arrived since the last call.
    pub fn activity(&self) -> bool {
        self.events.try_recv().is_ok()
    }

    /// Time of the newest matching raw-input report (any kind: movement or
    /// button), or None when nothing was seen since registration.
    pub fn last_activity(&self) -> Option<Instant> {
        lock_shared().last_activity
    }
}

unsafe fn create_window() -> Result<(), String> {
    let hinstance = unsafe { GetModuleHandleW(ptr::null()) };
    let class: Vec<u16> = "GPXToolRawInput\0".encode_utf16().collect();

    let mut class_info: WNDCLASSW = unsafe { std::mem::zeroed() };
    class_info.lpfnWndProc = Some(window_proc);
    class_info.hInstance = hinstance;
    class_info.lpszClassName = class.as_ptr();

    if unsafe { RegisterClassW(&class_info) } == 0 {
        return Err(format!(
            "RegisterClassW failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            ptr::null_mut(),
            hinstance,
            ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(format!(
            "CreateWindowExW failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    lock_shared().window = hwnd as isize;
    Ok(())
}

unsafe fn message_loop() {
    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_INPUT {
        unsafe { handle_raw_input(lparam as HRAWINPUT) };
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

unsafe fn set_registration(enabled: bool, hwnd: HWND) -> Result<(), String> {
    let device = RAWINPUTDEVICE {
        usUsagePage: USAGE_PAGE_MOUSE,
        usUsage: USAGE_MOUSE,
        dwFlags: if enabled { RIDEV_INPUTSINK } else { RIDEV_REMOVE },
        hwndTarget: if enabled { hwnd } else { ptr::null_mut() },
    };
    if unsafe { RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) } == 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

unsafe fn handle_raw_input(raw: HRAWINPUT) {
    let pids = {
        let state = lock_shared();
        if state.target_pids.is_empty() {
            return;
        }
        state.target_pids.clone()
    };

    let header_size = size_of::<RAWINPUTHEADER>() as u32;
    let mut size = 0u32;
    let queried =
        unsafe { GetRawInputData(raw, RID_INPUT, ptr::null_mut(), &mut size, header_size) };
    if queried == u32::MAX || size == 0 {
        return;
    }

    let words = (size as usize + 7) / 8;
    let mut buffer = vec![0u64; words];
    let mut copied = size;
    let result = unsafe {
        GetRawInputData(
            raw,
            RID_INPUT,
            buffer.as_mut_ptr().cast(),
            &mut copied,
            header_size,
        )
    };
    if result == u32::MAX || copied < header_size {
        return;
    }

    let header: RAWINPUTHEADER = unsafe { ptr::read_unaligned(buffer.as_ptr().cast()) };
    if header.dwType != RIM_TYPEMOUSE {
        return;
    }

    let Some(name) = device_name(header.hDevice) else {
        return;
    };
    if !device_matches(&name, &pids) {
        return;
    }

    let sender = {
        let mut state = lock_shared();
        if state.target_pids.is_empty() {
            return;
        }
        state.last_activity = Some(Instant::now());
        state.events.clone()
    };

    gpx_tool::dlog!("raw input activity from the mouse");
    if let Some(sender) = sender {
        let _ = sender.send(());
    }
}

fn device_name(device: HANDLE) -> Option<String> {
    let key = device as isize;
    if let Some(cached) = lock_shared().names.get(&key) {
        return cached.clone();
    }

    let name = unsafe {
        let mut count = 0u32;
        let queried = GetRawInputDeviceInfoW(
            device,
            RIDI_DEVICENAME,
            ptr::null_mut(),
            &mut count,
        );
        if queried == u32::MAX || count == 0 {
            None
        } else {
            let mut buffer = vec![0u16; count as usize + 1];
            let mut count = buffer.len() as u32;
            let result = GetRawInputDeviceInfoW(
                device,
                RIDI_DEVICENAME,
                buffer.as_mut_ptr().cast(),
                &mut count,
            );
            if result == u32::MAX {
                None
            } else {
                let length = (count as usize).min(buffer.len());
                let text = String::from_utf16_lossy(&buffer[..length]);
                Some(text.trim_end_matches('\0').to_owned())
            }
        }
    };

    lock_shared().names.insert(key, name.clone());
    gpx_tool::dlog!("raw input device {key:#x}: {name:?}");
    name
}

fn device_matches(name: &str, pids: &[u16]) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.contains(VENDOR_TAG)
        && pids
            .iter()
            .any(|pid| upper.contains(&format!("PID_{pid:04X}")))
}
