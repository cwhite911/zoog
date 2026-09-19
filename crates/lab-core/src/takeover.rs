//! Soft takeover (value-scaling pickup) for absolute hardware controls.
//!
//! The MiniLab 3 encoders and faders send absolute 0..=127 in both captured
//! modes, so after a preset change the knob position rarely matches the
//! parameter value. Plain crossing-pickup left a dead zone the user had to
//! hunt through (reported on hardware), so this uses value scaling instead:
//! the very first movement responds, with knob travel scaled into the
//! value's remaining range so knob and value converge at the extremes.
//! Once they meet, the control latches and tracks 1:1. Values never jump.

/// Pickup state for one absolute control.
#[derive(Debug, Default, Clone)]
pub struct SoftTakeover {
    latched: bool,
    last_cc: Option<u8>,
}

/// Snap-and-latch threshold: half a CC step.
const SNAP: f64 = 0.5 / 127.0;

impl SoftTakeover {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the latch (call when the underlying parameter changed without
    /// the hardware moving, e.g. preset load or an on-screen drag).
    pub fn release(&mut self) {
        self.latched = false;
        self.last_cc = None;
    }

    /// Feed one absolute CC value; `target` is the parameter's current
    /// normalized value (0.0..=1.0). Returns the normalized value to apply:
    /// the knob position once latched, otherwise a scaled step from
    /// `target` toward the knob's direction of travel.
    ///
    /// The first message after a release only records the reference
    /// position (direction is unknowable from one point) and returns
    /// `None`.
    pub fn update(&mut self, cc: u8, target: f64) -> Option<f64> {
        let position = cc as f64 / 127.0;
        let last = self.last_cc.replace(cc);

        if self.latched {
            return Some(position);
        }

        // Landing on the value (within half a CC step) latches directly.
        if (position - target).abs() <= SNAP {
            self.latched = true;
            return Some(position);
        }

        let last_pos = match last {
            Some(last) => last as f64 / 127.0,
            None => return None,
        };

        // Sweeping across the value latches too.
        if (last_pos < target && target <= position) || (position <= target && target < last_pos) {
            self.latched = true;
            return Some(position);
        }

        // Value scaling: map the knob's remaining travel onto the value's
        // remaining range, in the direction of movement.
        let scaled = if position > last_pos {
            let knob_room = 1.0 - last_pos;
            if knob_room <= f64::EPSILON {
                return None;
            }
            target + (position - last_pos) * (1.0 - target) / knob_room
        } else if position < last_pos {
            let knob_room = last_pos;
            if knob_room <= f64::EPSILON {
                return None;
            }
            target - (last_pos - position) * target / knob_room
        } else {
            return None;
        };

        let scaled = scaled.clamp(0.0, 1.0);
        if (position - scaled).abs() <= SNAP {
            self.latched = true;
        }
        Some(scaled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_message_only_sets_the_reference() {
        let mut t = SoftTakeover::new();
        assert_eq!(t.update(10, 0.5), None);
    }

    #[test]
    fn responds_immediately_without_jumping() {
        let mut t = SoftTakeover::new();
        // Param at 0.5, knob way down at 10.
        assert_eq!(t.update(10, 0.5), None);
        // Small move up: value moves up a little from 0.5, no jump to 0.09.
        let v = t.update(12, 0.5).expect("scaled response");
        assert!(v > 0.5, "must move in the knob's direction, got {v}");
        assert!(v < 0.52, "must be a scaled step, got {v}");
    }

    #[test]
    fn converges_at_the_extremes() {
        let mut t = SoftTakeover::new();
        assert_eq!(t.update(10, 0.5), None);
        // Sweep straight to the top: value must arrive at 1.0 exactly.
        let v = t.update(127, 0.5).unwrap();
        assert!((v - 1.0).abs() < 1e-9, "got {v}");
        // And now it is latched: tracks 1:1.
        let v = t.update(64, 0.9).unwrap();
        assert!((v - 64.0 / 127.0).abs() < 1e-9);
    }

    #[test]
    fn converges_downward_too() {
        let mut t = SoftTakeover::new();
        assert_eq!(t.update(100, 0.3), None);
        let v = t.update(0, 0.3).unwrap();
        assert!(v.abs() < 1e-9, "got {v}");
    }

    #[test]
    fn crossing_the_value_latches_and_tracks() {
        let mut t = SoftTakeover::new();
        assert_eq!(t.update(40, 0.5), None);
        let v = t.update(70, 0.5).expect("latch on crossing");
        assert!((v - 70.0 / 127.0).abs() < 1e-9);
        // Latched: follows even when moving away.
        assert!(t.update(10, 0.5).is_some());
    }

    #[test]
    fn close_match_latches_immediately() {
        let mut t = SoftTakeover::new();
        // 64/127 = 0.5039..., within half a step of 0.5; even a first
        // message may snap.
        let v = t.update(64, 0.5).unwrap();
        assert!((v - 64.0 / 127.0).abs() < 1e-9);
    }

    #[test]
    fn scaled_steps_converge_over_a_sweep() {
        let mut t = SoftTakeover::new();
        let mut value = 0.8;
        assert_eq!(t.update(20, value), None);
        // Sweep upward one step at a time; the gap between knob position
        // and value must shrink monotonically until latch.
        let mut gap = (20.0 / 127.0f64 - value).abs();
        for cc in 21..=127 {
            if let Some(v) = t.update(cc, value) {
                value = v;
            }
            let new_gap = (cc as f64 / 127.0 - value).abs();
            assert!(new_gap <= gap + 1e-9, "gap grew at cc={cc}");
            gap = new_gap;
        }
        assert!((value - 1.0).abs() < 1e-9);
    }

    #[test]
    fn release_requires_a_new_reference() {
        let mut t = SoftTakeover::new();
        assert_eq!(t.update(40, 0.5), None);
        assert!(t.update(70, 0.5).is_some());
        t.release();
        assert_eq!(t.update(69, 0.9), None);
        // Second message scales again instead of jumping.
        let v = t.update(68, 0.9).unwrap();
        assert!(v < 0.9 && v > 0.88, "got {v}");
    }
}
