use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use gpx_tool::autostart;
use gpx_tool::icon;

use super::Command;

const MENU_SHOW: &str = "gpx-tool.show";
const MENU_REFRESH: &str = "gpx-tool.refresh";
const MENU_QUIT: &str = "gpx-tool.quit";

pub const INITIAL_TOOLTIP: &str = "Featherweight | reading...";

/// A `TrayIcon` must live on a thread that pumps Windows messages, and the
/// tray has to keep updating (tooltip and icon) while the egui window is
/// hidden and eframe runs no frames. The tray therefore lives on its own
/// message-pump thread; the UI and the poll worker talk to it through this
/// handle.
#[derive(Clone)]
pub struct TrayHandle {
    updates: Sender<TrayUpdate>,
}

struct TrayUpdate {
    battery: Option<u8>,
    tooltip: String,
}

impl TrayHandle {
    pub fn update(&self, battery: Option<u8>, tooltip: &str) -> Result<(), String> {
        self.updates
            .send(TrayUpdate {
                battery,
                tooltip: tooltip.to_owned(),
            })
            .map_err(|_| String::from("the tray thread has stopped"))
    }
}

struct Tray {
    icon: TrayIcon,
    size: u32,
    last_icon: Option<(Option<u8>, bool)>,
    last_tooltip: Option<String>,
}

pub fn create(
    ctx: egui::Context,
    commands: Sender<Command>,
    window_visible: Arc<AtomicBool>,
    quitting: Arc<AtomicBool>,
    reset_to_main: Arc<AtomicBool>,
) -> Result<TrayHandle, String> {
    let (updates, update_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name(String::from("tray"))
        .spawn(move || {
            let tray = Tray::new(&ctx, commands, window_visible, quitting, reset_to_main);
            match tray {
                Ok(mut tray) => {
                    let _ = ready_tx.send(Ok(()));
                    pump(&mut tray, &update_rx);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            }
        })
        .map_err(|error| format!("could not start the tray thread: {error}"))?;

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(TrayHandle { updates }),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(String::from("the tray thread did not start in time")),
    }
}

impl Tray {
    fn new(
        ctx: &egui::Context,
        commands: Sender<Command>,
        window_visible: Arc<AtomicBool>,
        quitting: Arc<AtomicBool>,
        reset_to_main: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let size = small_icon_size();
        let light = autostart::system_uses_light_theme();
        let icon = Icon::from_rgba(icon::render_icon(size, None, light), size, size)
            .map_err(|e| format!("tray icon image is invalid: {e}"))?;

        let menu = Menu::new();
        let show = MenuItem::with_id(MENU_SHOW, "Show window", true, None);
        let refresh = MenuItem::with_id(MENU_REFRESH, "Refresh now", true, None);
        let quit = MenuItem::with_id(MENU_QUIT, "Quit", true, None);
        menu.append_items(&[&show, &refresh, &quit])
            .map_err(|e| format!("could not build tray menu: {e}"))?;

        let icon_handle = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip(INITIAL_TOOLTIP)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| format!("could not create tray icon: {e}"))?;

        let click_ctx = ctx.clone();
        let click_visible = Arc::clone(&window_visible);
        let click_quitting = Arc::clone(&quitting);
        let click_reset = Arc::clone(&reset_to_main);
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if click_quitting.load(Ordering::Relaxed) {
                    return;
                }
                if click_visible.load(Ordering::Relaxed) {
                    click_visible.store(false, Ordering::Relaxed);
                    click_ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    super::memory::schedule_trim(Arc::clone(&click_visible));
                } else {
                    show_window(&click_ctx, &click_visible, &click_reset);
                }
            }
        }));

        let menu_ctx = ctx.clone();
        let menu_visible = Arc::clone(&window_visible);
        let menu_quitting = Arc::clone(&quitting);
        let menu_reset = Arc::clone(&reset_to_main);
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id = event.id();
            if id == MENU_SHOW {
                if !menu_quitting.load(Ordering::Relaxed) {
                    show_window(&menu_ctx, &menu_visible, &menu_reset);
                }
            } else if id == MENU_REFRESH {
                let _ = commands.send(Command::Refresh);
                if menu_visible.load(Ordering::Relaxed) {
                    menu_ctx.request_repaint();
                }
            } else if id == MENU_QUIT {
                menu_quitting.store(true, Ordering::Relaxed);
                menu_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                menu_ctx.request_repaint();
            }
        }));

        Ok(Self {
            icon: icon_handle,
            size,
            last_icon: Some((None, light)),
            last_tooltip: Some(INITIAL_TOOLTIP.to_owned()),
        })
    }

    fn apply(&mut self, update: TrayUpdate) {
        if let Err(error) = self.update_icon(update.battery) {
            gpx_tool::dlog!("tray icon update failed: {error}");
        }
        if self.last_tooltip.as_deref() != Some(update.tooltip.as_str()) {
            match self.icon.set_tooltip(Some(&update.tooltip)) {
                Ok(()) => self.last_tooltip = Some(update.tooltip),
                Err(error) => {
                    gpx_tool::dlog!("tray tooltip update failed: {error}");
                }
            }
        }
    }

    /// Re-renders only when the battery percent or the taskbar theme changed.
    fn update_icon(&mut self, battery: Option<u8>) -> Result<(), String> {
        let light = autostart::system_uses_light_theme();
        let key = (battery, light);
        if self.last_icon == Some(key) {
            return Ok(());
        }
        let image = icon::render_icon(self.size, battery, light);
        let image = Icon::from_rgba(image, self.size, self.size)
            .map_err(|e| format!("tray icon image is invalid: {e}"))?;
        self.icon
            .set_icon(Some(image))
            .map_err(|e| format!("could not update tray icon: {e}"))?;
        self.last_icon = Some(key);
        Ok(())
    }
}

/// Pumps the Windows message queue of the tray thread and applies queued
/// tooltip/icon updates. The timeout bounds how long an update can wait.
fn pump(tray: &mut Tray, updates: &Receiver<TrayUpdate>) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
        PeekMessageW, QS_ALLINPUT, TranslateMessage, WM_QUIT,
    };

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        loop {
            match updates.try_recv() {
                Ok(update) => tray.apply(update),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        unsafe {
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                if msg.message == WM_QUIT {
                    return;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            MsgWaitForMultipleObjectsEx(0, std::ptr::null(), 50, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
    }
}

fn show_window(
    ctx: &egui::Context,
    window_visible: &AtomicBool,
    reset_to_main: &AtomicBool,
) {
    reset_to_main.store(true, Ordering::Relaxed);
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    window_visible.store(true, Ordering::Relaxed);
    ctx.request_repaint();
}

fn small_icon_size() -> u32 {
    use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::WindowsAndMessaging::SM_CXSMICON;

    let dpi = unsafe { GetDpiForSystem() }.max(96);
    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) };
    if size >= 8 {
        size as u32
    } else {
        ((16.0 * dpi as f32 / 96.0).round() as u32).max(8)
    }
}
