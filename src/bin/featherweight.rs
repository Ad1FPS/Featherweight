#![windows_subsystem = "windows"]

use std::collections::VecDeque;
use std::env;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use gpx_tool::autostart;
use gpx_tool::debuglog;
use gpx_tool::hidpp::{self, Battery, Device, Snapshot};
use gpx_tool::icon;
use gpx_tool::profile;
use gpx_tool::settings::{self, Settings, ThemeMode};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use rawinput::RawInput;

#[path = "gpx-gui/memory.rs"]
mod memory;

#[path = "gpx-gui/tray.rs"]
mod tray;

#[path = "gpx-gui/rawinput.rs"]
mod rawinput;

use tray::TrayHandle;

const OK_INTERVAL: Duration = Duration::from_secs(30);
const CUSTOM_INTERVAL: Duration = Duration::from_secs(10);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
const ASLEEP_SAFETY_INTERVAL: Duration = Duration::from_secs(600);
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(30);
const PATH_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const WAKE_ATTEMPTS: u32 = 3;
const WAKE_PAUSE: Duration = Duration::from_secs(1);
const HID_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
const WAKE_PING_TIMEOUT: Duration = Duration::from_millis(300);
const IDLE_SLICE: Duration = Duration::from_millis(100);

const PRESETS: [u16; 4] = [400, 800, 1600, 3200];
const SLEEP_PRESETS: [u16; 7] = [30, 60, 120, 300, 600, 900, 1800];
const LABEL_WIDTH: f32 = 88.0;
const CONTROL_WIDTH: f32 = 208.0;

const WINDOW_WIDTH: f32 = 360.0;
const WINDOW_INITIAL_HEIGHT: f32 = 460.0;
const WINDOW_MIN_HEIGHT: f32 = 120.0;
const WINDOW_MARGIN: f32 = 16.0;
const CARD_MARGIN: f32 = 12.0;
const BATTERY_LOW_PERCENT: u8 = 15;
const BATTERY_RED: egui::Color32 = egui::Color32::from_rgb(0xE5, 0x48, 0x4D);
const ACCENT_PRESETS: [[u8; 3]; 8] = [
    [0x3D, 0xBE, 0x6B],
    [0x3B, 0x82, 0xF6],
    [0x8B, 0x5C, 0xF6],
    [0xEC, 0x48, 0x99],
    [0xF9, 0x73, 0x16],
    [0x14, 0xB8, 0xA6],
    [0xEA, 0xB3, 0x08],
    [0x9C, 0xA3, 0xAF],
];

#[derive(Clone, Copy)]
struct Palette {
    dark: bool,
    bg: egui::Color32,
    card_bg: egui::Color32,
    card_border: egui::Color32,
    text: egui::Color32,
    muted: egui::Color32,
    control_bg: egui::Color32,
    control_border: egui::Color32,
    warning: egui::Color32,
    error: egui::Color32,
}

impl Palette {
    const DARK: Self = Self {
        dark: true,
        bg: egui::Color32::from_rgb(0x12, 0x12, 0x12),
        card_bg: egui::Color32::from_rgb(0x1C, 0x1C, 0x1C),
        card_border: egui::Color32::TRANSPARENT,
        text: egui::Color32::from_rgb(0xE6, 0xE6, 0xE6),
        muted: egui::Color32::from_rgb(0x9A, 0x9A, 0x9A),
        control_bg: egui::Color32::from_rgb(0x2D, 0x2D, 0x2D),
        control_border: egui::Color32::from_rgb(0x55, 0x55, 0x55),
        warning: egui::Color32::from_rgb(0xE1, 0xB9, 0x3C),
        error: egui::Color32::from_rgb(0xEB, 0x6E, 0x6E),
    };
    const LIGHT: Self = Self {
        dark: false,
        bg: egui::Color32::from_rgb(0xF3, 0xF3, 0xF3),
        card_bg: egui::Color32::from_rgb(0xFF, 0xFF, 0xFF),
        card_border: egui::Color32::from_rgb(0xE0, 0xE0, 0xE0),
        text: egui::Color32::from_rgb(0x1A, 0x1A, 0x1A),
        muted: egui::Color32::from_rgb(0x5F, 0x5F, 0x5F),
        control_bg: egui::Color32::from_rgb(0xEA, 0xEA, 0xEA),
        control_border: egui::Color32::from_rgb(0xC8, 0xC8, 0xC8),
        warning: egui::Color32::from_rgb(0xB4, 0x53, 0x09),
        error: egui::Color32::from_rgb(0xC6, 0x28, 0x28),
    };
}

fn rgb(color: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(color[0], color[1], color[2])
}

fn contrast_text(color: egui::Color32) -> egui::Color32 {
    fn linear(channel: u8) -> f32 {
        let value = f32::from(channel) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    let luminance = 0.2126 * linear(color.r())
        + 0.7152 * linear(color.g())
        + 0.0722 * linear(color.b());
    if luminance > 0.179 {
        egui::Color32::BLACK
    } else {
        egui::Color32::WHITE
    }
}

fn resolve_dark(theme: ThemeMode) -> bool {
    match theme {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => !autostart::apps_use_light_theme(),
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = env::args().skip(1).collect();
    let minimized = args.iter().any(|arg| arg == "--minimized");
    let debug_log = args.iter().any(|arg| arg == "--debug-log");

    let _single_instance = match acquire_single_instance() {
        InstanceLock::Acquired(handle) => handle,
        InstanceLock::AlreadyRunning => {
            show_already_running();
            return Ok(());
        }
        InstanceLock::Failed => {
            record_line("could not create the single-instance mutex; continuing");
            std::ptr::null_mut()
        }
    };

    debuglog::init(debug_log);
    if env::var("GPX_GUI_SELFTEST")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        run_selftest();
        return Ok(());
    }

    const WINDOW_ICON_SIZE: u32 = 256;
    let window_icon = egui::IconData {
        rgba: icon::render_icon(WINDOW_ICON_SIZE, Some(100), false),
        width: WINDOW_ICON_SIZE,
        height: WINDOW_ICON_SIZE,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([WINDOW_WIDTH, WINDOW_INITIAL_HEIGHT])
            .with_min_inner_size([WINDOW_WIDTH, WINDOW_MIN_HEIGHT])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_visible(!minimized)
            .with_icon(window_icon),
        ..Default::default()
    };

    eframe::run_native(
        "Featherweight",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, minimized, debug_log)))),
    )
}

enum InstanceLock {
    Acquired(windows_sys::Win32::Foundation::HANDLE),
    AlreadyRunning,
    Failed,
}

fn acquire_single_instance() -> InstanceLock {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\Featherweight_SingleInstance\0".encode_utf16().collect();
    let handle: HANDLE = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return InstanceLock::Failed;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(handle);
        }
        return InstanceLock::AlreadyRunning;
    }
    InstanceLock::Acquired(handle)
}

fn show_already_running() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };

    let text: Vec<u16> = "Featherweight is already running (check the system tray).\0"
        .encode_utf16()
        .collect();
    let caption: Vec<u16> = "Featherweight\0".encode_utf16().collect();

    let timeout_ms = env::var("GPX_TOOL_MSG_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10_000);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(timeout_ms));
        use windows_sys::Win32::System::Threading::ExitProcess;
        unsafe {
            ExitProcess(0);
        }
    });

    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }
}

fn icon_font_family() -> egui::FontFamily {
    egui::FontFamily::Name(Arc::from("icons"))
}

fn install_ui_font(ctx: &egui::Context) -> bool {
    let windows_dir = env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let fonts = windows_dir.join("Fonts");
    let mut definitions = egui::FontDefinitions::default();
    let mut changed = false;

    for name in ["SegUIVar.ttf", "segoeui.ttf"] {
        let Ok(bytes) = std::fs::read(fonts.join(name)) else {
            continue;
        };
        definitions.font_data.insert(
            String::from("windows-ui"),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        definitions
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, String::from("windows-ui"));
        changed = true;
        break;
    }

    let mut icons = false;
    for name in ["SegoeIcons.ttf", "segmdl2.ttf"] {
        let Ok(bytes) = std::fs::read(fonts.join(name)) else {
            continue;
        };
        definitions.font_data.insert(
            String::from("windows-icons"),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        definitions
            .families
            .insert(icon_font_family(), vec![String::from("windows-icons")]);
        changed = true;
        icons = true;
        break;
    }

    if changed {
        ctx.set_fonts(definitions);
    }
    icons
}

fn apply_appearance(ctx: &egui::Context, dark: bool, accent: egui::Color32) {
    let on_accent = contrast_text(accent);
    ctx.set_theme(if dark {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    });

    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let palette = Palette::DARK;
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = palette.bg;
        style.visuals.window_fill = palette.bg;
        style.visuals.faint_bg_color = palette.card_bg;
        style.visuals.extreme_bg_color = egui::Color32::from_rgb(0x0E, 0x0E, 0x0E);
        style.visuals.selection.bg_fill = accent;
        style.visuals.selection.stroke = egui::Stroke::new(1.0, on_accent);
        style.visuals.slider_trailing_fill = true;
        style.visuals.widgets.inactive.weak_bg_fill = palette.control_bg;
        style.visuals.widgets.hovered.weak_bg_fill =
            egui::Color32::from_rgb(0x38, 0x38, 0x38);
        style.visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(0x44, 0x44, 0x44);
        style.visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0, palette.text);
        style.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
        style.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        apply_text_styles(style);
    });

    ctx.style_mut_of(egui::Theme::Light, |style| {
        let palette = Palette::LIGHT;
        style.visuals = egui::Visuals::light();
        style.visuals.panel_fill = palette.bg;
        style.visuals.window_fill = palette.bg;
        style.visuals.faint_bg_color = palette.card_bg;
        style.visuals.extreme_bg_color = palette.control_bg;
        style.visuals.selection.bg_fill = accent;
        style.visuals.selection.stroke = egui::Stroke::new(1.0, on_accent);
        style.visuals.slider_trailing_fill = true;
        style.visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0, palette.text);
        style.visuals.widgets.inactive.weak_bg_fill = palette.control_bg;
        style.visuals.widgets.hovered.weak_bg_fill =
            egui::Color32::from_rgb(0xE3, 0xE3, 0xE3);
        style.visuals.widgets.active.weak_bg_fill =
            egui::Color32::from_rgb(0xDC, 0xDC, 0xDC);
        style.visuals.widgets.inactive.bg_fill = palette.control_bg;
        style.visuals.widgets.hovered.bg_fill = palette.control_bg;
        style.visuals.widgets.active.bg_fill = palette.control_bg;
        style.visuals.widgets.inactive.bg_stroke =
            egui::Stroke::new(1.0, palette.control_border);
        style.visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0, palette.text);
        style.visuals.widgets.hovered.fg_stroke =
            egui::Stroke::new(1.5, palette.text);
        style.visuals.widgets.active.fg_stroke =
            egui::Stroke::new(1.5, palette.text);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        apply_text_styles(style);
    });
}

fn apply_text_styles(style: &mut egui::Style) {
    use egui::{FontFamily, FontId, TextStyle};

    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(15.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(15.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(15.0, FontFamily::Monospace),
    );
}

fn set_title_bar(handle: &impl HasWindowHandle, dark: bool) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;

    let Ok(handle) = handle.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(window) = handle.as_raw() else {
        return;
    };
    let hwnd = window.hwnd.get() as *mut c_void;
    let enabled: i32 = if dark { 1 } else { 0 };
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            20,
            (&enabled as *const i32).cast::<c_void>(),
            std::mem::size_of::<i32>() as u32,
        );
    }
}

struct DebugLog {
    file: File,
    start: Instant,
}

impl DebugLog {
    fn create() -> Result<Self, String> {
        let path = debuglog::path();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("could not open {}: {e}", path.display()))?;
        Ok(Self {
            file,
            start: Instant::now(),
        })
    }

    fn tooltip(&mut self, text: &str, hidden: bool) {
        let seconds = self.start.elapsed().as_secs_f64();
        let _ = writeln!(self.file, "{seconds:.3} hidden={hidden} tooltip: {text}");
        let _ = self.file.flush();
    }
}

fn record_line(line: &str) {
    eprintln!("{line}");
    debuglog::record_always(line);
}

