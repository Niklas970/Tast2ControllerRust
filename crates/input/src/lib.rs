//! Mouse-to-stick engine.
//!
//! Pure logic without any platform code: raw mouse deltas go in (with a millisecond timestamp),
//! a stick position in the range -1.0..=1.0 comes out. The model is velocity based:
//! the mouse speed over a short window is shaped by a response curve, then a *radial*
//! anti-deadzone lifts small movements over the game's own stick deadzone without bending
//! the aim direction (a per-axis deadzone fix would snap aim to the axes).
//!
//! Screen convention: +x is right, +y is down (like the PlayStation stick).

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Stick position, each axis in -1.0..=1.0.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stick {
    pub x: f32,
    pub y: f32,
}

impl Stick {
    pub const CENTER: Stick = Stick { x: 0.0, y: 0.0 };

    pub fn magnitude(self) -> f32 {
        self.x.hypot(self.y)
    }
}

/// Tunable parameters of one mouse profile. All fields have defaults, so a TOML file may
/// contain only the values that differ.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MouseConfig {
    /// Stick deflection per mouse count per millisecond (before curve).
    pub sensitivity: f32,
    /// Extra multiplier for the horizontal axis.
    pub sensitivity_x: f32,
    /// Extra multiplier for the vertical axis.
    pub sensitivity_y: f32,
    pub invert_y: bool,
    /// Response curve exponent. 1.0 is linear, above 1.0 gives finer control of small movements.
    pub curve: f32,
    /// Extra boost for fast flicks (0.0 = off). Applied after the curve.
    pub acceleration: f32,
    /// Output magnitude at which any movement starts, to jump over the game's deadzone (0.0 - 0.9).
    pub anti_deadzone: f32,
    /// Maximum stick magnitude (0.1 - 1.0).
    pub max_deflection: f32,
    /// Movement is averaged over this window in ms. Larger is smoother, smaller is more direct.
    pub smoothing_ms: f32,
    /// Speed multiplier while aim-down-sights is held (1.0 = no change).
    pub ads_multiplier: f32,
    /// Movements slower than this (counts per ms) are treated as noise; 0.0 = off.
    pub noise_floor: f32,
}

impl Default for MouseConfig {
    fn default() -> Self {
        Self {
            sensitivity: 0.08,
            sensitivity_x: 1.0,
            sensitivity_y: 1.0,
            invert_y: false,
            curve: 1.3,
            acceleration: 0.0,
            anti_deadzone: 0.12,
            max_deflection: 1.0,
            smoothing_ms: 12.0,
            ads_multiplier: 0.6,
            noise_floor: 0.0,
        }
    }
}

impl MouseConfig {
    /// Clamps every value into a usable range, so bad config values can never produce NaN or
    /// a stuck stick.
    pub fn sanitized(mut self) -> Self {
        fn fix(v: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                fallback
            }
        }
        let d = Self::default();
        self.sensitivity = fix(self.sensitivity, 0.0, 10.0, d.sensitivity);
        self.sensitivity_x = fix(self.sensitivity_x, 0.0, 10.0, 1.0);
        self.sensitivity_y = fix(self.sensitivity_y, 0.0, 10.0, 1.0);
        self.curve = fix(self.curve, 0.5, 4.0, d.curve);
        self.acceleration = fix(self.acceleration, 0.0, 10.0, 0.0);
        self.anti_deadzone = fix(self.anti_deadzone, 0.0, 0.9, d.anti_deadzone);
        self.max_deflection = fix(self.max_deflection, 0.1, 1.0, 1.0);
        self.smoothing_ms = fix(self.smoothing_ms, 1.0, 200.0, d.smoothing_ms);
        self.ads_multiplier = fix(self.ads_multiplier, 0.05, 5.0, d.ads_multiplier);
        self.noise_floor = fix(self.noise_floor, 0.0, 5.0, 0.0);
        self
    }
}

/// Converts raw mouse movement into a stick position.
pub struct MouseEngine {
    cfg: MouseConfig,
    /// (timestamp ms, dx, dy)
    samples: VecDeque<(u64, i32, i32)>,
    ads: bool,
}

