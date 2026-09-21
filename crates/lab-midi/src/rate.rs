// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! Coalescing rate limiter for device feedback.
//!
//! Knob sweeps produce hundreds of display updates per second; the device
//! only needs the newest one at a bounded rate (about 30/s). `Coalescer`
//! keeps the latest pending item and releases it no faster than the
//! configured interval. Time is injected so tests need no clock.

use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Coalescer<T> {
    min_interval: Duration,
    last_release: Option<Instant>,
    pending: Option<T>,
}

impl<T> Coalescer<T> {
    pub fn new(min_interval: Duration) -> Self {
        Coalescer {
            min_interval,
            last_release: None,
            pending: None,
        }
    }

    /// Offer a new item. Returns it immediately when the interval has
    /// elapsed (or nothing was sent yet); otherwise stores it as pending,
    /// replacing any older pending item.
    pub fn offer(&mut self, item: T, now: Instant) -> Option<T> {
        if self.ready(now) {
            self.last_release = Some(now);
            self.pending = None;
            Some(item)
        } else {
            self.pending = Some(item);
            None
        }
    }

    /// Release the pending item if the interval has elapsed. Call
    /// periodically (e.g. on the event-loop tick).
    pub fn poll(&mut self, now: Instant) -> Option<T> {
        if self.pending.is_some() && self.ready(now) {
            self.last_release = Some(now);
            self.pending.take()
        } else {
            None
        }
    }

    fn ready(&self, now: Instant) -> bool {
        match self.last_release {
            None => true,
            Some(last) => now.duration_since(last) >= self.min_interval,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(33);

    #[test]
    fn first_item_passes_immediately() {
        let mut c = Coalescer::new(TICK);
        let t0 = Instant::now();
        assert_eq!(c.offer("a", t0), Some("a"));
    }

    #[test]
    fn rapid_items_coalesce_to_latest() {
        let mut c = Coalescer::new(TICK);
        let t0 = Instant::now();
        assert_eq!(c.offer(1, t0), Some(1));
        // Burst within the interval: all held, only the newest survives.
        assert_eq!(c.offer(2, t0 + Duration::from_millis(5)), None);
        assert_eq!(c.offer(3, t0 + Duration::from_millis(10)), None);
        assert_eq!(c.poll(t0 + Duration::from_millis(20)), None);
        assert_eq!(c.poll(t0 + TICK), Some(3));
        // Nothing left pending.
        assert_eq!(c.poll(t0 + TICK * 2), None);
    }

    #[test]
    fn offer_after_interval_passes_and_drops_pending() {
        let mut c = Coalescer::new(TICK);
        let t0 = Instant::now();
        assert_eq!(c.offer(1, t0), Some(1));
        assert_eq!(c.offer(2, t0 + Duration::from_millis(5)), None);
        // A newer item arriving after the interval supersedes the pending
        // one entirely.
        assert_eq!(c.offer(3, t0 + TICK), Some(3));
        assert_eq!(c.poll(t0 + TICK * 2), None);
    }
}