enum Command {
    Stop,
    Refresh,
    ApplyDpi(u16),
    ApplyRate(u16),
    ApplyLod(u8),
    Onboard,
    SaveToMouse { dpi: u16, lod: u8, rate: Option<u16> },
    SaveSleep {
        ps_timeout: Option<u16>,
        po_timeout: Option<u16>,
    },
    Appearance(ThemeMode, [u8; 3]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unavailable {
    Receiver,
    Mouse,
}

#[derive(Clone, Debug)]
struct StateError {
    kind: Unavailable,
    message: String,
}

impl StateError {
    fn from_error(error: &hidpp::Error) -> Self {
        let kind = if error.is_receiver_missing() {
            Unavailable::Receiver
        } else {
            Unavailable::Mouse
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

enum Event {
    State(Result<Snapshot, StateError>),
    Sleeping,
    Idle,
    DpiApplied(u16),
    RateApplied(u16),
    LodApplied(u8),
    Onboarded,
    SavedToMouse(u16, u8, Option<u16>),
    SleepTimers(profile::SleepTimers),
    SleepSaved(profile::SleepTimers),
    Targets(Settings, bool),
    Error(String),
    Reapplied(String),
}

/// Bounded hand-off from the worker to the UI. The worker keeps producing
/// events while the window is hidden, but eframe consumes none of them, so
/// repeated state/error/reapply events are coalesced instead of piling up.
#[derive(Clone)]
struct EventQueue {
    queue: Arc<Mutex<VecDeque<Event>>>,
}

impl EventQueue {
    fn new() -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn send(&self, event: Event) -> Result<(), ()> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        match &event {
            Event::State(_) => {
                queue.retain(|item| {
                    !matches!(
                        item,
                        Event::State(_) | Event::Sleeping | Event::Idle | Event::Error(_)
                    )
                });
            }
            Event::Sleeping => {
                queue.retain(|item| !matches!(item, Event::Sleeping | Event::Idle));
            }
            Event::Idle => {
                queue.retain(|item| !matches!(item, Event::Idle | Event::Sleeping));
            }
            Event::Error(_) => {
                queue.retain(|item| !matches!(item, Event::Error(_)));
            }
            Event::Reapplied(_) => {
                queue.retain(|item| !matches!(item, Event::Reapplied(_)));
            }
            Event::Targets(..) => {
                queue.retain(|item| !matches!(item, Event::Targets(..)));
            }
            Event::SleepTimers(_) => {
                queue.retain(|item| !matches!(item, Event::SleepTimers(_)));
            }
            _ => {}
        }
        queue.push_back(event);
        Ok(())
    }

    fn try_recv(&self) -> Result<Event, TryRecvError> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.pop_front().ok_or(TryRecvError::Empty)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Main,
    Settings,
}

fn tooltip_text(
    asleep: bool,
    idle: bool,
    state: Option<&Result<Snapshot, StateError>>,
) -> String {
    if asleep {
        return match state {
            Some(Ok(snapshot)) => {
                format!("Featherweight | asleep (last {}%)", snapshot.battery.percent)
            }
            _ => String::from("Featherweight | asleep"),
        };
    }
    if idle {
        return match state {
            Some(Ok(snapshot)) => {
                format!("Featherweight | idle (last {}%)", snapshot.battery.percent)
            }
            _ => String::from("Featherweight | idle"),
        };
    }
    match state {
        Some(Ok(snapshot)) => {
            let rate = match &snapshot.rate {
                Some(rate) => format!("{} Hz", rate.current_hz),
                None => String::from("rate --"),
            };
            let battery = if snapshot.battery.status == 0
                && snapshot.battery.external_power == 0
            {
                format!("{}%", snapshot.battery.percent)
            } else {
                format!(
                    "{}% ({})",
                    snapshot.battery.percent,
                    snapshot.battery.status_text()
                )
            };
            format!(
                "Featherweight | {} | {} DPI | {}",
                battery, snapshot.dpi.x, rate
            )
        }
        Some(Err(error)) => match error.kind {
            Unavailable::Receiver => "Featherweight | Receiver not found".to_owned(),
            Unavailable::Mouse => "Featherweight | Mouse not found".to_owned(),
        },
        None => "Featherweight | reading...".to_owned(),
    }
}

struct App {
    commands: Sender<Command>,
    events: EventQueue,
    ui_ctx: egui::Context,
    state: Option<Result<Snapshot, StateError>>,
    updated: Option<String>,
    settings: Settings,
    controlling: bool,
    asleep: bool,
    idle: bool,
    slider_dpi: u16,
    slider_initialized: bool,
    dpi_text: String,
    dpi_field_active: bool,
    pending_dpi: Option<u16>,
    pending_rate: Option<u16>,
    pending_lod: Option<u8>,
    confirm_save: bool,
    saving: bool,
    sleep_timers: Option<profile::SleepTimers>,
    sleep_selection: Option<profile::SleepTimers>,
    confirm_sleep: bool,
    sleep_saving: bool,
    saved_at: Option<Instant>,
    error_line: Option<String>,
    last_reapplied: Option<String>,
    stop: Arc<AtomicBool>,
    host_engaged: Arc<AtomicBool>,
    hid_lock: Arc<Mutex<()>>,
    tray: Option<TrayHandle>,
    quitting: Arc<AtomicBool>,
    window_visible: Arc<AtomicBool>,
    reset_to_main: Arc<AtomicBool>,
    debug: Option<DebugLog>,
    start_hidden: bool,
    hide_at: Instant,
    autostart: bool,
    last_fit_height: f32,
    dark: bool,
    accent: egui::Color32,
    on_accent: egui::Color32,
    battery_override: Option<u8>,
    title_bar_dark: Option<bool>,
    last_theme_check: Instant,
    view: View,
    icon_font: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, minimized: bool, debug_log: bool) -> Self {
        let icon_font = install_ui_font(&cc.egui_ctx);

        autostart::migrate();
        autostart::refresh_if_moved();
        let autostart_enabled = autostart::is_enabled();

        let settings = settings::load();
        let dark = resolve_dark(settings.theme);
        let accent = rgb(settings.accent);
        let on_accent = contrast_text(accent);
        apply_appearance(&cc.egui_ctx, dark, accent);
        set_title_bar(cc, dark);

        let controlling = settings.has_custom();
        let slider_dpi = settings.custom_dpi.unwrap_or(1600);

        let debug = if debug_log {
            match DebugLog::create() {
                Ok(log) => Some(log),
                Err(error) => {
                    record_line(&error);
                    None
                }
            }
        } else {
            None
        };

        let battery_override = if debuglog::enabled() {
            env::var("GPX_DEBUG_BATTERY")
                .ok()
                .and_then(|value| value.parse::<u8>().ok())
                .filter(|value| *value <= 100)
        } else {
            None
        };
        if let Some(percent) = battery_override {
            gpx_tool::dlog!("debug battery override: {percent}%");
        }

        let events = EventQueue::new();
        let event_tx = events.clone();
        let (commands, command_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let host_engaged = Arc::new(AtomicBool::new(false));
        let hid_lock = Arc::new(Mutex::new(()));
        let quitting = Arc::new(AtomicBool::new(false));
        let window_visible = Arc::new(AtomicBool::new(!minimized));
        let reset_to_main = Arc::new(AtomicBool::new(false));

        let tray = match tray::create(
            cc.egui_ctx.clone(),
            commands.clone(),
            Arc::clone(&window_visible),
            Arc::clone(&quitting),
            Arc::clone(&reset_to_main),
        ) {
            Ok(tray) => Some(tray),
            Err(error) => {
                record_line(&format!("tray icon creation failed: {error}"));
                None
            }
        };
        let start_hidden = minimized && tray.is_some();
        if minimized && tray.is_none() {
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }

        let raw = RawInput::start();
        let ctx = cc.egui_ctx.clone();
        let thread_stop = Arc::clone(&stop);
        let thread_host = Arc::clone(&host_engaged);
        let thread_lock = Arc::clone(&hid_lock);
        let thread_tray = tray.clone();
        let thread_visible = Arc::clone(&window_visible);
        thread::spawn(move || {
            poll_loop(
                event_tx,
                command_rx,
                ctx,
                thread_stop,
                thread_host,
                thread_lock,
                settings,
                controlling,
                true,
                raw,
                thread_tray,
                thread_visible,
            )
        });

        let mut app = Self {
            commands,
            events,
            ui_ctx: cc.egui_ctx.clone(),
            state: None,
            updated: None,
            settings,
            controlling,
            asleep: false,
            idle: false,
            slider_dpi,
            slider_initialized: settings.custom_dpi.is_some(),
            dpi_text: slider_dpi.to_string(),
            dpi_field_active: false,
            pending_dpi: None,
            pending_rate: None,
            pending_lod: None,
            confirm_save: false,
            saving: false,
            sleep_timers: None,
            sleep_selection: None,
            confirm_sleep: false,
            sleep_saving: false,
            saved_at: None,
            error_line: None,
            last_reapplied: None,
            stop,
            host_engaged,
            hid_lock,
            tray,
            quitting,
            window_visible,
            reset_to_main,
            debug,
            start_hidden,
            hide_at: Instant::now() + Duration::from_millis(400),
            autostart: autostart_enabled,
            last_fit_height: WINDOW_INITIAL_HEIGHT,
            dark,
            accent,
            on_accent,
            battery_override,
            title_bar_dark: Some(dark),
            last_theme_check: Instant::now(),
            view: View::Main,
            icon_font,
        };
        app.update_tooltip();
        app
    }

    fn palette(&self) -> Palette {
        if self.dark {
            Palette::DARK
        } else {
            Palette::LIGHT
        }
    }

    fn apply_appearance_now(&mut self) {
        self.on_accent = contrast_text(self.accent);
        apply_appearance(&self.ui_ctx, self.dark, self.accent);
    }

    fn set_theme_mode(&mut self, theme: ThemeMode) {
        if self.settings.theme == theme {
            return;
        }
        self.settings.theme = theme;
        self.dark = resolve_dark(theme);
        self.apply_appearance_now();
        let _ = self.commands.send(Command::Appearance(theme, self.settings.accent));
        self.ui_ctx.request_repaint();
    }

    fn set_accent(&mut self, accent: [u8; 3]) {
        if self.settings.accent == accent {
            return;
        }
        self.settings.accent = accent;
        self.accent = rgb(accent);
        self.apply_appearance_now();
        let _ = self
            .commands
            .send(Command::Appearance(self.settings.theme, accent));
        self.ui_ctx.request_repaint();
    }

    fn refresh_system_theme(&mut self) {
        if self.settings.theme != ThemeMode::System {
            self.last_theme_check = Instant::now();
            return;
        }
        if self.last_theme_check.elapsed() < Duration::from_secs(30) {
            return;
        }
        self.last_theme_check = Instant::now();
        let dark = resolve_dark(ThemeMode::System);
        if dark != self.dark {
            self.dark = dark;
            self.apply_appearance_now();
            self.ui_ctx.request_repaint();
        }
    }

    fn handle_events(&mut self) -> bool {
        let mut handled = false;
        while let Ok(event) = self.events.try_recv() {
            handled = true;
            match event {
                Event::State(Ok(snapshot)) => {
                    self.asleep = false;
                    self.idle = false;
                    if !self.slider_initialized {
                        self.slider_dpi = if self.controlling {
                            self.settings.custom_dpi.unwrap_or(snapshot.dpi.x)
                        } else {
                            snapshot.dpi.x
                        };
                        self.dpi_text = self.slider_dpi.to_string();
                        self.slider_initialized = true;
                    } else if !self.dpi_field_active && self.pending_dpi.is_none() {
                        self.slider_dpi = if self.controlling {
                            self.settings.custom_dpi.unwrap_or(snapshot.dpi.x)
                        } else {
                            snapshot.dpi.x
                        };
                        self.dpi_text = self.slider_dpi.to_string();
                    }
                    self.updated = Some(local_time_string());
                    gpx_tool::dlog!(
                        "ui consumed state: dpi={} lod={} mode={} rate={:?}",
                        snapshot.dpi.x,
                        snapshot.dpi.lod,
                        snapshot.mode,
                        snapshot.rate.as_ref().map(|rate| rate.current_hz)
                    );
                    self.state = Some(Ok(snapshot));
                }
                Event::State(Err(message)) => {
                    self.asleep = false;
                    self.idle = false;
                    self.state = Some(Err(message));
                }
                Event::Sleeping => {
                    gpx_tool::dlog!("ui consumed Sleeping");
                    self.asleep = true;
                    self.idle = false;
                }
                Event::Idle => {
                    gpx_tool::dlog!("ui consumed Idle");
                    self.idle = true;
                    self.asleep = false;
                }
                Event::DpiApplied(value) => {
                    gpx_tool::dlog!("ui consumed DpiApplied {value}");
                    self.settings.custom_dpi = Some(value);
                    self.controlling = true;
                    self.slider_dpi = value;
                    self.dpi_text = value.to_string();
                    self.slider_initialized = true;
                    if self.pending_dpi == Some(value) {
                        self.pending_dpi = None;
                    }
                    self.error_line = None;
                }
                Event::RateApplied(hz) => {
                    gpx_tool::dlog!("ui consumed RateApplied {hz}");
                    self.settings.custom_rate = Some(hz);
                    self.controlling = true;
                    if self.pending_rate == Some(hz) {
                        self.pending_rate = None;
                    }
                    self.error_line = None;
                }
                Event::LodApplied(lod) => {
                    gpx_tool::dlog!("ui consumed LodApplied {lod}");
                    self.settings.custom_lod = Some(lod);
                    self.controlling = true;
                    if self.pending_lod == Some(lod) {
                        self.pending_lod = None;
                    }
                    self.error_line = None;
                }
                Event::Onboarded => {
                    gpx_tool::dlog!("ui consumed Onboarded");
                    self.controlling = false;
                    self.pending_dpi = None;
                    self.pending_rate = None;
                    self.pending_lod = None;
                    self.confirm_save = false;
                    self.saving = false;
                    self.saved_at = None;
                    self.error_line = None;
                }
                Event::SavedToMouse(dpi, lod, rate) => {
                    gpx_tool::dlog!(
                        "ui consumed SavedToMouse dpi={dpi} lod={lod} rate={rate:?}"
                    );
                    self.settings.custom_dpi = None;
                    self.settings.custom_lod = None;
                    self.settings.custom_rate = None;
                    self.controlling = false;
                    self.pending_dpi = None;
                    self.pending_rate = None;
                    self.pending_lod = None;
                    self.confirm_save = false;
                    self.saving = false;
                    self.saved_at = Some(Instant::now());
                    self.slider_dpi = dpi;
                    self.dpi_text = dpi.to_string();
                    self.slider_initialized = true;
                    self.error_line = None;
                }
                Event::SleepTimers(timers) => {
                    gpx_tool::dlog!(
                        "ui consumed SleepTimers ps={} po={}",
                        timers.ps_timeout,
                        timers.po_timeout
                    );
                    self.sleep_timers = Some(timers);
                    if !self.sleep_saving && !self.confirm_sleep {
                        self.sleep_selection = Some(timers);
                    }
                }
                Event::SleepSaved(timers) => {
                    gpx_tool::dlog!(
                        "ui consumed SleepSaved ps={} po={}",
                        timers.ps_timeout,
                        timers.po_timeout
                    );
                    self.sleep_timers = Some(timers);
                    self.sleep_selection = Some(timers);
                    self.confirm_sleep = false;
                    self.sleep_saving = false;
                    self.saved_at = Some(Instant::now());
                    self.error_line = None;
                }
                Event::Targets(new_settings, controlling) => {
                    self.settings = new_settings;
                    self.controlling = controlling;
                }
                Event::Error(message) => {
                    self.pending_dpi = None;
                    self.pending_rate = None;
                    self.pending_lod = None;
                    self.saving = false;
                    self.sleep_saving = false;
                    self.error_line = Some(message);
                }
                Event::Reapplied(time) => {
                    self.last_reapplied = Some(time);
                }
            }
        }
        handled
    }

    fn update_tooltip(&mut self) {
        let text = tooltip_text(self.asleep, self.idle, self.state.as_ref());
        let battery = match &self.state {
            Some(Ok(snapshot)) => Some(snapshot.battery.percent),
            _ => None,
        };
        let Some(tray) = &self.tray else {
            return;
        };
        if let Err(error) = tray.update(battery, &text) {
            self.error_line = Some(error);
            return;
        }
        if let Some(debug) = &mut self.debug {
            let hidden = !self.window_visible.load(Ordering::Relaxed);
            debug.tooltip(&text, hidden);
        }
    }

    fn queue_apply_dpi(&mut self, value: u16) {
        self.error_line = None;
        self.pending_dpi = Some(value);
        self.slider_dpi = value;
        self.dpi_text = value.to_string();
        gpx_tool::dlog!("ui event: apply dpi {value}");
        let _ = self.commands.send(Command::ApplyDpi(value));
        self.ui_ctx.request_repaint();
    }

    fn queue_apply_rate(&mut self, hz: u16) {
        self.error_line = None;
        self.pending_rate = Some(hz);
        gpx_tool::dlog!("ui event: apply rate {hz} Hz");
        let _ = self.commands.send(Command::ApplyRate(hz));
        self.ui_ctx.request_repaint();
    }

    fn queue_apply_lod(&mut self, lod: u8) {
        self.error_line = None;
        self.pending_lod = Some(lod);
        gpx_tool::dlog!("ui event: apply lod {lod}");
        let _ = self.commands.send(Command::ApplyLod(lod));
        self.ui_ctx.request_repaint();
    }

    fn queue_save_to_mouse(&mut self, dpi: u16, lod: u8, rate: Option<u16>) {
        self.confirm_save = false;
        self.saving = true;
        self.saved_at = None;
        self.error_line = None;
        gpx_tool::dlog!(
            "ui event: save to mouse dpi={dpi} lod={lod} rate={rate:?}"
        );
        let _ = self.commands.send(Command::SaveToMouse { dpi, lod, rate });
        self.ui_ctx.request_repaint();
    }

    fn queue_save_sleep(&mut self, ps_timeout: Option<u16>, po_timeout: Option<u16>) {
        self.confirm_sleep = false;
        self.sleep_saving = true;
        self.saved_at = None;
        self.error_line = None;
        gpx_tool::dlog!(
            "ui event: save sleep ps={ps_timeout:?} po={po_timeout:?}"
        );
        let _ = self.commands.send(Command::SaveSleep {
            ps_timeout,
            po_timeout,
        });
        self.ui_ctx.request_repaint();
    }

    fn pending_any(&self) -> bool {
        self.pending_dpi.is_some() || self.pending_rate.is_some() || self.pending_lod.is_some()
    }

    fn draw_settings_card(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        palette: &Palette,
    ) {
        let mut selected_rate: Option<u16> = None;
        let frame = egui::Frame::new()
            .fill(palette.card_bg)
            .corner_radius(10.0)
            .inner_margin(CARD_MARGIN);
        let frame = if palette.dark {
            frame
        } else {
            frame.stroke(egui::Stroke::new(1.0, palette.card_border))
        };
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut sleep_choice: Option<profile::SleepTimers> = None;

            egui::Grid::new("settings_grid")
                .num_columns(2)
                .spacing([8.0, 8.0])
                .show(ui, |ui| {
                    let active_dpi = self.pending_dpi.unwrap_or_else(|| {
                        if self.controlling {
                            self.settings.custom_dpi.unwrap_or(snapshot.dpi.x)
                        } else {
                            snapshot.dpi.x
                        }
                    });

                    label_cell(ui, "DPI", "How sensitive the mouse movement is.");
                    ui.allocate_ui_with_layout(
                        egui::vec2(CONTROL_WIDTH, 24.0),
                        row_layout(),
                        |ui| {
                            ui.spacing_mut().button_padding = egui::vec2(6.0, 4.0);
                            for preset in PRESETS {
                                let valid = hidpp::is_valid_dpi(&snapshot.ranges, preset);
                                let selected = preset == active_dpi;
                                let text = if selected {
                                    egui::RichText::new(preset.to_string()).color(self.on_accent)
                                } else {
                                    egui::RichText::new(preset.to_string())
                                };
                                let button = egui::Button::new(text)
                                    .selected(selected)
                                    .min_size(egui::vec2(42.0, 24.0));
                                let button = if selected {
                                    button.fill(self.accent)
                                } else {
                                    button
                                };
                                if ui.add_enabled(valid, button).clicked() {
                                    self.queue_apply_dpi(preset);
                                }
                            }
                        },
                    );
                    ui.end_row();

                    empty_label_cell(ui);
                    ui.allocate_ui_with_layout(
                        egui::vec2(CONTROL_WIDTH, 24.0),
                        row_layout(),
                        |ui| {
                            ui.spacing_mut().button_padding = egui::vec2(6.0, 4.0);
                            let values = hidpp::valid_dpi_values(&snapshot.ranges);
                            if values.is_empty() {
                                ui.label(
                                    egui::RichText::new("unavailable")
                                        .small()
                                        .color(palette.muted),
                                );
                                return;
                            }
                            let mut index = values
                                .iter()
                                .enumerate()
                                .min_by_key(|(_, value)| value.abs_diff(self.slider_dpi))
                                .map(|(index, _)| index)
                                .unwrap_or(0);
                            let slider = ui
                                .scope(|ui| {
                                    ui.visuals_mut().selection.bg_fill = self.accent;
                                    ui.add_sized(
                                        [90.0, 24.0],
                                        egui::Slider::new(&mut index, 0..=values.len() - 1)
                                            .show_value(false)
                                            .trailing_fill(true),
                                    )
                                })
                                .inner;
                            if slider.changed() {
                                self.slider_dpi = values[index];
                                self.dpi_text = self.slider_dpi.to_string();
                            }

                            let handle_radius = slider.rect.height() / 2.5;
                            let start = slider.rect.left() + handle_radius;
                            let end = slider.rect.right() - handle_radius;
                            let fraction = if values.len() > 1 {
                                index as f32 / (values.len() - 1) as f32
                            } else {
                                0.0
                            };
                            ui.painter().circle(
                                egui::pos2(
                                    start + (end - start) * fraction,
                                    slider.rect.center().y,
                                ),
                                handle_radius + 0.5,
                                self.accent,
                                egui::Stroke::new(1.0, self.on_accent),
                            );

                            let field = ui.add_sized(
                                [50.0, 24.0],
                                egui::TextEdit::singleline(&mut self.dpi_text)
                                    .align(egui::Align2::RIGHT_CENTER),
                            );
                            self.dpi_field_active = field.has_focus();
                            let typed = self.dpi_text.trim().parse::<u16>().ok();
                            let apply_typed = field.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter));
                            let apply_clicked = ui
                                .scope(|ui| {
                                    let widgets = &mut ui.visuals_mut().widgets;
                                    widgets.hovered.weak_bg_fill = self.accent;
                                    widgets.hovered.fg_stroke =
                                        egui::Stroke::new(1.0, self.on_accent);
                                    widgets.active.weak_bg_fill =
                                        self.accent.gamma_multiply(0.9);
                                    widgets.active.fg_stroke =
                                        egui::Stroke::new(1.0, self.on_accent);
                                    ui.button("Apply")
                                })
                                .inner
                                .clicked();
                            if apply_typed || apply_clicked {
                                if let Some(target) = typed
                                    .and_then(|value| {
                                        hidpp::nearest_valid_dpi(&snapshot.ranges, value)
                                    })
                                    .or_else(|| {
                                        hidpp::nearest_valid_dpi(
                                            &snapshot.ranges,
                                            self.slider_dpi,
                                        )
                                    })
                                {
                                    self.queue_apply_dpi(target);
                                }
                            }
                        },
                    );
                    ui.end_row();

                    label_cell(ui, "Polling rate", "How often the mouse reports movement to the PC.");
                    ui.allocate_ui_with_layout(
                        egui::vec2(CONTROL_WIDTH, 24.0),
                        row_layout(),
                        |ui| match &snapshot.rate {
                            Some(rate) if !rate.supported_hz.is_empty() => {
                                let mut current = self
                                    .pending_rate
                                    .filter(|hz| rate.supported_hz.contains(hz))
                                    .unwrap_or_else(|| {
                                        if rate.supported_hz.contains(&rate.current_hz) {
                                            rate.current_hz
                                        } else {
                                            self.settings
                                                .custom_rate
                                                .filter(|hz| rate.supported_hz.contains(hz))
                                                .unwrap_or(rate.supported_hz[0])
                                        }
                                    });
                                let previous = current;
                                let response = egui::ComboBox::from_id_salt("polling_rate")
                                    .width(CONTROL_WIDTH)
                                    .selected_text(format!("{current} Hz"))
                                    .show_ui(ui, |ui| {
                                        let widgets = &mut ui.visuals_mut().widgets;
                                        widgets.hovered.weak_bg_fill = self.accent;
                                        widgets.hovered.fg_stroke =
                                            egui::Stroke::new(1.0, self.on_accent);
                                        widgets.active.weak_bg_fill =
                                            self.accent.gamma_multiply(0.9);
                                        widgets.active.fg_stroke =
                                            egui::Stroke::new(1.0, self.on_accent);
                                        for hz in &rate.supported_hz {
                                            ui.selectable_value(
                                                &mut current,
                                                *hz,
                                                format!("{hz} Hz"),
                                            );
                                        }
                                    })
                                    .response;
                                response.on_hover_text(
                                    "How often the mouse reports movement to the PC.",
                                );
                                if current != previous {
                                    self.queue_apply_rate(current);
                                }
                                selected_rate = Some(current);
                            }
                            _ => {
                                ui.label(
                                    egui::RichText::new("unavailable")
                                        .small()
                                        .color(palette.muted),
                                );
                            }
                        },
                    );
                    ui.end_row();

                    if matches!(selected_rate, Some(hz) if hz >= 4000) {
                        empty_label_cell(ui);
                        ui.allocate_ui_with_layout(
                            egui::vec2(CONTROL_WIDTH, 24.0),
                            row_layout(),
                            |ui| {
                                ui.label(
                                    egui::RichText::new("Higher rates use more battery.")
                                        .small()
                                        .color(palette.muted),
                                );
                            },
                        );
                        ui.end_row();
                    }

                    let mut selected_lod = self.pending_lod.unwrap_or_else(|| {
                        if hidpp::LOD_VALUES.contains(&snapshot.dpi.lod) {
                            snapshot.dpi.lod
                        } else {
                            self.settings.custom_lod.unwrap_or(1)
                        }
                    });
                    let previous_lod = selected_lod;
                    label_cell(
                        ui,
                        "Lift-off",
                        "How high the mouse can be lifted before tracking stops.",
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(CONTROL_WIDTH, 24.0),
                        row_layout(),
                        |ui| {
                            let response = egui::ComboBox::from_id_salt("lod")
                                .width(CONTROL_WIDTH)
                                .selected_text(lod_label(selected_lod))
                                .show_ui(ui, |ui| {
                                    let widgets = &mut ui.visuals_mut().widgets;
                                    widgets.hovered.weak_bg_fill = self.accent;
                                    widgets.hovered.fg_stroke =
                                        egui::Stroke::new(1.0, self.on_accent);
                                    widgets.active.weak_bg_fill =
                                        self.accent.gamma_multiply(0.9);
                                    widgets.active.fg_stroke =
                                        egui::Stroke::new(1.0, self.on_accent);
                                    for lod in hidpp::LOD_VALUES {
                                        ui.selectable_value(
                                            &mut selected_lod,
                                            lod,
                                            lod_label(lod),
                                        );
                                    }
                                })
                                .response;
                            response.on_hover_text(
                                "How high the mouse can be lifted before tracking stops.",
                            );
                        },
                    );
                    if selected_lod != previous_lod {
                        self.queue_apply_lod(selected_lod);
                    }
                    ui.end_row();

                    let timers = self.sleep_timers;
                    let selection = self.sleep_selection.or(timers);
                    match timers {
                        Some(current) => {
                            let mut ps = selection
                                .map(|value| value.ps_timeout)
                                .unwrap_or(current.ps_timeout);
                            let previous_ps = ps;
                            label_cell(
                                ui,
                                "Sleep after",
                                "Idle time before the mouse sleeps to save battery.",
                            );
                            ui.allocate_ui_with_layout(
                                egui::vec2(CONTROL_WIDTH, 24.0),
                                row_layout(),
                                |ui| {
                                    let response = egui::ComboBox::from_id_salt("sleep_ps")
                                        .width(CONTROL_WIDTH)
                                        .selected_text(sleep_label(ps))
                                        .show_ui(ui, |ui| {
                                            let widgets = &mut ui.visuals_mut().widgets;
                                            widgets.hovered.weak_bg_fill = self.accent;
                                            widgets.hovered.fg_stroke =
                                                egui::Stroke::new(1.0, self.on_accent);
                                            widgets.active.weak_bg_fill =
                                                self.accent.gamma_multiply(0.9);
                                            widgets.active.fg_stroke =
                                                egui::Stroke::new(1.0, self.on_accent);
                                            for seconds in SLEEP_PRESETS {
                                                ui.selectable_value(
                                                    &mut ps,
                                                    seconds,
                                                    sleep_label(seconds),
                                                );
                                            }
                                        })
                                        .response;
                                    response.on_hover_text(
                                        "Idle time before the mouse sleeps to save battery.",
                                    );
                                },
                            );
                            if ps != previous_ps {
                                let mut po = selection
                                    .map(|value| value.po_timeout)
                                    .unwrap_or(current.po_timeout);
                                if po < ps {
                                    po = ps;
                                }
                                sleep_choice = Some(profile::SleepTimers {
                                    ps_timeout: ps,
                                    po_timeout: po,
                                });
                            }
                            ui.end_row();

                            let mut po = selection
                                .map(|value| value.po_timeout)
                                .unwrap_or(current.po_timeout);
                            if po < ps {
                                po = ps;
                            }
                            let previous_po = po;
                            label_cell(
                                ui,
                                "Deep sleep",
                                "How long after sleeping before the mouse turns off.",
                            );
                            ui.allocate_ui_with_layout(
                                egui::vec2(CONTROL_WIDTH, 24.0),
                                row_layout(),
                                |ui| {
                                    let response = egui::ComboBox::from_id_salt("sleep_po")
                                        .width(CONTROL_WIDTH)
                                        .selected_text(sleep_label(po))
                                        .show_ui(ui, |ui| {
                                            let widgets = &mut ui.visuals_mut().widgets;
                                            widgets.hovered.weak_bg_fill = self.accent;
                                            widgets.hovered.fg_stroke =
                                                egui::Stroke::new(1.0, self.on_accent);
                                            widgets.active.weak_bg_fill =
                                                self.accent.gamma_multiply(0.9);
                                            widgets.active.fg_stroke =
                                                egui::Stroke::new(1.0, self.on_accent);
                                            for seconds in SLEEP_PRESETS {
                                                if seconds >= ps {
                                                    ui.selectable_value(
                                                        &mut po,
                                                        seconds,
                                                        sleep_label(seconds),
                                                    );
                                                }
                                            }
                                        })
                                        .response;
                                    response.on_hover_text(
                                        "How long after sleeping before the mouse turns off.",
                                    );
                                },
                            );
                            if po != previous_po {
                                sleep_choice = Some(profile::SleepTimers {
                                    ps_timeout: ps,
                                    po_timeout: po,
                                });
                            }
                            ui.end_row();
                        }
                        None => {
                            label_cell(
                                ui,
                                "Sleep after",
                                "Idle time before the mouse sleeps to save battery.",
                            );
                            ui.allocate_ui_with_layout(
                                egui::vec2(CONTROL_WIDTH, 24.0),
                                row_layout(),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new("reading...")
                                            .small()
                                            .color(palette.muted),
                                    );
                                },
                            );
                            ui.end_row();
                        }
                    }
                });

            if let Some(choice) = sleep_choice {
                self.sleep_selection = Some(choice);
                self.confirm_sleep = true;
                self.confirm_save = false;
                self.saved_at = None;
            }

            if self.pending_any() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("applying...").small().color(palette.muted));
            }
        });
    }

    fn draw_appearance_card(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let frame = egui::Frame::new()
            .fill(palette.card_bg)
            .corner_radius(10.0)
            .inner_margin(CARD_MARGIN);
        let frame = if palette.dark {
            frame
        } else {
            frame.stroke(egui::Stroke::new(1.0, palette.card_border))
        };
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new("Appearance")
                    .small()
                    .color(palette.muted),
            );
            ui.add_space(4.0);
            egui::Grid::new("appearance_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Theme");
                    let mut theme = self.settings.theme;
                    egui::ComboBox::from_id_salt("theme")
                        .width(140.0)
                        .selected_text(theme_label(theme))
                        .show_ui(ui, |ui| {
                            let widgets = &mut ui.visuals_mut().widgets;
                            widgets.hovered.weak_bg_fill = self.accent;
                            widgets.hovered.fg_stroke =
                                egui::Stroke::new(1.0, self.on_accent);
                            widgets.active.weak_bg_fill = self.accent.gamma_multiply(0.9);
                            widgets.active.fg_stroke =
                                egui::Stroke::new(1.0, self.on_accent);
                            ui.selectable_value(&mut theme, ThemeMode::System, "System");
                            ui.selectable_value(&mut theme, ThemeMode::Dark, "Dark");
                            ui.selectable_value(&mut theme, ThemeMode::Light, "Light");
                        });
                    if theme != self.settings.theme {
                        self.set_theme_mode(theme);
                    }
                    ui.end_row();

                    ui.label("Accent");
                    ui.horizontal(|ui| {
                        for preset in ACCENT_PRESETS {
                            let active = self.settings.accent == preset;
                            if swatch(ui, rgb(preset), active, palette).clicked() {
                                self.set_accent(preset);
                            }
                        }
                        let mut custom = self.settings.accent;
                        let response = ui
                            .scope(|ui| {
                                ui.spacing_mut().interact_size = egui::vec2(18.0, 18.0);
                                ui.color_edit_button_srgb(&mut custom)
                            })
                            .inner;
                        if response.changed() {
                            self.set_accent(custom);
                        }
                        if !ACCENT_PRESETS.contains(&self.settings.accent) {
                            ui.painter().rect_stroke(
                                response.rect.expand(2.0),
                                4.0,
                                egui::Stroke::new(1.5, palette.text),
                                egui::StrokeKind::Outside,
                            );
                        }
                    });
                    ui.end_row();
                });
        });
    }

    fn draw_settings_view(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let mut back = false;
        ui.horizontal(|ui| {
            if back_button(ui, palette, self.accent, self.icon_font).clicked() {
                back = true;
            }
            ui.label(egui::RichText::new("Settings").size(28.0).strong());
        });
        if back {
            gpx_tool::dlog!("ui event: leave settings (back)");
            self.view = View::Main;
        }
        ui.add_space(12.0);

        self.draw_appearance_card(ui, palette);

        ui.add_space(12.0);
        let frame = card_frame(palette);
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new("Startup")
                    .small()
                    .color(palette.muted),
            );
            ui.add_space(4.0);
            let mut enabled = self.autostart;
            if accent_checkbox(
                ui,
                &mut enabled,
                "Start with Windows",
                palette,
                self.accent,
                self.on_accent,
            )
            .changed()
            {
                match autostart::set_enabled(enabled) {
                    Ok(()) => {
                        self.autostart = enabled;
                        self.error_line = None;
                    }
                    Err(error) => {
                        self.error_line =
                            Some(format!("Could not update Start with Windows: {error}"));
                    }
                }
            }
        });

        ui.add_space(12.0);
        let frame = card_frame(palette);
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new("About")
                    .small()
                    .color(palette.muted),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "Featherweight {}",
                    env!("CARGO_PKG_VERSION")
                ))
                .size(15.0),
            );
            ui.label(
                egui::RichText::new("No telemetry. No network access.")
                    .small()
                    .color(palette.muted),
            );
        });

        if let Some(error) = &self.error_line {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(error)
                    .small()
                    .color(palette.error),
            );
        }
    }

    fn lock_for(&self, max: Duration) -> Option<MutexGuard<'_, ()>> {
        let deadline = Instant::now() + max;
        loop {
            if let Ok(guard) = self.hid_lock.try_lock() {
                return Some(guard);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.reset_to_main.swap(false, Ordering::Relaxed) {
            self.view = View::Main;
        }
        self.refresh_system_theme();
        if self.title_bar_dark != Some(self.dark) {
            set_title_bar(frame, self.dark);
            self.title_bar_dark = Some(self.dark);
        }

        if self.start_hidden {
            let now = Instant::now();
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.window_visible.store(false, Ordering::Relaxed);
            memory::schedule_trim(Arc::clone(&self.window_visible));
            if now >= self.hide_at {
                self.start_hidden = false;
            } else {
                ctx.request_repaint_after(self.hide_at - now);
            }
        }

        if self.handle_events() {
            self.update_tooltip();
        }

        if self.tray.is_some()
            && !self.quitting.load(Ordering::Relaxed)
            && ctx.input(|input| input.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            let was_visible = self.window_visible.swap(false, Ordering::Relaxed);
            if was_visible {
                memory::schedule_trim(Arc::clone(&self.window_visible));
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.view == View::Settings
            && ui.input(|input| input.key_pressed(egui::Key::Escape))
        {
            gpx_tool::dlog!("ui event: leave settings (Esc)");
            self.view = View::Main;
        }

        let state = self.state.clone();
        let asleep = self.asleep;
        let idle = self.idle;
        let palette = self.palette();
        let mut content_height = 0.0;
        let mut content_top = 0.0;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(palette.bg)
                    .inner_margin(WINDOW_MARGIN),
            )
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                content_top = ui.cursor().top();
                if self.view == View::Settings {
                    self.draw_settings_view(ui, &palette);
                } else {
                    draw_header(ui, self, &palette);
                    if asleep || idle {
                        let message = if asleep {
                            "Mouse asleep \u{2014} move it to wake"
                        } else {
                            "Mouse idle \u{2014} move it to wake"
                        };
                        ui.label(
                            egui::RichText::new(message)
                                .small()
                                .color(palette.warning),
                        );
                        ui.add_space(8.0);
                    }
                    let body = |ui: &mut egui::Ui, app: &mut App| match &state {
                        None => {
                            ui.label("Reading mouse...");
                        }
                        Some(Err(message)) => draw_error(ui, message, &app.palette()),
                        Some(Ok(snapshot)) => draw_snapshot(ui, snapshot, app),
                    };
                    if asleep || idle {
                        ui.add_enabled_ui(false, |ui| {
                            body(ui, self);
                            draw_footer(ui, self);
                        });
                    } else {
                        body(ui, self);
                        draw_footer(ui, self);
                    }
                }
                content_height = ui.cursor().top() - content_top;
            });

        let target = content_height + WINDOW_MARGIN * 2.0;
        if target > WINDOW_MIN_HEIGHT && (target - self.last_fit_height).abs() > 2.0 {
            self.last_fit_height = target;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                WINDOW_WIDTH,
                target,
            )));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.commands.send(Command::Stop);

        let _guard = self.lock_for(Duration::from_millis(300));
        if self.host_engaged.load(Ordering::Relaxed) {
            let _ = hidpp::set_onboard_mode_quick();
        }
    }
}

