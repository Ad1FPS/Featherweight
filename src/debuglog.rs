use std::env;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Sink {
    file: File,
    start: Instant,
}

static SINK: OnceLock<Option<Mutex<Sink>>> = OnceLock::new();

pub fn path() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("gpx-gui-debug.log")))
        .unwrap_or_else(|| PathBuf::from("gpx-gui-debug.log"))
}

fn env_enabled() -> bool {
    env::var("GPX_DEBUG")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn build() -> Option<Mutex<Sink>> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path())
        .ok()?;
    Some(Mutex::new(Sink {
        file,
        start: Instant::now(),
    }))
}

pub fn init(force: bool) {
    let _ = SINK.get_or_init(|| {
        if force || env_enabled() {
            build()
        } else {
            None
        }
    });
}

/// True when a debug log sink is active (GPX_DEBUG=1 or --debug-log).
pub fn enabled() -> bool {
    matches!(SINK.get(), Some(Some(_)))
}

pub fn log(args: fmt::Arguments<'_>) {
    let sink = SINK.get_or_init(|| if env_enabled() { build() } else { None });
    let Some(sink) = sink else {
        return;
    };
    let Ok(mut sink) = sink.lock() else {
        return;
    };
    let millis = sink.start.elapsed().as_secs_f64() * 1000.0;
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("?");
    let _ = writeln!(sink.file, "{millis:12.3} [{name}] {args}");
    let _ = sink.file.flush();
}

pub fn record_always(line: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path()) {
        let _ = writeln!(file, "{line}");
    }
}

pub fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        $crate::debuglog::log(format_args!($($arg)*))
    };
}
