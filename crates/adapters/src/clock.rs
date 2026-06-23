//! Clock adapters implementing [`ClockPort`] — the injected source of "now" and
//! the local timezone, so naming and the retention scheduler are deterministic.
//!
//! - [`SystemClock`] — production: the real wall clock with the host's local UTC
//!   offset (the untested boundary; exercised by integration runs).
//! - [`FixedClock`] — a deterministic clock for tests and reproducible runs
//!   (e.g. the differential-oracle conformance test).
//!
//! See `documentation/02-architecture-v2.md` §6 (determinism).

use chrono::{DateTime, FixedOffset, Local};

use mybtrfs_application::ports::ClockPort;

/// Production [`ClockPort`]: the real wall clock stamped with the host's local
/// UTC offset, so `short`/`long` timestamps render in local time.
pub struct SystemClock;

impl ClockPort for SystemClock {
    fn now(&self) -> DateTime<FixedOffset> {
        Local::now().fixed_offset()
    }
}

/// Deterministic [`ClockPort`] returning a fixed instant — for tests and
/// reproducible runs.
pub struct FixedClock {
    now: DateTime<FixedOffset>,
}

impl FixedClock {
    /// Create a clock pinned to `now` (an instant carrying its local UTC offset).
    #[must_use]
    pub fn new(now: DateTime<FixedOffset>) -> Self {
        Self { now }
    }
}

impl ClockPort for FixedClock {
    fn now(&self) -> DateTime<FixedOffset> {
        self.now
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};

    #[test]
    fn fixed_clock_returns_its_configured_instant() {
        crate::init_test_logger();
        let offset = FixedOffset::east_opt(2 * 3600).unwrap();
        let instant = offset.with_ymd_and_hms(2026, 6, 22, 19, 30, 0).unwrap();
        let clock = FixedClock::new(instant);
        assert_eq!(clock.now(), instant);
        assert_eq!(clock.now().offset().local_minus_utc(), 2 * 3600);
    }

    #[test]
    fn system_clock_returns_a_plausible_current_time() {
        crate::init_test_logger();
        // The real clock is the untested boundary; this only smoke-checks that it
        // returns a current (not epoch/zero) instant.
        assert!(SystemClock.now().year() >= 2020);
    }
}