fn lod_label(lod: u8) -> &'static str {
    match lod {
        1 => "Low",
        2 => "Medium",
        3 => "High",
        _ => "Unknown",
    }
}

fn row_layout() -> egui::Layout {
    egui::Layout::left_to_right(egui::Align::Center).with_main_align(egui::Align::Min)
}

fn label_cell(ui: &mut egui::Ui, text: &str, tooltip: &str) {
    ui.allocate_ui_with_layout(egui::vec2(LABEL_WIDTH, 24.0), row_layout(), |ui| {
        ui.label(text).on_hover_text(tooltip);
    });
}

fn empty_label_cell(ui: &mut egui::Ui) {
    ui.allocate_ui_with_layout(egui::vec2(LABEL_WIDTH, 24.0), row_layout(), |_ui| {});
}

fn sleep_label(seconds: u16) -> String {
    match seconds {
        30 => String::from("30 s"),
        60 => String::from("1 min"),
        120 => String::from("2 min"),
        300 => String::from("5 min"),
        600 => String::from("10 min"),
        900 => String::from("15 min"),
        1800 => String::from("30 min"),
        other => format!("{other} s (custom)"),
    }
}

fn theme_label(theme: ThemeMode) -> &'static str {
    match theme {
        ThemeMode::System => "System",
        ThemeMode::Dark => "Dark",
        ThemeMode::Light => "Light",
    }
}

