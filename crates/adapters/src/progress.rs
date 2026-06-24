//! Terminal progress reporting adapter: [`IndicatifProgress`] — a live
//! [`ProgressPort`](mybtrfs_application::ports::ProgressPort) implementation
//! that displays braille spinners (for indeterminate steps) and count bars
//! (for deletion runs where the total is known). All methods are thread-safe;
//! [`report_bytes`](IndicatifProgress::report_bytes) is called from the
//! transfer adapter's byte-counting thread.

use std::sync::Mutex;
use std::time::Duration;

use indicatif::{HumanBytes, ProgressBar, ProgressStyle};
use mybtrfs_application::ports::ProgressPort;

/// Terminal progress reporter backed by `indicatif`. Manages one active
/// indicator at a time (the last call to `start_*` wins). Calling `start_*`
/// while another indicator is active silently finishes the previous one first.
pub struct IndicatifProgress {
    /// The currently active indicator, if any.
    bar: Mutex<Option<ProgressBar>>,
}

impl IndicatifProgress {
    /// Create a new progress reporter (inactive until `start_spinner` or
    /// `start_bar` is called).
    #[must_use]
    pub fn new() -> Self {
        Self {
            bar: Mutex::new(None),
        }
    }
}

impl Default for IndicatifProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressPort for IndicatifProgress {
    fn start_spinner(&self, msg: &str) {
        let bar = ProgressBar::new_spinner();
        // Professional spinner with rotating block animation
        bar.set_style(
            ProgressStyle::with_template("{spinner} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner())
                .tick_strings(&["▰▱▱▱▱", "▰▰▱▱▱", "▰▰▰▱▱", "▰▰▰▰▱", "▰▰▰▰▰"]),
        );
        bar.set_message(msg.to_owned());
        bar.enable_steady_tick(Duration::from_millis(200));
        replace_bar(&self.bar, bar);
    }

    fn start_bar(&self, msg: &str, total: u64) {
        let bar = ProgressBar::new(total);
        // Professional progress bar with block characters and percentage
        bar.set_style(
            ProgressStyle::with_template("{msg}\n{bar:40.cyan/blue} {percent}% complete")
                .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );
        bar.set_message(msg.to_owned());
        replace_bar(&self.bar, bar);
    }

    fn advance_bar(&self, n: u64) {
        with_bar(&self.bar, |b| b.inc(n));
    }

    fn report_bytes(&self, total_bytes: u64, bytes_per_sec: u64) {
        with_bar(&self.bar, |b| {
            b.set_message(format!(
                "Transferring… {} · {}/s",
                HumanBytes(total_bytes),
                HumanBytes(bytes_per_sec),
            ));
        });
    }

    fn finish(&self, msg: &str) {
        if let Ok(mut guard) = self.bar.lock() {
            if let Some(bar) = guard.take() {
                if msg.is_empty() {
                    bar.finish_and_clear();
                } else {
                    bar.finish_with_message(msg.to_owned());
                }
            }
        }
    }
}

/// Replace the active indicator with `new_bar`, finishing the previous one
/// silently if one exists.
fn replace_bar(slot: &Mutex<Option<ProgressBar>>, new_bar: ProgressBar) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(old) = guard.take() {
            old.finish_and_clear();
        }
        *guard = Some(new_bar);
    }
}

/// Call `f` on the active indicator if one exists; no-op otherwise.
fn with_bar(slot: &Mutex<Option<ProgressBar>>, f: impl FnOnce(&ProgressBar)) {
    if let Ok(guard) = slot.lock() {
        if let Some(bar) = guard.as_ref() {
            f(bar);
        }
    }
}
