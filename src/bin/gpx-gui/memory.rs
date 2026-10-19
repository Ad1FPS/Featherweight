use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

static TRIM_PENDING: AtomicBool = AtomicBool::new(false);

/// Asks Windows to trim this process's working set once, about two seconds
/// after the window was hidden. The visibility flag is checked again right
/// before trimming, so a window that was shown again is never trimmed.
pub fn schedule_trim(window_visible: Arc<AtomicBool>) {
    if TRIM_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(2));
        if !window_visible.load(Ordering::Relaxed) {
            let trimmed = unsafe {
                use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
                use windows_sys::Win32::System::Threading::GetCurrentProcess;
                K32EmptyWorkingSet(GetCurrentProcess()) != 0
            };
            gpx_tool::dlog!("empty working set after hide: {trimmed}");
        }
        TRIM_PENDING.store(false, Ordering::SeqCst);
    });
}