fn swatch(
    ui: &mut egui::Ui,
    color: egui::Color32,
    active: bool,
    palette: &Palette,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let painter = ui.painter();
    let center = rect.center();
    painter.circle_filled(center, 7.0, color);
    if active {
        painter.circle_stroke(
            center,
            8.5,
            egui::Stroke::new(1.5, palette.text),
        );
    }
    response
}

fn card_frame(palette: &Palette) -> egui::Frame {
    let frame = egui::Frame::new()
        .fill(palette.card_bg)
        .corner_radius(10.0)
        .inner_margin(CARD_MARGIN);
    if palette.dark {
        frame
    } else {
        frame.stroke(egui::Stroke::new(1.0, palette.card_border))
    }
}

fn draw_header(ui: &mut egui::Ui, app: &mut App, palette: &Palette) {
    let mut open_settings = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Featherweight").size(28.0).strong());
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if cog_button(ui, palette, app.accent, app.icon_font).clicked() {
                    open_settings = true;
                }
            },
        );
    });
    if open_settings {
        gpx_tool::dlog!("ui event: open settings");
        app.view = View::Settings;
        app.ui_ctx.request_repaint();
    }
}

fn icon_button(
    ui: &mut egui::Ui,
    palette: &Palette,
    accent: egui::Color32,
) -> (egui::Response, egui::Rect, egui::Color32) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let color = if response.hovered() {
        accent
    } else {
        palette.muted
    };
    (response, rect, color)
}

fn cog_button(
    ui: &mut egui::Ui,
    palette: &Palette,
    accent: egui::Color32,
    icon_font: bool,
) -> egui::Response {
    let (response, rect, color) = icon_button(ui, palette, accent);
    if icon_font {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{E713}",
            egui::FontId::new(16.0, icon_font_family()),
            color,
        );
    } else {
        draw_cog(ui.painter(), rect.center(), 9.5, color);
    }
    response.on_hover_text("App settings")
}

fn back_button(
    ui: &mut egui::Ui,
    palette: &Palette,
    accent: egui::Color32,
    icon_font: bool,
) -> egui::Response {
    let (response, rect, color) = icon_button(ui, palette, accent);
    if icon_font {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{E72B}",
            egui::FontId::new(16.0, icon_font_family()),
            color,
        );
    } else {
        draw_back_arrow(ui.painter(), rect.center(), color);
    }
    response.on_hover_text("Back")
}

fn draw_cog(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    color: egui::Color32,
) {
    for index in 0..8 {
        let angle = std::f32::consts::TAU * index as f32 / 8.0;
        let direction = egui::vec2(angle.cos(), angle.sin());
        painter.line_segment(
            [
                center + direction * (radius * 0.58),
                center + direction * radius,
            ],
            egui::Stroke::new(3.0, color),
        );
    }
    painter.circle_stroke(
        center,
        radius * 0.56,
        egui::Stroke::new(1.6, color),
    );
    painter.circle_stroke(
        center,
        radius * 0.24,
        egui::Stroke::new(1.6, color),
    );
}

fn draw_back_arrow(
    painter: &egui::Painter,
    center: egui::Pos2,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.8, color);
    painter.line_segment(
        [
            egui::pos2(center.x - 8.0, center.y),
            egui::pos2(center.x + 8.0, center.y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x - 8.0, center.y),
            egui::pos2(center.x - 2.0, center.y - 5.5),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x - 8.0, center.y),
            egui::pos2(center.x - 2.0, center.y + 5.5),
        ],
        stroke,
    );
}

fn draw_snapshot(ui: &mut egui::Ui, snapshot: &Snapshot, app: &mut App) {
    let palette = app.palette();
    let subtitle = if snapshot.name == "PRO X 2" {
        "PRO X SUPERLIGHT 2 \u{00B7} Lightspeed"
    } else {
        snapshot.name.as_str()
    };
    ui.label(egui::RichText::new(subtitle).size(14.0).color(palette.muted));

    let percent = app.battery_override.unwrap_or(snapshot.battery.percent);
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(format!("{}%", percent))
            .size(40.0)
            .strong(),
    );

    let charging = is_charging(&snapshot.battery);
    ui.add(
        egui::ProgressBar::new(f32::from(percent) / 100.0)
            .fill(battery_bar_color(percent, charging, app.accent))
            .desired_width(ui.available_width())
            .desired_height(8.0)
            .animate(false),
    );
    ui.label(
        egui::RichText::new(battery_status_text(&snapshot.battery, percent))
            .small()
            .color(palette.muted),
    );

    ui.add_space(16.0);
    app.draw_settings_card(ui, snapshot, &palette);
    ui.add_space(16.0);
}