impl MouseEngine {
    pub fn new(cfg: MouseConfig) -> Self {
        Self {
            cfg: cfg.sanitized(),
            samples: VecDeque::new(),
            ads: false,
        }
    }

    pub fn set_config(&mut self, cfg: MouseConfig) {
        self.cfg = cfg.sanitized();
    }

    pub fn config(&self) -> &MouseConfig {
        &self.cfg
    }

    /// Switches aim-down-sights mode (applies `ads_multiplier`).
    pub fn set_ads(&mut self, on: bool) {
        self.ads = on;
    }

    /// Forgets all movement (call when input capture is released).
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Adds a raw mouse movement that happened at `now_ms` (monotonic milliseconds).
    pub fn push(&mut self, now_ms: u64, dx: i32, dy: i32) {
        if dx != 0 || dy != 0 {
            self.samples.push_back((now_ms, dx, dy));
        }
    }

    /// Whether the stick can still be off center without new input (the movement window has
    /// not run out yet), so the caller should keep ticking.
    pub fn is_active(&self) -> bool {
        !self.samples.is_empty()
    }

    /// Current stick position at `now_ms`.
    pub fn tick(&mut self, now_ms: u64) -> Stick {
        let window = self.cfg.smoothing_ms.max(1.0);
        let window_ms = window.ceil() as u64;
        while let Some(&(t, _, _)) = self.samples.front() {
            if now_ms.saturating_sub(t) >= window_ms {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        if self.samples.is_empty() {
            return Stick::CENTER;
        }
        let (sx, sy) = self
            .samples
            .iter()
            .fold((0i64, 0i64), |(ax, ay), &(_, x, y)| {
                (ax + i64::from(x), ay + i64::from(y))
            });
        // counts per millisecond over the full window
        let vx = sx as f32 / window;
        let vy = sy as f32 / window;
        self.shape(vx, vy)
    }

    fn shape(&self, vx: f32, vy: f32) -> Stick {
        let c = &self.cfg;
        let ads = if self.ads { c.ads_multiplier } else { 1.0 };
        let gain = c.sensitivity * ads;
        let dx = vx * gain * c.sensitivity_x;
        let mut dy = vy * gain * c.sensitivity_y;
        if c.invert_y {
            dy = -dy;
        }
        let speed = (vx.hypot(vy)) * 1.0;
        if speed < c.noise_floor {
            return Stick::CENTER;
        }
        let m = dx.hypot(dy);
        if m <= f32::EPSILON {
            return Stick::CENTER;
        }
        // response curve, then optional flick boost
        let curved = m.powf(c.curve);
        let boosted = curved * (1.0 + c.acceleration * curved);
        let shaped = boosted.min(1.0);
        // radial anti-deadzone: start at `anti_deadzone`, end at max_deflection
        let out = (c.anti_deadzone + (1.0 - c.anti_deadzone) * shaped).min(c.max_deflection);
        let k = out / m;
        Stick {
            x: (dx * k).clamp(-1.0, 1.0),
            y: (dy * k).clamp(-1.0, 1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> MouseConfig {
        MouseConfig {
            smoothing_ms: 10.0,
            curve: 1.0,
            anti_deadzone: 0.0,
            ads_multiplier: 0.5,
            ..MouseConfig::default()
        }
    }

    #[test]
    fn idle_is_centered() {
        let mut e = MouseEngine::new(cfg());
        assert_eq!(e.tick(0), Stick::CENTER);
        assert!(!e.is_active());
    }

    #[test]
    fn linear_response_matches_sensitivity() {
        let mut e = MouseEngine::new(MouseConfig {
            sensitivity: 0.1,
            ..cfg()
        });
        // 20 counts over a 10 ms window = 2 counts/ms -> 0.2 deflection
        e.push(0, 20, 0);
        let s = e.tick(1);
        assert!((s.x - 0.2).abs() < 1e-5, "{s:?}");
        assert!(s.y.abs() < 1e-6);
    }

    #[test]
    fn stick_returns_to_center_after_the_window() {
        let mut e = MouseEngine::new(cfg());
        e.push(0, 30, 10);
        assert!(e.tick(5).magnitude() > 0.0);
        assert_eq!(e.tick(10), Stick::CENTER);
        assert!(!e.is_active());
    }

    #[test]
    fn anti_deadzone_is_radial_and_keeps_direction() {
        let mut e = MouseEngine::new(MouseConfig {
            anti_deadzone: 0.3,
            sensitivity: 0.01,
            ..cfg()
        });
        // tiny diagonal movement, direction 3:4
        e.push(0, 3, 4);
        let s = e.tick(1);
        assert!(s.magnitude() >= 0.3, "must clear the game deadzone: {s:?}");
        let angle_in = (4.0f32).atan2(3.0);
        let angle_out = s.y.atan2(s.x);
        assert!(
            (angle_in - angle_out).abs() < 1e-4,
            "direction must be preserved"
        );
    }

    #[test]
    fn output_never_exceeds_max_deflection() {
        let mut e = MouseEngine::new(MouseConfig {
            sensitivity: 1.0,
            max_deflection: 0.8,
            ..cfg()
        });
        e.push(0, 500, -500);
        let s = e.tick(1);
        assert!(s.magnitude() <= 0.8 + 1e-5, "{s:?}");
    }

    #[test]
    fn ads_scales_speed_down() {
        let mut a = MouseEngine::new(MouseConfig {
            sensitivity: 0.1,
            ..cfg()
        });
        let mut b = MouseEngine::new(MouseConfig {
            sensitivity: 0.1,
            ..cfg()
        });
        b.set_ads(true);
        a.push(0, 20, 0);
        b.push(0, 20, 0);
        let (sa, sb) = (a.tick(1).x, b.tick(1).x);
        assert!((sb - sa * 0.5).abs() < 1e-5, "{sa} vs {sb}");
    }

    #[test]
    fn higher_curve_is_finer_for_small_movements() {
        let linear = MouseConfig {
            curve: 1.0,
            sensitivity: 0.1,
            ..cfg()
        };
        let curved = MouseConfig {
            curve: 2.0,
            sensitivity: 0.1,
            ..cfg()
        };
        let (mut a, mut b) = (MouseEngine::new(linear), MouseEngine::new(curved));
        a.push(0, 20, 0);
        b.push(0, 20, 0);
        assert!(b.tick(1).x < a.tick(1).x);
    }

    #[test]
    fn invert_y_flips_vertical_only() {
        let mut e = MouseEngine::new(MouseConfig {
            invert_y: true,
            sensitivity: 0.1,
            ..cfg()
        });
        e.push(0, 10, 10);
        let s = e.tick(1);
        assert!(s.x > 0.0 && s.y < 0.0, "{s:?}");
    }

    #[test]
    fn noise_floor_ignores_tiny_movement() {
        let mut e = MouseEngine::new(MouseConfig {
            noise_floor: 0.5,
            ..cfg()
        });
        e.push(0, 1, 0); // 0.1 counts/ms
        assert_eq!(e.tick(1), Stick::CENTER);
    }

    #[test]
    fn bad_config_values_are_sanitized() {
        let c = MouseConfig {
            curve: f32::NAN,
            smoothing_ms: -5.0,
            max_deflection: 9.0,
            ..MouseConfig::default()
        }
        .sanitized();
        assert!(c.curve.is_finite() && c.smoothing_ms >= 1.0 && c.max_deflection <= 1.0);
    }

    #[test]
    fn partial_toml_uses_defaults() {
        let c: MouseConfig = toml::from_str("sensitivity = 0.2\ninvert_y = true").unwrap();
        assert_eq!(c.sensitivity, 0.2);
        assert!(c.invert_y);
        assert_eq!(c.curve, MouseConfig::default().curve);
        assert!(toml::from_str::<MouseConfig>("unknown = 1").is_err());
    }
}
