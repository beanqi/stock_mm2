use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Nanoseconds since Unix epoch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Ts(pub i64);

impl Ts {
    pub fn from_millis(ms: i64) -> Self {
        Self(ms.saturating_mul(1_000_000))
    }

    pub fn from_secs(secs: i64) -> Self {
        Self(secs.saturating_mul(1_000_000_000))
    }

    pub fn from_micros(us: i64) -> Self {
        Self(us.saturating_mul(1_000))
    }

    pub fn millis(self) -> i64 {
        self.0 / 1_000_000
    }

    pub fn saturating_add_millis(self, ms: i64) -> Self {
        Self(self.0.saturating_add(ms.saturating_mul(1_000_000)))
    }

    pub fn duration_since_ms(self, earlier: Self) -> i64 {
        (self.0 - earlier.0) / 1_000_000
    }

    pub fn now_system() -> Self {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self(d.as_nanos() as i64)
    }
}

pub trait Clock: Send + Sync {
    fn now(&self) -> Ts;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Ts {
        Ts::now_system()
    }
}

#[derive(Debug)]
pub struct TestClock {
    now: AtomicI64,
}

impl TestClock {
    pub fn new(start: Ts) -> Self {
        Self {
            now: AtomicI64::new(start.0),
        }
    }

    pub fn set(&self, ts: Ts) {
        self.now.store(ts.0, Ordering::SeqCst);
    }

    pub fn advance_ms(&self, ms: i64) {
        self.now
            .fetch_add(ms.saturating_mul(1_000_000), Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now(&self) -> Ts {
        Ts(self.now.load(Ordering::SeqCst))
    }
}