fn is_charging(battery: &Battery) -> bool {
    matches!(battery.status, 1 | 2 | 3)
}

fn battery_bar_color(percent: u8, charging: bool, accent: egui::Color32) -> egui::Color32 {
    if charging || percent >= BATTERY_LOW_PERCENT {
        accent
    } else {
        BATTERY_RED
    }
}

fn battery_status_text(battery: &Battery, percent: u8) -> String {
    if percent < BATTERY_LOW_PERCENT {
        if is_charging(battery) {
            "Low battery \u{00B7} charging".to_owned()
        } else {
            format!("Low battery \u{00B7} {}", battery.status_text())
        }
    } else {
        battery.status_text().to_owned()
    }
}

fn draw_footer(ui: &mut egui::Ui, app: &mut App) {
    let palette = app.palette();
    let snapshot_values = match &app.state {
        Some(Ok(snapshot)) => Some((
            snapshot.dpi.x,
            snapshot.dpi.lod,
            snapshot.rate.as_ref().map(|rate| rate.current_hz),
        )),
        _ => None,
    };
    let connected = snapshot_values.is_some() && !app.asleep && !app.idle;
    let controlling = app.controlling;
    let dpi = app
        .pending_dpi
        .or(app.settings.custom_dpi)
        .or(snapshot_values.map(|values| values.0))
        .unwrap_or(1600);
    let lod = app
        .pending_lod
        .or(app.settings.custom_lod)
        .or(snapshot_values.map(|values| values.1))
        .unwrap_or(1);
    let rate = app
        .pending_rate
        .or(app.settings.custom_rate)
        .or_else(|| snapshot_values.and_then(|values| values.2));
    let updated = app.updated.clone();
    let last_reapplied = app.last_reapplied.clone();

    let mut onboard = false;
    let mut start_save = false;
    let mut start_sleep_save = false;

    ui.horizontal_wrapped(|ui| {
        draw_status_pill(
            ui,
            &palette,
            app.accent,
            controlling,
            updated.as_deref(),
            last_reapplied.as_deref(),
        );
        if controlling {
            let save = ui.add_enabled(
                connected && !app.saving,
                egui::Button::new("Save to mouse"),
            );
            let save = if connected {
                save
            } else {
                save.on_disabled_hover_text("Mouse not connected")
            };
            if save.clicked() {
                gpx_tool::dlog!("ui event: save confirmation opened");
                app.confirm_save = true;
                app.confirm_sleep = false;
                app.saved_at = None;
            }
            if ui.button("Back to mouse profile").clicked() {
                onboard = true;
            }
        }
    });

    if onboard {
        app.error_line = None;
        app.pending_dpi = None;
        app.pending_rate = None;
        app.pending_lod = None;
        app.confirm_save = false;
        app.confirm_sleep = false;
        app.sleep_selection = app.sleep_timers;
        app.saved_at = None;
        gpx_tool::dlog!("ui event: onboard");
        let _ = app.commands.send(Command::Onboard);
        app.ui_ctx.request_repaint();
    }

    if app.saving {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Saving\u{2026}").small().color(palette.muted));
    } else if app.confirm_save && controlling {
        ui.add_space(8.0);
        let text = match rate {
            Some(hz) => format!(
                "Write {dpi} DPI \u{00B7} {} \u{00B7} {hz} Hz to the mouse's memory?",
                lod_label(lod)
            ),
            None => format!(
                "Write {dpi} DPI \u{00B7} {} to the mouse's memory?",
                lod_label(lod)
            ),
        };
        ui.add(egui::Label::new(
            egui::RichText::new(text).small().color(palette.text),
        ));
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                start_save = true;
            }
            if ui.button("Cancel").clicked() {
                app.confirm_save = false;
            }
        });
    }

    if app.sleep_saving {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Saving\u{2026}").small().color(palette.muted));
    } else if app.confirm_sleep {
        if let (Some(current), Some(selection)) = (app.sleep_timers, app.sleep_selection) {
            ui.add_space(8.0);
            let text = sleep_confirmation_text(current, selection);
            ui.add(egui::Label::new(
                egui::RichText::new(text).small().color(palette.text),
            ));
            ui.horizontal(|ui| {
                let save = ui.add_enabled(
                    connected && !app.sleep_saving,
                    egui::Button::new("Save"),
                );
                if save.clicked() {
                    start_sleep_save = true;
                }
                if ui.button("Cancel").clicked() {
                    app.confirm_sleep = false;
                    app.sleep_selection = Some(current);
                }
            });
        } else {
            app.confirm_sleep = false;
        }
    }

    if start_save {
        app.queue_save_to_mouse(dpi, lod, rate);
    }

    if start_sleep_save {
        if let (Some(current), Some(selection)) = (app.sleep_timers, app.sleep_selection) {
            let ps = if selection.ps_timeout != current.ps_timeout {
                Some(selection.ps_timeout)
            } else {
                None
            };
            let po = if selection.po_timeout != current.po_timeout {
                Some(selection.po_timeout)
            } else {
                None
            };
            if ps.is_none() && po.is_none() {
                app.confirm_sleep = false;
            } else {
                app.queue_save_sleep(ps, po);
            }
        }
    }

    if let Some(saved_at) = app.saved_at {
        if saved_at.elapsed() < Duration::from_secs(5) {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Saved to mouse")
                    .small()
                    .color(palette.text),
            );
            ui.ctx()
                .request_repaint_after(Duration::from_millis(250));
        } else {
            app.saved_at = None;
        }
    }

    if let Some(error) = &app.error_line {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(error)
                .small()
                .color(palette.error),
        );
    }
}

fn sleep_confirmation_text(current: profile::SleepTimers, selection: profile::SleepTimers) -> String {
    let ps_changed = selection.ps_timeout != current.ps_timeout;
    let po_changed = selection.po_timeout != current.po_timeout;
    if ps_changed && po_changed {
        format!(
            "Write sleep timer ({} \u{00B7} deep sleep {}) to the mouse's memory?",
            sleep_label(selection.ps_timeout),
            sleep_label(selection.po_timeout)
        )
    } else if po_changed {
        format!(
            "Write deep sleep timer ({}) to the mouse's memory?",
            sleep_label(selection.po_timeout)
        )
    } else {
        format!(
            "Write sleep timer ({}) to the mouse's memory?",
            sleep_label(selection.ps_timeout)
        )
    }
}

fn draw_status_pill(
    ui: &mut egui::Ui,
    palette: &Palette,
    accent: egui::Color32,
    active: bool,
    updated: Option<&str>,
    last_reapplied: Option<&str>,
) {
    let response = egui::Frame::new()
        .fill(palette.card_bg)
        .corner_radius(12.0)
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(
                    rect.center(),
                    4.0,
                    if active { accent } else { palette.muted },
                );
                let color = if active { palette.text } else { palette.muted };
                ui.label(egui::RichText::new(if active {
                    "App settings active"
                } else {
                    "Mouse profile"
                })
                .small()
                .color(color));
            });
        })
        .response;

    let mut tooltip = String::new();
    if let Some(time) = updated {
        tooltip.push_str(&format!("Updated {time}"));
    }
    if let Some(time) = last_reapplied {
        if !tooltip.is_empty() {
            tooltip.push('\n');
        }
        tooltip.push_str(&format!("Last re-applied {time}"));
    }
    if tooltip.is_empty() {
        tooltip = String::from("No update yet");
    }
    response.on_hover_text(tooltip);
}

fn accent_checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    text: &str,
    palette: &Palette,
    accent: egui::Color32,
    on_accent: egui::Color32,
) -> egui::Response {
    let mut response = ui
        .horizontal(|ui| {
            let (rect, box_response) =
                ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
            let enabled = ui.is_enabled();
            let fill = if *checked {
                if enabled {
                    accent
                } else {
                    accent.gamma_multiply(0.5)
                }
            } else {
                palette.control_bg
            };
            ui.painter().rect(
                rect,
                4.0,
                fill,
                egui::Stroke::new(1.0, palette.control_border),
                egui::StrokeKind::Inside,
            );
            if *checked {
                let color = if enabled {
                    on_accent
                } else {
                    egui::Color32::from_gray(180)
                };
                ui.painter().add(egui::Shape::line(
                    vec![
                        egui::pos2(rect.left() + 4.0, rect.center().y),
                        egui::pos2(rect.left() + 7.0, rect.bottom() - 4.0),
                        egui::pos2(rect.right() - 3.0, rect.top() + 4.0),
                    ],
                    egui::Stroke::new(2.0, color),
                ));
            }
            let label = ui.add(
                egui::Label::new(egui::RichText::new(text).size(15.0))
                    .sense(egui::Sense::click()),
            );
            box_response.union(label)
        })
        .inner;
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    response
}

fn draw_error(ui: &mut egui::Ui, error: &StateError, palette: &Palette) {
    let (heading, hint) = match error.kind {
        Unavailable::Receiver => (
            "Receiver not found",
            "Receiver not found. Plug the Lightspeed receiver back in; it is retried every 2 s.",
        ),
        Unavailable::Mouse => (
            "Mouse not found",
            "Mouse not found. Wake it by moving it, and close G HUB / Logitech software; \
             it is retried every 2 s.",
        ),
    };
    ui.heading(heading);
    ui.add_space(4.0);
    ui.label(egui::RichText::new(hint).color(palette.error));
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(&error.message)
            .small()
            .color(palette.muted),
    );
}

struct PollOutcome {
    state: Option<Result<Snapshot, StateError>>,
    asleep: bool,
    idle: bool,
    reapplied: Option<String>,
    error: Option<String>,
    settings_changed: bool,
}

struct Worker {
    settings: Settings,
    controlling: bool,
    device: Option<Device>,
    short: Option<hidpp::ShortReader>,
    raw: RawInput,
    pid: Option<u16>,
    force_sleep_once: bool,
    last_snapshot: Option<Snapshot>,
    last_error: Option<StateError>,
    last_ok: bool,
    asleep: bool,
    idle: bool,
    last_activity: Instant,
    last_path_check: Instant,
    known_pids: Vec<u16>,
    idle_override: Option<u64>,
    first_open_done: bool,
    reconnect_tries: u32,
    sleep_loaded: bool,
    sleep_timers: Option<profile::SleepTimers>,
    timers_dirty: bool,
}

impl Worker {
    fn open_device(&mut self) -> Result<Device, hidpp::Error> {
        let result = if !self.first_open_done {
            self.first_open_done = true;
            // A sleeping mouse often answers the index-1 probe late (around
            // 330 ms), which is longer than the quick initial scan allows.
            // Fall back to the reconnect path with its 1 s wake probe.
            Device::open().or_else(|error| {
                if error.is_no_answer() {
                    Device::open_reconnect(true)
                } else {
                    Err(error)
                }
            })
        } else {
            let full = self.reconnect_tries % 5 == 4;
            self.reconnect_tries = self.reconnect_tries.wrapping_add(1);
            Device::open_reconnect(full)
        };
        if result.is_ok() {
            self.reconnect_tries = 0;
            self.sleep_loaded = false;
        }
        if let Ok(device) = &result {
            let pid = device.product_id();
            self.pid = Some(pid);
            self.asleep = false;
            self.idle = false;
            self.last_activity = Instant::now();
            // Only the path we opened is known for now; any other present
            // long-report interface is probed once (the wired mouse wins).
            self.known_pids = vec![pid];
            self.raw.enable(&[pid]);
            if debuglog::enabled() {
                match hidpp::ShortReader::open_for(device) {
                    Ok(Some(reader)) => {
                        gpx_tool::dlog!("short collection opened (read-only)");
                        self.short = Some(reader);
                    }
                    Ok(None) => {
                        gpx_tool::dlog!("no HID++ short collection found for this receiver");
                        self.short = None;
                    }
                    Err(error) => {
                        gpx_tool::dlog!("could not open the short collection: {error}");
                        self.short = None;
                    }
                }
            }
        }
        result
    }

    fn idle_seconds(&self) -> u64 {
        self.idle_override
            .unwrap_or_else(|| {
                self.sleep_timers
                    .map(|timers| u64::from(timers.ps_timeout))
                    .unwrap_or(60)
            })
            .max(1)
    }

    /// When the worker should wake next. The idle deadline can come before
    /// the next poll so "idle" appears at the configured time without sending
    /// a request; polling itself stops after at most 30 s without movement.
    fn next_check(&self, ok: bool) -> Instant {
        let base = Instant::now() + self.wait_after(ok);
        if self.asleep || self.idle || self.device.is_none() {
            return base;
        }
        let idle_at = self.last_activity + Duration::from_secs(self.idle_seconds());
        base.min(idle_at)
    }

    fn wait_after(&self, ok: bool) -> Duration {
        if self.device.is_none() {
            RETRY_INTERVAL
        } else if self.asleep || self.idle {
            ASLEEP_SAFETY_INTERVAL
        } else if !ok {
            RETRY_INTERVAL
        } else if self.controlling {
            CUSTOM_INTERVAL
        } else {
            OK_INTERVAL
        }
    }

    fn tray_update(&self) -> (Option<u8>, String) {
        let state = match (&self.last_error, &self.last_snapshot) {
            (Some(error), _) => Some(Err(error.clone())),
            (None, Some(snapshot)) => Some(Ok(snapshot.clone())),
            (None, None) => None,
        };
        let battery = match &state {
            Some(Ok(snapshot)) => Some(snapshot.battery.percent),
            _ => None,
        };
        (battery, tooltip_text(self.asleep, self.idle, state.as_ref()))
    }

    fn update_hidden_tray(&self, tray: &Option<TrayHandle>, visible: &AtomicBool) {
        if visible.load(Ordering::Relaxed) {
            return;
        }
        let Some(tray) = tray else {
            return;
        };
        let (battery, text) = self.tray_update();
        gpx_tool::dlog!("tray update (hidden): {text}");
        if let Err(error) = tray.update(battery, &text) {
            gpx_tool::dlog!("{error}");
        }
    }

    /// Drops a dead handle. Asleep/idle must never be carried over to a new
    /// device; the next successful open always starts with a full poll.
    fn forget_device(&mut self) {
        self.device = None;
        self.pid = None;
        self.asleep = false;
        self.idle = false;
        self.raw.disable();
    }

