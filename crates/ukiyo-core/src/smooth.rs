//! Frame-rate independent smoothing.
//!
//! A [`Smoother`] follows its target with an exponential approach whose time
//! constant depends on the direction: `attack` while rising, `release` while
//! falling. With a time constant τ the value covers 63% of the gap in τ
//! seconds and 95% in 3τ, whatever the frame rate. τ = 0 jumps.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Smoothing {
    /// Seconds, while the target is above the value.
    pub attack: f32,
    /// Seconds, while the target is below the value.
    pub release: f32,
}

impl Smoothing {
    pub const NONE: Smoothing = Smoothing {
        attack: 0.0,
        release: 0.0,
    };

    pub fn symmetric(tau: f32) -> Self {
        Smoothing {
            attack: tau,
            release: tau,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Smoother {
    pub smoothing: Smoothing,
    value: f32,
    primed: bool,
}

impl Smoother {
    pub fn new(smoothing: Smoothing) -> Self {
        Smoother {
            smoothing,
            value: 0.0,
            primed: false,
        }
    }

    /// Starts at `value` instead of jumping to the first target.
    pub fn with_value(smoothing: Smoothing, value: f32) -> Self {
        Smoother {
            smoothing,
            value,
            primed: true,
        }
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    /// Jump straight to `value`.
    pub fn reset(&mut self, value: f32) {
        self.value = value;
        self.primed = true;
    }

    /// Advance by `dt` seconds toward `target`. The first update jumps (a
    /// scene shouldn't fade in from 0 when it loads at 3am).
    pub fn update(&mut self, target: f32, dt: f32) -> f32 {
        if !self.primed || !self.value.is_finite() {
            self.reset(target);
            return self.value;
        }
        let tau = if target > self.value {
            self.smoothing.attack
        } else {
            self.smoothing.release
        };
        self.value = approach(self.value, target, tau, dt);
        self.value
    }

    /// Close enough to `target` that another update wouldn't be visible.
    pub fn settled(&self, target: f32) -> bool {
        (self.value - target).abs() <= 1e-3 * (1.0 + target.abs())
    }
}

/// One exponential step from `value` toward `target` with time constant `tau`.
pub fn approach(value: f32, target: f32, tau: f32, dt: f32) -> f32 {
    if tau <= 0.0 || dt <= 0.0 {
        return if dt <= 0.0 && tau > 0.0 {
            value
        } else {
            target
        };
    }
    let k = 1.0 - (-dt / tau).exp();
    value + (target - value) * k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_update_jumps() {
        let mut s = Smoother::new(Smoothing::symmetric(1.0));
        assert_eq!(s.update(0.7, 0.016), 0.7);
    }

    #[test]
    fn one_time_constant_is_63_percent() {
        let mut s = Smoother::with_value(Smoothing::symmetric(0.5), 0.0);
        s.update(1.0, 0.5);
        assert!((s.value() - 0.632).abs() < 1e-3, "{}", s.value());
    }

    #[test]
    fn frame_rate_independent() {
        let mut a = Smoother::with_value(Smoothing::symmetric(0.3), 0.0);
        let mut b = a;
        for _ in 0..60 {
            a.update(1.0, 1.0 / 60.0);
        }
        for _ in 0..10 {
            b.update(1.0, 0.1);
        }
        assert!(
            (a.value() - b.value()).abs() < 1e-4,
            "{} vs {}",
            a.value(),
            b.value()
        );
    }

    #[test]
    fn attack_and_release_differ() {
        let sm = Smoothing {
            attack: 0.05,
            release: 2.0,
        };
        let mut s = Smoother::with_value(sm, 0.0);
        s.update(1.0, 0.1);
        assert!(s.value() > 0.85, "fast attack: {}", s.value());
        s.reset(1.0);
        s.update(0.0, 0.1);
        assert!(s.value() > 0.94, "slow release: {}", s.value());
    }

    #[test]
    fn zero_tau_jumps_and_zero_dt_holds() {
        let mut s = Smoother::with_value(Smoothing::NONE, 0.0);
        assert_eq!(s.update(0.4, 0.016), 0.4);
        let mut s = Smoother::with_value(Smoothing::symmetric(1.0), 0.2);
        assert_eq!(s.update(1.0, 0.0), 0.2);
    }

    #[test]
    fn settles() {
        let mut s = Smoother::with_value(Smoothing::symmetric(0.1), 0.0);
        for _ in 0..200 {
            s.update(1.0, 1.0 / 60.0);
        }
        assert!(s.settled(1.0));
        assert!(!Smoother::with_value(Smoothing::symmetric(0.1), 0.0).settled(1.0));
    }

    #[test]
    fn recovers_from_nan() {
        let mut s = Smoother::with_value(Smoothing::symmetric(1.0), f32::NAN);
        assert_eq!(s.update(0.5, 0.016), 0.5);
    }
}
