use std::collections::VecDeque;

use crate::types::Ts;

/// Time-bucketed rolling window with O(log n) insert and O(1) median.
#[derive(Clone, Debug)]
pub struct RollingMedian {
    window_ms: i64,
    bucket_ms: i64,
    samples: VecDeque<(i64, f64)>,
    sorted: Vec<f64>,
}

impl RollingMedian {
    pub fn new(window_ms: u64, bucket_ms: u64) -> Self {
        Self {
            window_ms: window_ms as i64,
            bucket_ms: bucket_ms.max(1) as i64,
            samples: VecDeque::new(),
            sorted: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn push(&mut self, now: Ts, value: f64) {
        if !value.is_finite() {
            return;
        }
        let bucket = now.millis() / self.bucket_ms * self.bucket_ms;
        if self.samples.back().is_some_and(|back| back.0 == bucket) {
            let prev = self.samples.back().map(|b| b.1).unwrap();
            self.remove_sorted(prev);
            if let Some(back) = self.samples.back_mut() {
                back.1 = value;
            }
            self.insert_sorted(value);
            self.evict(now);
            return;
        }
        self.samples.push_back((bucket, value));
        self.insert_sorted(value);
        self.evict(now);
    }

    fn evict(&mut self, now: Ts) {
        let cutoff = now.millis().saturating_sub(self.window_ms);
        while let Some(&(ts, v)) = self.samples.front() {
            if ts < cutoff {
                self.samples.pop_front();
                self.remove_sorted(v);
            } else {
                break;
            }
        }
    }

    fn insert_sorted(&mut self, v: f64) {
        let idx = self
            .sorted
            .partition_point(|x| *x < v || (*x == v && x.to_bits() < v.to_bits()));
        self.sorted.insert(idx, v);
    }

    fn remove_sorted(&mut self, v: f64) {
        if let Ok(idx) = self
            .sorted
            .binary_search_by(|x| x.partial_cmp(&v).unwrap_or(std::cmp::Ordering::Equal))
        {
            self.sorted.remove(idx);
        }
    }

    pub fn median(&self) -> Option<f64> {
        if self.sorted.is_empty() {
            return None;
        }
        let n = self.sorted.len();
        if n % 2 == 1 {
            Some(self.sorted[n / 2])
        } else {
            Some((self.sorted[n / 2 - 1] + self.sorted[n / 2]) / 2.0)
        }
    }

    /// Median absolute deviation from the current median.
    pub fn mad(&self) -> Option<f64> {
        let med = self.median()?;
        if self.sorted.is_empty() {
            return None;
        }
        let mut devs: Vec<f64> = self.sorted.iter().map(|x| (x - med).abs()).collect();
        devs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = devs.len();
        if n % 2 == 1 {
            Some(devs[n / 2])
        } else {
            Some((devs[n / 2 - 1] + devs[n / 2]) / 2.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_and_window_evict() {
        let mut w = RollingMedian::new(1_000, 100);
        let t0 = Ts::from_millis(10_000);
        w.push(t0, 1.0);
        w.push(t0.saturating_add_millis(100), 2.0);
        w.push(t0.saturating_add_millis(200), 3.0);
        assert_eq!(w.median(), Some(2.0));
        w.push(t0.saturating_add_millis(2_000), 10.0);
        assert_eq!(w.len(), 1);
        assert_eq!(w.median(), Some(10.0));
    }

    #[test]
    fn mad_is_robust() {
        let mut w = RollingMedian::new(10_000, 1);
        let t0 = Ts::from_millis(0);
        for (i, v) in [1.0, 1.0, 1.0, 1.0, 100.0].iter().enumerate() {
            w.push(t0.saturating_add_millis(i as i64), *v);
        }
        assert_eq!(w.median(), Some(1.0));
        assert_eq!(w.mad(), Some(0.0));
    }
}