    fn with_device_once<T>(
        &mut self,
        mut op: impl FnMut(&Device) -> Result<T, hidpp::Error>,
    ) -> Result<T, hidpp::Error> {
        let device = match self.device.take() {
            Some(device) => device,
            None => self.open_device()?,
        };
        match op(&device) {
            Ok(value) => {
                self.device = Some(device);
                Ok(value)
            }
            Err(error) => {
                if !error.is_broken_handle() {
                    self.device = Some(device);
                } else {
                    self.forget_device();
                }
                Err(error)
            }
        }
    }

    /// One cheap battery getter with a short timeout, used while the mouse is
    /// asleep. Keeps the cached handle on a timeout and drops it on a broken
    /// handle.
    fn wake_ping(&mut self) -> Result<(), hidpp::Error> {
        let mut device = match self.device.take() {
            Some(device) => device,
            None => self.open_device()?,
        };
        device.set_timeout(WAKE_PING_TIMEOUT);
        let result = device.battery().map(|_| ());
        device.set_timeout(HID_REQUEST_TIMEOUT);
        match result {
            Ok(()) => {
                self.device = Some(device);
                Ok(())
            }
            Err(error) => {
                if !error.is_broken_handle() {
                    self.device = Some(device);
                } else {
                    self.forget_device();
                }
                Err(error)
            }
        }
    }

    /// Reads (and with GPX_DEBUG=1 logs) pending unsolicited reports from both
    /// the long and the short collection while the worker is idle. Returns true
    /// when the handle broke (device unplugged).
    ///
    /// Reports are logged only. A request that timed out can produce a late
    /// reply when the mouse wakes; that reply is a consequence of our own
    /// request, not a passive wake signal, so it no longer triggers a poll
    /// (which would keep the mouse awake).
    fn drain_reports(&mut self, hid_lock: &Mutex<()>) -> bool {
        let Some(device) = self.device.take() else {
            return false;
        };
        let Ok(_guard) = hid_lock.try_lock() else {
            self.device = Some(device);
            return false;
        };

        let mut drop_device = false;
        loop {
            match device.read_input_report(Duration::ZERO) {
                Ok(Some(report)) => {
                    gpx_tool::dlog!("report long raw={}", debuglog::hex(&report));
                }
                Ok(None) => break,
                Err(error) => {
                    gpx_tool::dlog!("idle long read error: {error}");
                    drop_device = error.is_broken_handle();
                    break;
                }
            }
        }

        if let Some(short) = self.short.take() {
            let mut keep_short = true;
            loop {
                match short.read(Duration::ZERO) {
                    Ok(Some(report)) => {
                        gpx_tool::dlog!("report short raw={}", debuglog::hex(&report));
                    }
                    Ok(None) => break,
                    Err(error) => {
                        gpx_tool::dlog!("idle short read error: {error}");
                        keep_short = !error.is_broken_handle();
                        break;
                    }
                }
            }
            if keep_short {
                self.short = Some(short);
            }
        }

        if !drop_device {
            self.device = Some(device);
        } else {
            self.forget_device();
        }
        drop_device
    }

    /// Switches to the mouse's own wired interface when a new HID++ long
    /// interface appears (charging cable connected while the receiver link is
    /// down). Enumeration is passive; only a new interface triggers one probe.
    fn try_switch_to_wired(&mut self) -> bool {
        let pids = match hidpp::long_report_interface_pids() {
            Ok(pids) => pids,
            Err(_) => return false,
        };
        let new_interface = pids
            .iter()
            .any(|present| !self.known_pids.contains(present));
        if !new_interface {
            return false;
        }
        self.known_pids = pids.clone();
        if self
            .device
            .as_ref()
            .map(|device| device.device_index() == 0xFF)
            .unwrap_or(false)
        {
            return false;
        }

        gpx_tool::dlog!(
            "path: new long-report interface (PIDs {:04X?}), trying the wired mouse",
            pids
        );
        match Device::open_wired() {
            Ok(device) => {
                let pid = device.product_id();
                gpx_tool::dlog!(
                    "path: using wired mouse PID {pid:04X} (device index {:#04X})",
                    device.device_index()
                );
                self.device = Some(device);
                self.pid = Some(pid);
                self.asleep = false;
                self.idle = false;
                self.reconnect_tries = 0;
                self.sleep_loaded = false;
                self.last_activity = Instant::now();
                self.raw.enable(&[pid]);
                true
            }
            Err(error) => {
                gpx_tool::dlog!("path: wired probe failed: {error}");
                false
            }
        }
    }

    fn run_command<T>(
        &mut self,
        stop: &AtomicBool,
        mut op: impl FnMut(&Device) -> Result<T, hidpp::Error>,
    ) -> Result<T, hidpp::Error> {
        let mut last = hidpp::Error::Timeout;
        for attempt in 0..WAKE_ATTEMPTS {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if attempt > 0 {
                thread::sleep(WAKE_PAUSE);
            }
            let device = match self.device.take() {
                Some(device) => device,
                None => match self.open_device() {
                    Ok(device) => device,
                    Err(error) => {
                        last = error;
                        continue;
                    }
                },
            };
            match op(&device) {
                Ok(value) => {
                    self.device = Some(device);
                    self.asleep = false;
                    self.idle = false;
                    return Ok(value);
                }
                Err(error) if error.is_no_answer() || error.is_broken_handle() => {
                    if !error.is_broken_handle() {
                        self.device = Some(device);
                    } else {
                        self.forget_device();
                    }
                    last = error;
                }
                Err(error) => {
                    self.device = Some(device);
                    return Err(error);
                }
            }
        }
        Err(last)
    }

    fn send_state(
        &self,
        events: &EventQueue,
        ctx: &egui::Context,
        visible: &AtomicBool,
        label: &str,
    ) {
        let Some(snapshot) = &self.last_snapshot else {
            return;
        };
        gpx_tool::dlog!(
            "worker result sent: State ({label}) dpi={} lod={} mode={} rate={:?}",
            snapshot.dpi.x,
            snapshot.dpi.lod,
            snapshot.mode,
            snapshot.rate.as_ref().map(|rate| rate.current_hz)
        );
        let _ = events.send(Event::State(Ok(snapshot.clone())));
        request_repaint_if_visible(ctx, visible);
    }

