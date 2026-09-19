//! Soft takeover (pickup) for absolute hardware controls.
//!
//! The MiniLab 3 encoders and faders send absolute 0..=127 in both captured
//! modes, so after a preset change the knob position rarely matches the
//! parameter value. A control stays inert until its position reaches or
//! crosses the parameter's current value, then latches and tracks.

/// Pickup state for one absolute control.
#[derive(Debug, Default, Clone)]
pub struct SoftTakeover {
    latched: bool,
    last_cc: Option<u8>,
}

impl SoftTakeover {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the latch (call when the underlying parameter changed without
    /// the hardware moving, e.g. preset load).
    pub fn release(&mut self) {
        self.latched = false;
        self.last_cc = None;
    }

    /// Feed one absolute CC value; `target` is the parameter's current
    /// normalized value (0.0..=1.0). Returns the normalized value to apply,
    /// or `None` while the control has not picked up the parameter yet.
    pub fn update(&mut self, cc: u8, target: f64) -> Option<f64> {
        let position = cc as f64 / 127.0;
        if self.latched {
            self.last_cc = Some(cc);
            return Some(position);
        }

        // Latch when the knob lands on the value (within half a CC step) or
        // sweeps across it between two consecutive messages.
        let close_enough = (position - target).abs() <= 0.5 / 127.0;
        let crossed = match self.last_cc {
            Some(last) => {
                let last_pos = last as f64 / 127.0;
                (last_pos < target && target <= position)
                    || (position <= target && target < last_pos)
            }
            None => false,
        };
        self.last_cc = Some(cc);

        if close_enough || crossed {
            self.latched = true;
            Some(position)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_jump_before_pickup() {
        let mut t = SoftTakeover::new();
        // Param sits at 0.5; knob is way down at 10.
        assert_eq!(t.update(10, 0.5), None);
        assert_eq!(t.update(20, 0.5), None);
    }

    #[test]
    fn latches_on_cross_and_tracks() {
        let mut t = SoftTakeover::new();
        assert_eq!(t.update(40, 0.5), None);
        // Sweeping up through the target latches.
        let v = t.update(70, 0.5).expect("should latch on crossing");
        assert!((v - 70.0 / 127.0).abs() < 1e-9);
        // Latched: follows even when moving away.
        assert!(t.update(10, 0.5).is_some());
    }

    #[test]
    fn latches_on_close_match() {
        let mut t = SoftTakeover::new();
        // 64/127 = 0.5039..., within half a step of 0.5.
        assert!(t.update(64, 0.5).is_some());
    }

    #[test]
    fn latches_on_downward_cross() {
        let mut t = SoftTakeover::new();
        assert_eq!(t.update(100, 0.5), None);
        assert!(t.update(30, 0.5).is_some());
    }

    #[test]
    fn release_requires_new_pickup() {
        let mut t = SoftTakeover::new();
        assert_eq!(t.update(40, 0.5), None);
        assert!(t.update(70, 0.5).is_some());
        t.release();
        // New param target far from knob: inert again.
        assert_eq!(t.update(69, 0.1), None);
        // First message after release cannot claim a "cross" from stale
        // state.
        assert_eq!(t.update(68, 0.1), None);
        assert!(t.update(12, 0.1).is_some());
    }
}