    fn poll(
        &mut self,
        hid_lock: &Mutex<()>,
        stop: &AtomicBool,
        host_engaged: &AtomicBool,
    ) -> PollOutcome {
        let started = Instant::now();

        if self.asleep || self.idle {
            let ping = {
                let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                self.wake_ping()
            };
            match ping {
                Ok(()) => {
                    gpx_tool::dlog!("worker inactive probe answered; reading state");
                    self.asleep = false;
                    self.idle = false;
                    self.last_activity = Instant::now();
                }
                Err(error) if error.is_no_answer() => {
                    self.last_ok = false;
                    if self.device.is_none() {
                        // The handle could not be opened at all: this is a
                        // connection failure, not a sleep timeout.
                        self.asleep = false;
                        self.idle = false;
                        let state_error = StateError::from_error(&error);
                        self.last_error = Some(state_error.clone());
                        gpx_tool::dlog!(
                            "worker inactive probe could not open the mouse: {error}; retrying in {} s",
                            RETRY_INTERVAL.as_secs()
                        );
                        return PollOutcome {
                            state: Some(Err(state_error)),
                            asleep: false,
                            idle: false,
                            reapplied: None,
                            error: None,
                            settings_changed: false,
                        };
                    }
                    self.asleep = true;
                    self.idle = false;
                    if let Some(pid) = self.pid {
                        self.raw.enable(&[pid]);
                    }
                    gpx_tool::dlog!(
                        "worker inactive probe timed out; mouse asleep, next safety probe in {} s",
                        ASLEEP_SAFETY_INTERVAL.as_secs()
                    );
                    return PollOutcome {
                        state: None,
                        asleep: true,
                        idle: false,
                        reapplied: None,
                        error: None,
                        settings_changed: false,
                    };
                }
                Err(error) => {
                    self.last_ok = false;
                    self.asleep = false;
                    self.idle = false;
                    gpx_tool::dlog!("worker inactive probe failed: {error}");
                    let state_error = StateError::from_error(&error);
                    self.last_error = Some(state_error.clone());
                    return PollOutcome {
                        state: Some(Err(state_error)),
                        asleep: false,
                        idle: false,
                        reapplied: None,
                        error: None,
                        settings_changed: false,
                    };
                }
            }
        }

        let state = {
            let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
            self.with_device_once(|device| device.snapshot())
        };

        let snapshot = match state {
            Ok(snapshot) => {
                self.last_ok = true;
                self.asleep = false;
                self.idle = false;
                self.last_error = None;
                self.last_snapshot = Some(snapshot.clone());
                if !self.sleep_loaded {
                    let timers = {
                        let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                        self.with_device_once(|device| profile::read_sleep_timers_device(device))
                    };
                    match timers {
                        Ok(timers) => {
                            self.sleep_loaded = true;
                            self.sleep_timers = Some(timers);
                            self.timers_dirty = true;
                            gpx_tool::dlog!(
                                "worker read sleep timers ps={} po={}",
                                timers.ps_timeout,
                                timers.po_timeout
                            );
                        }
                        Err(error) => {
                            gpx_tool::dlog!("worker could not read sleep timers: {error}");
                        }
                    }
                }
                snapshot
            }
            Err(error) if error.is_no_answer() => {
                self.last_ok = false;
                if self.device.is_none() {
                    // Open/reconnect failed: report the connection error and
                    // retry every 2 s; never carry an asleep state over.
                    self.asleep = false;
                    self.idle = false;
                    let state_error = StateError::from_error(&error);
                    self.last_error = Some(state_error.clone());
                    gpx_tool::dlog!(
                        "worker snapshot could not open the mouse: {error}; retrying in {} s",
                        RETRY_INTERVAL.as_secs()
                    );
                    return PollOutcome {
                        state: Some(Err(state_error)),
                        asleep: false,
                        idle: false,
                        reapplied: None,
                        error: None,
                        settings_changed: false,
                    };
                }
                if self.try_switch_to_wired() {
                    return PollOutcome {
                        state: None,
                        asleep: false,
                        idle: false,
                        reapplied: None,
                        error: None,
                        settings_changed: false,
                    };
                }
                self.asleep = true;
                self.idle = false;
                if let Some(pid) = self.pid {
                    self.raw.enable(&[pid]);
                }
                gpx_tool::dlog!(
                    "worker snapshot timed out; mouse marked asleep, raw input tracking on, safety probe in {} s",
                    ASLEEP_SAFETY_INTERVAL.as_secs()
                );
                return PollOutcome {
                    state: None,
                    asleep: true,
                    idle: false,
                    reapplied: None,
                    error: None,
                    settings_changed: false,
                };
            }
            Err(error) => {
                self.last_ok = false;
                self.asleep = false;
                self.idle = false;
                let state_error = StateError::from_error(&error);
                self.last_error = Some(state_error.clone());
                return PollOutcome {
                    state: Some(Err(state_error)),
                    asleep: false,
                    idle: false,
                    reapplied: None,
                    error: None,
                    settings_changed: false,
                };
            }
        };

        if self.force_sleep_once {
            self.force_sleep_once = false;
            self.asleep = true;
            self.idle = false;
            if let Some(pid) = self.pid {
                self.raw.enable(&[pid]);
            }
            gpx_tool::dlog!("debug: mouse forced asleep for wake-detection test");
            return PollOutcome {
                state: Some(Ok(snapshot)),
                asleep: true,
                idle: false,
                reapplied: None,
                error: None,
                settings_changed: false,
            };
        }

        if !self.controlling || stop.load(Ordering::Relaxed) {
            return PollOutcome {
                state: Some(Ok(snapshot)),
                asleep: false,
                idle: false,
                reapplied: None,
                error: None,
                settings_changed: false,
            };
        }

        let settings_before = self.settings;
        let controlling_before = self.controlling;

        if let Some(dpi) = self.settings.custom_dpi {
            if !hidpp::is_valid_dpi(&snapshot.ranges, dpi) {
                self.settings.custom_dpi = None;
            }
        }
        if let Some(hz) = self.settings.custom_rate {
            if let Some(rate) = &snapshot.rate {
                if !rate.supported_hz.contains(&hz) {
                    self.settings.custom_rate = None;
                }
            }
        }
        if let Some(lod) = self.settings.custom_lod {
            if !hidpp::LOD_VALUES.contains(&lod) {
                self.settings.custom_lod = None;
            }
        }
        if !self.settings.has_custom() {
            self.controlling = false;
        }
        let settings_changed =
            self.settings != settings_before || self.controlling != controlling_before;

        if !self.controlling {
            return PollOutcome {
                state: Some(Ok(snapshot)),
                asleep: false,
                idle: false,
                reapplied: None,
                error: None,
                settings_changed,
            };
        }

        let dpi_target = self.settings.custom_dpi;
        let lod_target = self.settings.custom_lod;
        let rate_target = self.settings.custom_rate;

        let dpi_differs = matches!(
            dpi_target,
            Some(target) if snapshot.dpi.x != target || snapshot.dpi.y != target
        );
        let lod_differs = matches!(lod_target, Some(lod) if snapshot.dpi.lod != lod);
        let rate_differs = match (rate_target, &snapshot.rate) {
            (Some(hz), Some(rate)) => rate.current_hz != hz,
            _ => false,
        };

        let needs_reapply = snapshot.mode != 2 || dpi_differs || lod_differs || rate_differs;
        if !needs_reapply {
            return PollOutcome {
                state: Some(Ok(snapshot)),
                asleep: false,
                idle: false,
                reapplied: None,
                error: None,
                settings_changed,
            };
        }

        let host_known = snapshot.mode == 2;
        let mut error: Option<String> = None;
        let mut applied_any = false;
        let mut applied_dpi = None;
        let mut applied_rate = None;

        if let Some(target) = dpi_target {
            let apply_dpi =
                !host_known || snapshot.dpi.x != target || snapshot.dpi.y != target || lod_differs;
            if apply_dpi && !stop.load(Ordering::Relaxed) {
                let lod = lod_target.unwrap_or(snapshot.dpi.lod);
                let result = {
                    let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                    self.with_device_once(|device| {
                        device.apply_dpi_lod_fast(target, target, lod, host_known)
                    })
                };
                match result {
                    Ok(dpi) => {
                        applied_any = true;
                        applied_dpi = Some(dpi);
                    }
                    Err(e) => {
                        error = Some(format!("Could not re-apply {target} DPI: {e}"));
                    }
                }
            }
        } else if let Some(lod) = lod_target {
            if (!host_known || lod_differs) && !stop.load(Ordering::Relaxed) {
                let result = {
                    let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                    self.with_device_once(|device| {
                        device.apply_dpi_lod_fast(snapshot.dpi.x, snapshot.dpi.y, lod, host_known)
                    })
                };
                match result {
                    Ok(dpi) => {
                        applied_any = true;
                        applied_dpi = Some(dpi);
                    }
                    Err(e) => {
                        error = Some(format!("Could not re-apply LOD {lod}: {e}"));
                    }
                }
            }
        }

        if let Some(hz) = rate_target {
            if (!host_known || rate_differs) && !stop.load(Ordering::Relaxed) {
                let current = snapshot.rate.as_ref().map(|rate| rate.current_hz);
                let result = {
                    let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                    self.with_device_once(|device| {
                        device.apply_report_rate_fast(hz, host_known, current)
                    })
                };
                match result {
                    Ok(confirmed) => {
                        applied_any = true;
                        applied_rate = Some(confirmed);
                    }
                    Err(e) => {
                        if error.is_none() {
                            error = Some(format!("Could not re-apply {hz} Hz: {e}"));
                        }
                    }
                }
            }
        }

        if applied_any {
            host_engaged.store(true, Ordering::Relaxed);
            let time = local_time_string();
            let mut patched = snapshot.clone();
            patched.mode = 2;
            if let Some(dpi) = applied_dpi {
                patched.dpi = dpi;
            }
            if let Some(hz) = applied_rate {
                if let Some(rate) = &mut patched.rate {
                    rate.current_hz = hz;
                }
            }
            let fresh = {
                let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                self.with_device_once(|device| device.snapshot())
            };
            let state = match fresh {
                Ok(snapshot) => {
                    self.last_snapshot = Some(snapshot.clone());
                    snapshot
                }
                Err(_) => {
                    self.last_snapshot = Some(patched.clone());
                    patched
                }
            };
            gpx_tool::dlog!(
                "worker poll re-applied in {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
            return PollOutcome {
                state: Some(Ok(state)),
                asleep: false,
                idle: false,
                reapplied: Some(time),
                error,
                settings_changed,
            };
        }

        PollOutcome {
            state: Some(Ok(snapshot)),
            asleep: false,
            idle: false,
            reapplied: None,
            error,
            settings_changed,
        }
    }
}

struct BatchOutcome {
    stop: bool,
    force_poll: bool,
    failed: bool,
}

fn push_coalesced(list: &mut Vec<Command>, command: Command) {
    match &command {
        Command::Stop => list.clear(),
        Command::Onboard => list.retain(|item| matches!(item, Command::Stop)),
        Command::Refresh => list.retain(|item| !matches!(item, Command::Refresh)),
        Command::ApplyDpi(_) => list.retain(|item| !matches!(item, Command::ApplyDpi(_))),
        Command::ApplyRate(_) => list.retain(|item| !matches!(item, Command::ApplyRate(_))),
        Command::ApplyLod(_) => list.retain(|item| !matches!(item, Command::ApplyLod(_))),
        Command::SaveToMouse { .. } => {
            list.retain(|item| !matches!(item, Command::SaveToMouse { .. }))
        }
        Command::SaveSleep { .. } => {
            list.retain(|item| !matches!(item, Command::SaveSleep { .. }))
        }
        Command::Appearance(..) => {
            list.retain(|item| !matches!(item, Command::Appearance(..)))
        }
    }
    list.push(command);
}

fn collect_commands(first: Command, commands: &Receiver<Command>) -> Vec<Command> {
    let mut list = Vec::new();
    push_coalesced(&mut list, first);
    while let Ok(command) = commands.try_recv() {
        push_coalesced(&mut list, command);
    }
    list
}

fn send_error(events: &EventQueue, message: String) {
    gpx_tool::dlog!("worker result sent: Error {message}");
    let _ = events.send(Event::Error(message));
}

/// The hidden window gets no egui frames on purpose: tray updates are handled
/// by the worker, and eframe would otherwise run logic/paint passes for every
/// requested repaint. Repaints are only needed while the window is visible.
fn request_repaint_if_visible(ctx: &egui::Context, visible: &AtomicBool) {
    if visible.load(Ordering::Relaxed) {
        ctx.request_repaint();
    }
}

fn current_dpi(
    worker: &mut Worker,
    stop: &AtomicBool,
    hid_lock: &Mutex<()>,
) -> Result<hidpp::Dpi, hidpp::Error> {
    if let Some(snapshot) = &worker.last_snapshot {
        return Ok(snapshot.dpi);
    }
    let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
    worker.run_command(stop, |device| device.dpi())
}

fn handle_batch(
    worker: &mut Worker,
    commands: Vec<Command>,
    events: &EventQueue,
    ctx: &egui::Context,
    visible: &AtomicBool,
    stop: &AtomicBool,
    host_engaged: &AtomicBool,
    hid_lock: &Mutex<()>,
    persist: bool,
) -> BatchOutcome {
    if commands
        .iter()
        .any(|command| matches!(command, Command::Stop))
    {
        return BatchOutcome {
            stop: true,
            force_poll: false,
            failed: false,
        };
    }

    let mut force_poll = false;
    let mut failed = false;
    let mut applied_any = false;

    for command in commands {
        match command {
            Command::Stop => {
                return BatchOutcome {
                    stop: true,
                    force_poll: false,
                    failed: false,
                };
            }
            Command::Refresh => {
                force_poll = true;
            }
            Command::Appearance(theme, accent) => {
                worker.settings.theme = theme;
                worker.settings.accent = accent;
                if persist {
                    if let Err(error) = settings::save(&worker.settings) {
                        send_error(events, error);
                    }
                }
            }
            Command::Onboard => {
                let result = {
                    let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                    worker.run_command(stop, |device| device.apply_onboard_mode())
                };
                match result {
                    Ok(()) => {
                        worker.controlling = false;
                        host_engaged.store(false, Ordering::Relaxed);
                        if let Some(snapshot) = &mut worker.last_snapshot {
                            snapshot.mode = 1;
                        }
                        gpx_tool::dlog!("worker result sent: Onboarded");
                        if events.send(Event::Onboarded).is_err() {
                            return BatchOutcome {
                                stop: true,
                                force_poll: false,
                                failed: false,
                            };
                        }
                        force_poll = true;
                    }
                    Err(error) => {
                        failed = true;
                        send_error(events, format!("Could not switch to onboard mode: {error}"));
                    }
                }
            }
            Command::SaveToMouse { dpi, lod, rate } => {
                let result = {
                    let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                    worker.with_device_once(|device| {
                        profile::save_to_mouse_device(
                            device,
                            profile::SaveFields {
                                dpi: Some(dpi),
                                lod: Some(lod),
                                rate,
                                ..profile::SaveFields::default()
                            },
                            true,
                        )
                    })
                };
                match result {
                    Ok(saved) => {
                        worker.settings.custom_dpi = None;
                        worker.settings.custom_lod = None;
                        worker.settings.custom_rate = None;
                        worker.controlling = false;
                        host_engaged.store(false, Ordering::Relaxed);
                        applied_any = true;

                        let fresh = {
                            let _guard =
                                hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                            worker.with_device_once(|device| {
                                device.apply_onboard_mode()?;
                                device.snapshot()
                            })
                        };
                        match fresh {
                            Ok(mut snapshot) => {
                                snapshot.mode = 1;
                                snapshot.dpi.x = saved.dpi;
                                snapshot.dpi.y = saved.dpi;
                                snapshot.dpi.lod = saved.lod;
                                if let Some(hz) = saved.rate {
                                    if let Some(rate) = &mut snapshot.rate {
                                        rate.current_hz = hz;
                                    }
                                }
                                worker.last_snapshot = Some(snapshot.clone());
                                let _ = events.send(Event::State(Ok(snapshot)));
                            }
                            Err(error) => {
                                gpx_tool::dlog!(
                                    "worker: snapshot after save failed: {error}"
                                );
                                if let Some(snapshot) = &mut worker.last_snapshot {
                                    snapshot.mode = 1;
                                    snapshot.dpi.x = saved.dpi;
                                    snapshot.dpi.y = saved.dpi;
                                    snapshot.dpi.lod = saved.lod;
                                    if let Some(hz) = saved.rate {
                                        if let Some(rate) = &mut snapshot.rate {
                                            rate.current_hz = hz;
                                        }
                                    }
                                    let _ = events.send(Event::State(Ok(snapshot.clone())));
                                }
                            }
                        }
                        request_repaint_if_visible(ctx, visible);
                        gpx_tool::dlog!(
                            "worker result sent: SavedToMouse dpi={} lod={} rate={:?}",
                            saved.dpi,
                            saved.lod,
                            saved.rate
                        );
                        if events
                            .send(Event::SavedToMouse(saved.dpi, saved.lod, saved.rate))
                            .is_err()
                        {
                            return BatchOutcome {
                                stop: true,
                                force_poll: false,
                                failed: false,
                            };
                        }
                    }
                    Err(error) => {
                        failed = true;
                        send_error(
                            events,
                            format!("Could not save to the mouse: {error}"),
                        );
                    }
                }
            }
            Command::SaveSleep {
                ps_timeout,
                po_timeout,
            } => {
                let fields = profile::SaveFields {
                    ps_timeout,
                    po_timeout,
                    ..profile::SaveFields::default()
                };
                let result = {
                    let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                    worker.with_device_once(|device| {
                        profile::save_to_mouse_device(device, fields, true)
                    })
                };
                match result {
                    Ok(saved) => {
                        host_engaged.store(false, Ordering::Relaxed);
                        let timers = saved.sleep;
                        worker.sleep_timers = Some(timers);
                        worker.sleep_loaded = true;
                        let fresh = {
                            let _guard =
                                hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                            worker.with_device_once(|device| {
                                device.apply_onboard_mode()?;
                                device.snapshot()
                            })
                        };
                        match fresh {
                            Ok(snapshot) => {
                                worker.last_snapshot = Some(snapshot.clone());
                                let _ = events.send(Event::State(Ok(snapshot)));
                            }
                            Err(error) => {
                                gpx_tool::dlog!(
                                    "worker: snapshot after sleep save failed: {error}"
                                );
                            }
                        }
                        request_repaint_if_visible(ctx, visible);
                        gpx_tool::dlog!(
                            "worker result sent: SleepSaved ps={} po={}",
                            timers.ps_timeout,
                            timers.po_timeout
                        );
                        if events.send(Event::SleepSaved(timers)).is_err() {
                            return BatchOutcome {
                                stop: true,
                                force_poll: false,
                                failed: false,
                            };
                        }
                        force_poll = true;
                    }
                    Err(error) => {
                        failed = true;
                        send_error(
                            events,
                            format!("Could not save the sleep timer: {error}"),
                        );
                    }
                }
            }
            Command::ApplyDpi(value) => {
                let base = match current_dpi(worker, stop, hid_lock) {
                    Ok(dpi) => dpi,
                    Err(error) => {
                        failed = true;
                        send_error(events, format!("Could not read the current DPI: {error}"));
                        continue;
                    }
                };
                if let Some(snapshot) = &worker.last_snapshot {
                    if !snapshot.ranges.is_empty() && !hidpp::is_valid_dpi(&snapshot.ranges, value)
                    {
                        failed = true;
                        send_error(
                            events,
                            format!("Could not set {value} DPI: not a valid DPI value"),
                        );
                        continue;
                    }
                }
                let host_known = worker
                    .last_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.mode == 2)
                    .unwrap_or(false);
                let result = {
                    let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                    worker.run_command(stop, |device| {
                        device.apply_dpi_lod_fast(value, value, base.lod, host_known)
                    })
                };
                match result {
                    Ok(dpi) => {
                        worker.settings.custom_dpi = Some(dpi.x);
                        worker.controlling = true;
                        applied_any = true;
                        host_engaged.store(true, Ordering::Relaxed);
                        if let Some(snapshot) = &mut worker.last_snapshot {
                            snapshot.dpi = dpi;
                            snapshot.mode = 2;
                        }
                        worker.send_state(events, ctx, visible, "after DPI");
                        gpx_tool::dlog!("worker result sent: DpiApplied {}", dpi.x);
                        if events.send(Event::DpiApplied(dpi.x)).is_err() {
                            return BatchOutcome {
                                stop: true,
                                force_poll: false,
                                failed: false,
                            };
                        }
                    }
                    Err(error) => {
                        failed = true;
                        send_error(events, format!("Could not set {value} DPI: {error}"));
                    }
                }
            }
            Command::ApplyLod(lod) => {
                let base = match current_dpi(worker, stop, hid_lock) {
                    Ok(dpi) => dpi,
                    Err(error) => {
                        failed = true;
                        send_error(events, format!("Could not read the current DPI: {error}"));
                        continue;
                    }
                };
                let host_known = worker
                    .last_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.mode == 2)
                    .unwrap_or(false);
                let result = {
                    let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                    worker.run_command(stop, |device| {
                        device.apply_dpi_lod_fast(base.x, base.y, lod, host_known)
                    })
                };
                match result {
                    Ok(dpi) => {
                        worker.settings.custom_lod = Some(dpi.lod);
                        worker.controlling = true;
                        applied_any = true;
                        host_engaged.store(true, Ordering::Relaxed);
                        if let Some(snapshot) = &mut worker.last_snapshot {
                            snapshot.dpi = dpi;
                            snapshot.mode = 2;
                        }
                        worker.send_state(events, ctx, visible, "after LOD");
                        gpx_tool::dlog!("worker result sent: LodApplied {}", dpi.lod);
                        if events.send(Event::LodApplied(dpi.lod)).is_err() {
                            return BatchOutcome {
                                stop: true,
                                force_poll: false,
                                failed: false,
                            };
                        }
                    }
                    Err(error) => {
                        failed = true;
                        send_error(events, format!("Could not set LOD {lod}: {error}"));
                    }
                }
            }
            Command::ApplyRate(hz) => {
                let current = worker
                    .last_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.rate.as_ref())
                    .map(|rate| rate.current_hz);
                let host_known = worker
                    .last_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.mode == 2)
                    .unwrap_or(false);
                let result = {
                    let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                    worker.run_command(stop, |device| {
                        device.apply_report_rate_fast(hz, host_known, current)
                    })
                };
                match result {
                    Ok(confirmed) => {
                        worker.settings.custom_rate = Some(confirmed);
                        worker.controlling = true;
                        applied_any = true;
                        host_engaged.store(true, Ordering::Relaxed);
                        let mut have_rate = false;
                        if let Some(snapshot) = &mut worker.last_snapshot {
                            snapshot.mode = 2;
                            if let Some(rate) = &mut snapshot.rate {
                                rate.current_hz = confirmed;
                                have_rate = true;
                            }
                        }
                        if have_rate {
                            worker.send_state(events, ctx, visible, "after rate");
                        } else {
                            let fresh = {
                                let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
                                worker.with_device_once(|device| device.snapshot())
                            };
                            if let Ok(snapshot) = fresh {
                                worker.last_snapshot = Some(snapshot.clone());
                                let _ = events.send(Event::State(Ok(snapshot)));
                                request_repaint_if_visible(ctx, visible);
                            }
                        }
                        gpx_tool::dlog!("worker result sent: RateApplied {confirmed}");
                        if events.send(Event::RateApplied(confirmed)).is_err() {
                            return BatchOutcome {
                                stop: true,
                                force_poll: false,
                                failed: false,
                            };
                        }
                    }
                    Err(error) => {
                        failed = true;
                        send_error(events, format!("Could not set {hz} Hz: {error}"));
                    }
                }
            }
        }
        request_repaint_if_visible(ctx, visible);
    }

    if applied_any && persist {
        if let Err(error) = settings::save(&worker.settings) {
            send_error(events, error);
        }
    }

    BatchOutcome {
        stop: false,
        force_poll,
        failed,
    }
}

/// Debug-only sleep test (GPX_DEBUG_SLEEP_TEST=1, needs a debug log): open the
/// mouse once, send no requests for 40 s, then send one 300 ms battery getter.
/// A timeout means the mouse slept; an answer means it still responds in its
/// idle/power-save state.
fn debug_sleep_test(worker: &mut Worker, hid_lock: &Mutex<()>, stop: &AtomicBool) {
    let open = {
        let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
        worker.open_device()
    };
    match open {
        Ok(_) => gpx_tool::dlog!("sleep-test: device opened; no requests for 40 s"),
        Err(error) => {
            gpx_tool::dlog!("sleep-test: open failed: {error}");
            return;
        }
    }

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(40) {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }

    let result = {
        let _guard = hid_lock.lock().unwrap_or_else(|e| e.into_inner());
        worker.wake_ping()
    };
    match result {
        Ok(()) => gpx_tool::dlog!(
            "sleep-test: RESULT answer after 40 s without requests (mouse stayed reachable)"
        ),
        Err(error) if error.is_no_answer() => gpx_tool::dlog!(
            "sleep-test: RESULT timeout after 40 s without requests (mouse slept)"
        ),
        Err(error) => gpx_tool::dlog!("sleep-test: RESULT error after 40 s: {error}"),
    }
}

fn poll_loop(
    events: EventQueue,
    commands: Receiver<Command>,
    ctx: egui::Context,
    stop: Arc<AtomicBool>,
    host_engaged: Arc<AtomicBool>,
    hid_lock: Arc<Mutex<()>>,
    settings: Settings,
    controlling: bool,
    persist: bool,
    raw: RawInput,
    tray: Option<TrayHandle>,
    visible: Arc<AtomicBool>,
) {
    let force_sleep_once = debuglog::enabled()
        && env::var("GPX_DEBUG_FORCE_ASLEEP")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    let idle_override = if debuglog::enabled() {
        env::var("GPX_DEBUG_IDLE_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value >= 1)
    } else {
        None
    };
    gpx_tool::dlog!("worker idle override: {idle_override:?} s");
    let mut worker = Worker {
        settings,
        controlling,
        device: None,
        short: None,
        raw,
        pid: None,
        force_sleep_once,
        last_snapshot: None,
        last_error: None,
        last_ok: true,
        asleep: false,
        idle: false,
        last_activity: Instant::now(),
        last_path_check: Instant::now(),
        known_pids: Vec::new(),
        idle_override,
        first_open_done: false,
        reconnect_tries: 0,
        sleep_loaded: false,
        sleep_timers: None,
        timers_dirty: false,
    };
    if env::var("GPX_DEBUG_SLEEP_TEST")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        debug_sleep_test(&mut worker, &hid_lock, &stop);
        return;
    }

    let mut next_poll = Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let first = match commands.try_recv() {
            Ok(command) => Some(command),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => return,
        };

        let first = match first {
            Some(command) => command,
            None => {
                let now = Instant::now();
                if now >= next_poll {
                    if !worker.asleep && !worker.idle {
                        let sleep_after = worker.idle_seconds();
                        let stop_after = IDLE_POLL_INTERVAL
                            .min(Duration::from_secs(sleep_after));
                        let no_input = worker.last_activity.elapsed();
                        if no_input >= stop_after {
                            if no_input < Duration::from_secs(sleep_after) {
                                // Stop polling now; only change the displayed
                                // state once the configured sleep time elapsed.
                                next_poll = worker.last_activity
                                    + Duration::from_secs(sleep_after);
                                continue;
                            }
                            worker.idle = true;
                            if let Some(pid) = worker.pid {
                                worker.raw.enable(&[pid]);
                            }
                            gpx_tool::dlog!(
                                "worker idle: no mouse input for {:.1} s; polling stopped",
                                no_input.as_secs_f64()
                            );
                            if events.send(Event::Idle).is_err() {
                                return;
                            }
                            worker.update_hidden_tray(&tray, &visible);
                            request_repaint_if_visible(&ctx, &visible);
                            next_poll = Instant::now() + ASLEEP_SAFETY_INTERVAL;
                            continue;
                        }
                    }
                    let was_asleep = worker.asleep;
                    let was_idle = worker.idle;
                    let outcome = worker.poll(&hid_lock, &stop, &host_engaged);
                    if outcome.settings_changed
                        && events
                            .send(Event::Targets(worker.settings, worker.controlling))
                            .is_err()
                    {
                        return;
                    }
                    if let Some(time) = outcome.reapplied {
                        if events.send(Event::Reapplied(time)).is_err() {
                            return;
                        }
                    }
                    if let Some(error) = outcome.error {
                        send_error(&events, error);
                    }
                    if worker.timers_dirty {
                        worker.timers_dirty = false;
                        if let Some(timers) = worker.sleep_timers {
                            if events.send(Event::SleepTimers(timers)).is_err() {
                                return;
                            }
                        }
                    }
                    let ok = outcome
                        .state
                        .as_ref()
                        .map(|state| state.is_ok())
                        .unwrap_or(false);
                    if let Some(state) = outcome.state {
                        gpx_tool::dlog!("worker result sent: State ok={ok}");
                        if events.send(Event::State(state)).is_err() {
                            return;
                        }
                    }
                    if outcome.asleep && !was_asleep {
                        gpx_tool::dlog!("worker result sent: Sleeping");
                        if events.send(Event::Sleeping).is_err() {
                            return;
                        }
                    }
                    if outcome.idle && !was_idle {
                        gpx_tool::dlog!("worker result sent: Idle");
                        if events.send(Event::Idle).is_err() {
                            return;
                        }
                    }
                    worker.update_hidden_tray(&tray, &visible);
                    request_repaint_if_visible(&ctx, &visible);
                    next_poll = worker.next_check(ok);
                    continue;
                }

                let wait = next_poll.saturating_duration_since(now).min(IDLE_SLICE);
                match commands.recv_timeout(wait) {
                    Ok(command) => command,
                    Err(RecvTimeoutError::Timeout) => {
                        if worker.raw.activity() {
                            worker.last_activity =
                                worker.raw.last_activity().unwrap_or_else(Instant::now);
                            if worker.asleep || worker.idle {
                                gpx_tool::dlog!("raw input wake: polling the mouse");
                                worker.asleep = false;
                                worker.idle = false;
                                next_poll = Instant::now();
                                continue;
                            }
                        }
                        if worker.last_path_check.elapsed() >= PATH_CHECK_INTERVAL {
                            worker.last_path_check = Instant::now();
                            if worker.try_switch_to_wired() {
                                next_poll = Instant::now();
                                continue;
                            }
                        }
                        if worker.drain_reports(&hid_lock) {
                            next_poll = Instant::now();
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        };

        let batch = collect_commands(first, &commands);
        gpx_tool::dlog!("worker recv: batch of {} command(s)", batch.len());
        let outcome = handle_batch(
            &mut worker,
            batch,
            &events,
            &ctx,
            &visible,
            &stop,
            &host_engaged,
            &hid_lock,
            persist,
        );
        if outcome.stop {
            worker.raw.disable();
            return;
        }
        worker.update_hidden_tray(&tray, &visible);
        request_repaint_if_visible(&ctx, &visible);
        if outcome.force_poll {
            next_poll = Instant::now();
        } else {
            next_poll = worker.next_check(!outcome.failed);
        }
    }
}

fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if now < deadline {
        thread::sleep(deadline - now);
    }
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sorted[sorted.len() / 2]
}

fn wait_for_applied(
    events: &EventQueue,
    kind: &str,
    value: u16,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(String::from("timeout"));
        }
        match events.try_recv() {
            Ok(Event::DpiApplied(dpi)) if kind == "dpi" && u16::from(dpi) == value => return Ok(()),
            Ok(Event::RateApplied(hz)) if kind == "rate" && u16::from(hz) == value => return Ok(()),
            Ok(Event::LodApplied(lod)) if kind == "lod" && u16::from(lod) == value => return Ok(()),
            Ok(Event::Onboarded) if kind == "onboard" => return Ok(()),
            Ok(Event::Error(message)) => return Err(message),
            Ok(_) => {}
            Err(_) => thread::sleep(Duration::from_millis(2)),
        }
    }
}

fn run_selftest() {
    gpx_tool::dlog!("selftest: start");
    let events = EventQueue::new();
    let event_tx = events.clone();
    let (commands, command_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let host = Arc::new(AtomicBool::new(false));
    let lock = Arc::new(Mutex::new(()));
    let ctx = egui::Context::default();
    let thread_stop = Arc::clone(&stop);
    let raw = RawInput::start();
    let worker = thread::Builder::new()
        .name(String::from("hid-worker"))
        .spawn(move || {
            poll_loop(
                event_tx,
                command_rx,
                ctx,
                thread_stop,
                host,
                lock,
                Settings::default(),
                false,
                false,
                raw,
                None,
                Arc::new(AtomicBool::new(true)),
            )
        })
        .expect("spawn worker");

    let state_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < state_deadline {
        match events.try_recv() {
            Ok(Event::State(_)) => break,
            Ok(_) => {}
            Err(_) => thread::sleep(Duration::from_millis(2)),
        }
    }

    let mut lost = 0usize;
    let mut rate_times = Vec::new();
    let mut lod_times = Vec::new();

    {
        let sent = Instant::now();
        gpx_tool::dlog!("selftest click: dpi 800");
        let _ = commands.send(Command::ApplyDpi(800));
        match wait_for_applied(&events, "dpi", 800, Duration::from_secs(20)) {
            Ok(()) => gpx_tool::dlog!(
                "selftest result: dpi 800 applied in {:.1} ms",
                sent.elapsed().as_secs_f64() * 1000.0
            ),
            Err(error) => {
                lost += 1;
                gpx_tool::dlog!("selftest result: dpi 800 LOST ({error})");
            }
        }
        sleep_until(sent + Duration::from_secs(1));
    }

    for index in 0..10 {
        let hz = if index % 2 == 0 { 1000 } else { 2000 };
        gpx_tool::dlog!("selftest click: rate {hz} Hz (#{index})");
        let sent = Instant::now();
        let _ = commands.send(Command::ApplyRate(hz));
        match wait_for_applied(&events, "rate", hz, Duration::from_secs(20)) {
            Ok(()) => {
                let millis = sent.elapsed().as_secs_f64() * 1000.0;
                gpx_tool::dlog!("selftest result: rate {hz} Hz applied in {millis:.1} ms");
                rate_times.push(millis);
            }
            Err(error) => {
                lost += 1;
                gpx_tool::dlog!("selftest result: rate {hz} Hz LOST ({error})");
            }
        }
        sleep_until(sent + Duration::from_secs(1));
    }

    for index in 0..10 {
        let lod: u8 = if index % 2 == 0 { 1 } else { 3 };
        gpx_tool::dlog!("selftest click: lod {lod} (#{index})");
        let sent = Instant::now();
        let _ = commands.send(Command::ApplyLod(lod));
        match wait_for_applied(&events, "lod", u16::from(lod), Duration::from_secs(20)) {
            Ok(()) => {
                let millis = sent.elapsed().as_secs_f64() * 1000.0;
                gpx_tool::dlog!("selftest result: lod {lod} applied in {millis:.1} ms");
                lod_times.push(millis);
            }
            Err(error) => {
                lost += 1;
                gpx_tool::dlog!("selftest result: lod {lod} LOST ({error})");
            }
        }
        sleep_until(sent + Duration::from_secs(1));
    }

    gpx_tool::dlog!(
        "selftest summary: rate median={:.1} ms max={:.1} ms (n={}); lod median={:.1} ms max={:.1} ms (n={}); lost={lost}",
        median(&rate_times),
        rate_times.iter().copied().fold(0.0, f64::max),
        rate_times.len(),
        median(&lod_times),
        lod_times.iter().copied().fold(0.0, f64::max),
        lod_times.len()
    );

    let sent = Instant::now();
    let _ = commands.send(Command::Onboard);
    match wait_for_applied(&events, "onboard", 0, Duration::from_secs(30)) {
        Ok(()) => gpx_tool::dlog!(
            "selftest restore: onboard in {:.1} ms",
            sent.elapsed().as_secs_f64() * 1000.0
        ),
        Err(error) => gpx_tool::dlog!("selftest restore: onboard failed ({error})"),
    }

    stop.store(true, Ordering::Relaxed);
    let _ = commands.send(Command::Stop);
    let _ = worker.join();
    gpx_tool::dlog!("selftest: done");
}

#[repr(C)]
#[derive(Default)]
struct WinSystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLocalTime(system_time: *mut WinSystemTime);
}

fn local_time_string() -> String {
    let mut time = WinSystemTime::default();
    unsafe {
        GetLocalTime(&mut time);
    }
    format!("{:02}:{:02}:{:02}", time.hour, time.minute, time.second)
}
