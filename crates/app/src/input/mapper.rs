//! Turns keyboard and mouse input into a controller state, according to the active profile.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use tast2controller_input::MouseEngine;

use super::keys::{self, InputId};
use crate::config::{Binding, Config, MouseConfig, MouseTarget, Profile};
use tast2controller_core::controller::{button, ControllerState, TOUCHPAD_HEIGHT, TOUCHPAD_WIDTH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stick {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
enum Action {
    Button(u32),
    L2(f32),
    R2(f32),
    Stick(Stick, Dir, f32),
    StickScale(Stick, f32),
    Touch(u16, u16),
    Profile(usize),
    ProfileNext,
}

#[derive(Clone, Debug)]
struct Bound {
    action: Action,
    toggle: bool,
    turbo_hz: Option<f32>,
}

struct CompiledProfile {
    name: String,
    binds: HashMap<InputId, Bound>,
    mouse: MouseConfig,
    normalize_diagonals: bool,
}

fn parse_action(
    name: &str,
    value: Option<f32>,
    x: Option<u16>,
    y: Option<u16>,
    profile: Option<&str>,
    profiles: &[Profile],
) -> Result<Action> {
    let v = value.unwrap_or(1.0).clamp(0.0, 1.0);
    let n = name.trim().to_ascii_lowercase().replace('-', "_");
    let btn = |b| Ok(Action::Button(b));
    let profile_index = |p: &str| {
        profiles
            .iter()
            .position(|q| q.name.eq_ignore_ascii_case(p))
            .ok_or_else(|| anyhow!("unknown profile {p:?}"))
    };
    match n.as_str() {
        "cross" | "x" => btn(button::CROSS),
        "circle" | "moon" => btn(button::CIRCLE),
        "square" | "box" => btn(button::SQUARE),
        "triangle" | "pyramid" => btn(button::TRIANGLE),
        "dpad_up" => btn(button::DPAD_UP),
        "dpad_down" => btn(button::DPAD_DOWN),
        "dpad_left" => btn(button::DPAD_LEFT),
        "dpad_right" => btn(button::DPAD_RIGHT),
        "l1" => btn(button::L1),
        "r1" => btn(button::R1),
        "l3" => btn(button::L3),
        "r3" => btn(button::R3),
        "options" | "start" => btn(button::OPTIONS),
        "share" | "create" => btn(button::SHARE),
        "touchpad" | "touchpad_click" => btn(button::TOUCHPAD),
        "ps" | "home" => btn(button::PS),
        "l2" => Ok(Action::L2(v)),
        "r2" => Ok(Action::R2(v)),
        "left_stick_up" => Ok(Action::Stick(Stick::Left, Dir::Up, v)),
        "left_stick_down" => Ok(Action::Stick(Stick::Left, Dir::Down, v)),
        "left_stick_left" => Ok(Action::Stick(Stick::Left, Dir::Left, v)),
        "left_stick_right" => Ok(Action::Stick(Stick::Left, Dir::Right, v)),
        "right_stick_up" => Ok(Action::Stick(Stick::Right, Dir::Up, v)),
        "right_stick_down" => Ok(Action::Stick(Stick::Right, Dir::Down, v)),
        "right_stick_left" => Ok(Action::Stick(Stick::Right, Dir::Left, v)),
        "right_stick_right" => Ok(Action::Stick(Stick::Right, Dir::Right, v)),
        "left_stick_scale" => Ok(Action::StickScale(
            Stick::Left,
            value.unwrap_or(0.5).clamp(0.0, 1.0),
        )),
        "right_stick_scale" => Ok(Action::StickScale(
            Stick::Right,
            value.unwrap_or(0.5).clamp(0.0, 1.0),
        )),
        "touch" => Ok(Action::Touch(
            x.unwrap_or(TOUCHPAD_WIDTH / 2).min(TOUCHPAD_WIDTH - 1),
            y.unwrap_or(TOUCHPAD_HEIGHT / 2).min(TOUCHPAD_HEIGHT - 1),
        )),
        "touch_left" => Ok(Action::Touch(TOUCHPAD_WIDTH / 4, TOUCHPAD_HEIGHT / 2)),
        "touch_right" => Ok(Action::Touch(TOUCHPAD_WIDTH * 3 / 4, TOUCHPAD_HEIGHT / 2)),
        "profile_next" => Ok(Action::ProfileNext),
        "profile" => Ok(Action::Profile(profile_index(
            profile.context("the profile action needs `profile = \"name\"`")?,
        )?)),
        other => {
            if let Some(p) = other.strip_prefix("profile:") {
                return Ok(Action::Profile(profile_index(p)?));
            }
            bail!("unknown action {name:?}")
        }
    }
}

fn compile(cfg: &Config) -> Result<Vec<CompiledProfile>> {
    if cfg.profiles.is_empty() {
        bail!("the config defines no [[profile]]");
    }
    let mut out = Vec::new();
    for p in &cfg.profiles {
        let mut binds = HashMap::new();
        for (input, binding) in &p.bind {
            let id = keys::parse(input)
                .ok_or_else(|| anyhow!("profile {:?}: unknown input name {input:?}", p.name))?;
            let bound = match binding {
                Binding::Simple(a) => Bound {
                    action: parse_action(a, None, None, None, None, &cfg.profiles)?,
                    toggle: false,
                    turbo_hz: None,
                },
                Binding::Detailed(d) => Bound {
                    action: parse_action(
                        &d.action,
                        d.value,
                        d.x,
                        d.y,
                        d.profile.as_deref(),
                        &cfg.profiles,
                    )
                    .with_context(|| format!("profile {:?}, input {input:?}", p.name))?,
                    toggle: d.toggle,
                    turbo_hz: d.turbo_hz.filter(|h| *h > 0.0),
                },
            };
            binds.insert(id, bound);
        }
        out.push(CompiledProfile {
            name: p.name.clone(),
            binds,
            mouse: p.mouse.clone(),
            normalize_diagonals: p.normalize_diagonals,
        });
    }
    Ok(out)
}

pub struct Mapper {
    profiles: Vec<CompiledProfile>,
    active: usize,
    /// physically held inputs and when they went down
    held: HashMap<InputId, Instant>,
    /// toggle-bound inputs currently latched on
    toggled: HashSet<InputId>,
    /// wheel "presses" and when they end
    wheel_until: HashMap<InputId, Instant>,
    wheel_press: Duration,
    mouse_samples: VecDeque<(Instant, i32, i32)>,
    engine: MouseEngine,
    epoch: Instant,
    touch_id_next: u8,
    touch: Option<(i8, u16, u16)>,
}

pub enum Event {
    Down(InputId),
    Up(InputId),
    Wheel(InputId),
    MouseMove(i32, i32),
}

impl Mapper {
    pub fn new(cfg: &Config) -> Result<Mapper> {
        let profiles = compile(cfg)?;
        let active = match &cfg.input.start_profile {
            Some(n) => profiles
                .iter()
                .position(|p| p.name.eq_ignore_ascii_case(n))
                .ok_or_else(|| anyhow!("input.start_profile {n:?} is not a profile"))?,
            None => 0,
        };
        let profiles_first_engine = profiles[active].mouse.engine.clone();
        Ok(Mapper {
            profiles,
            active,
            held: HashMap::new(),
            toggled: HashSet::new(),
            wheel_until: HashMap::new(),
            wheel_press: Duration::from_millis(cfg.input.wheel_press_ms),
            mouse_samples: VecDeque::new(),
            engine: MouseEngine::new(profiles_first_engine),
            epoch: Instant::now(),
            touch_id_next: 0,
            touch: None,
        })
    }

    pub fn profile_name(&self) -> &str {
        &self.profiles[self.active].name
    }

    fn switch_profile(&mut self, index: usize) {
        if index != self.active {
            self.active = index;
            self.toggled.clear();
            self.engine
                .set_config(self.profiles[index].mouse.engine.clone());
            self.engine.reset();
            log::info!("profile: {}", self.profiles[index].name);
        }
    }

    /// Forgets everything held (e.g. when input capture is released).
    pub fn release_all(&mut self) {
        self.held.clear();
        self.toggled.clear();
        self.wheel_until.clear();
        self.mouse_samples.clear();
        self.engine.reset();
    }

    pub fn handle(&mut self, ev: Event, now: Instant) {
        match ev {
            Event::Down(id) => {
                if self.held.contains_key(&id) {
                    return; // key repeat
                }
                self.held.insert(id, now);
                self.on_press(id);
            }
            Event::Up(id) => {
                self.held.remove(&id);
            }
            Event::Wheel(id) => {
                if !self.wheel_until.contains_key(&id) {
                    self.on_press(id);
                }
                self.wheel_until.insert(id, now + self.wheel_press);
            }
            Event::MouseMove(dx, dy) => {
                let target = self.profiles[self.active].mouse.target;
                if matches!(target, MouseTarget::Gyro) {
                    self.mouse_samples.push_back((now, dx, dy));
                } else {
                    self.engine.push(
                        now.saturating_duration_since(self.epoch).as_millis() as u64,
                        dx,
                        dy,
                    );
                }
            }
        }
    }

    fn on_press(&mut self, id: InputId) {
        let Some(b) = self.profiles[self.active].binds.get(&id).cloned() else {
            return;
        };
        match b.action {
            Action::Profile(i) => self.switch_profile(i),
            Action::ProfileNext => self.switch_profile((self.active + 1) % self.profiles.len()),
            _ if b.toggle && !self.toggled.remove(&id) => {
                self.toggled.insert(id);
            }
            _ => {}
        }
    }

    fn mouse_velocity(&mut self, now: Instant, window_ms: f32) -> (f32, f32) {
        let window = Duration::from_secs_f32(window_ms.max(1.0) / 1000.0);
        while let Some(&(t, _, _)) = self.mouse_samples.front() {
            if now.duration_since(t) > window {
                self.mouse_samples.pop_front();
            } else {
                break;
            }
        }
        let (sx, sy) = self
            .mouse_samples
            .iter()
            .fold((0i64, 0i64), |(ax, ay), &(_, x, y)| {
                (ax + x as i64, ay + y as i64)
            });
        (
            sx as f32 / window_ms.max(1.0),
            sy as f32 / window_ms.max(1.0),
        )
    }

    /// Computes the controller state for this moment.
    pub fn state(&mut self, now: Instant) -> ControllerState {
        self.wheel_until.retain(|_, until| *until > now);

        let profile = &self.profiles[self.active];
        let mut st = ControllerState::default();
        let mut l2 = 0f32;
        let mut r2 = 0f32;
        // per stick: up, down, left, right
        let mut dirs = [[0f32; 4]; 2];
        let mut scale = [1f32; 2];
        let mut touch_pos: Option<(u16, u16)> = None;

        let active_inputs = self
            .held
            .iter()
            .map(|(&id, &t)| (id, Some(t)))
            .chain(self.wheel_until.keys().map(|&id| (id, None)))
            .chain(self.toggled.iter().map(|&id| (id, None)));

        for (id, since) in active_inputs {
            let Some(b) = profile.binds.get(&id) else {
                continue;
            };
            if b.toggle && since.is_some() {
                continue; // a toggle binding counts through `toggled`, not while held
            }
            if let (Some(hz), Some(t)) = (b.turbo_hz, since) {
                let phase = (now.duration_since(t).as_secs_f32() * hz).fract();
                if phase >= 0.5 {
                    continue;
                }
            }
            match b.action {
                Action::Button(bt) => st.buttons |= bt,
                Action::L2(v) => l2 = l2.max(v),
                Action::R2(v) => r2 = r2.max(v),
                Action::Stick(s, d, v) => {
                    let i = s as usize;
                    let j = d as usize;
                    dirs[i][j] = dirs[i][j].max(v);
                }
                Action::StickScale(s, v) => scale[s as usize] = scale[s as usize].min(v),
                Action::Touch(x, y) => touch_pos = touch_pos.or(Some((x, y))),
                Action::Profile(_) | Action::ProfileNext => {}
            }
        }

        st.l2 = (l2 * 255.0).round() as u8;
        st.r2 = (r2 * 255.0).round() as u8;

        let mut sticks = [(0f32, 0f32); 2];
        for i in 0..2 {
            let [up, down, left, right] = dirs[i];
            let (mut x, mut y) = (right - left, down - up);
            if profile.normalize_diagonals && x != 0.0 && y != 0.0 {
                let target = x.abs().max(y.abs());
                let len = (x * x + y * y).sqrt();
                x = x / len * target;
                y = y / len * target;
            }
            sticks[i] = (x * scale[i], y * scale[i]);
        }

        let m = profile.mouse.clone();
        let now_ms = now.saturating_duration_since(self.epoch).as_millis() as u64;
        match m.target {
            MouseTarget::LeftStick | MouseTarget::RightStick => {
                // aiming down sights (L2 held) slows the mouse by `ads_multiplier`
                self.engine.set_ads(st.l2 > 20);
                let s = self.engine.tick(now_ms);
                let i = if m.target == MouseTarget::LeftStick {
                    0
                } else {
                    1
                };
                sticks[i].0 += s.x;
                sticks[i].1 += s.y;
            }
            MouseTarget::Gyro => {
                let (vx, vy) = self.mouse_velocity(now, m.engine.smoothing_ms);
                let y_sign = if m.engine.invert_y { -1.0 } else { 1.0 };
                let vx = vx * m.engine.sensitivity_x;
                let vy = vy * m.engine.sensitivity_y * y_sign;
                st.gyro[0] = (-vy * m.gyro_sensitivity).clamp(-30.0, 30.0);
                st.gyro[1] = (-vx * m.gyro_sensitivity).clamp(-30.0, 30.0);
            }
            MouseTarget::None => {
                self.mouse_samples.clear();
                self.engine.reset();
            }
        }

        let to_i16 = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        st.left_x = to_i16(sticks[0].0);
        st.left_y = to_i16(sticks[0].1);
        st.right_x = to_i16(sticks[1].0);
        st.right_y = to_i16(sticks[1].1);

        match (touch_pos, self.touch) {
            (Some((x, y)), Some((id, _, _))) => self.touch = Some((id, x, y)),
            (Some((x, y)), None) => {
                let id = self.touch_id_next as i8;
                self.touch_id_next = (self.touch_id_next + 1) & 0x7f;
                self.touch = Some((id, x, y));
            }
            (None, _) => self.touch = None,
        }
        if let Some((id, x, y)) = self.touch {
            st.touches[0].id = id;
            st.touches[0].x = x;
            st.touches[0].y = y;
        }
        st
    }

    /// Whether the state can change without new input (mouse decay, wheel release, turbo).
    pub fn needs_ticks(&self) -> bool {
        !self.mouse_samples.is_empty()
            || self.engine.is_active()
            || !self.wheel_until.is_empty()
            || self.held.keys().chain(self.toggled.iter()).any(|id| {
                self.profiles[self.active]
                    .binds
                    .get(id)
                    .map(|b| b.turbo_hz.is_some())
                    .unwrap_or(false)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(binds: &str) -> Config {
        toml::from_str(&format!(
            "[[profile]]\nname = \"a\"\n[profile.bind]\n{binds}\n[[profile]]\nname = \"b\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn keys_drive_sticks_and_buttons() {
        let c = cfg("W = \"left_stick_up\"\nD = \"left_stick_right\"\nSpace = \"cross\"\nMouseLeft = \"r2\"");
        let mut m = Mapper::new(&c).unwrap();
        let t = Instant::now();
        m.handle(Event::Down(keys::parse("W").unwrap()), t);
        m.handle(Event::Down(keys::parse("Space").unwrap()), t);
        m.handle(Event::Down(keys::MOUSE_LEFT), t);
        let s = m.state(t);
        assert_eq!(s.left_y, -32767);
        assert_eq!(s.buttons, button::CROSS);
        assert_eq!(s.r2, 255);
        m.handle(Event::Down(keys::parse("D").unwrap()), t);
        let s = m.state(t);
        assert!(s.left_x > 23000 && s.left_x < 23300, "{}", s.left_x);
        m.handle(Event::Up(keys::parse("W").unwrap()), t);
        m.handle(Event::Up(keys::parse("D").unwrap()), t);
        assert_eq!(m.state(t).left_x, 0);
    }

    #[test]
    fn toggle_and_profile_switch() {
        let c = cfg("C = { action = \"circle\", toggle = true }\nF2 = \"profile:b\"");
        let mut m = Mapper::new(&c).unwrap();
        let t = Instant::now();
        let k = keys::parse("C").unwrap();
        m.handle(Event::Down(k), t);
        m.handle(Event::Up(k), t);
        assert_eq!(m.state(t).buttons, button::CIRCLE);
        m.handle(Event::Down(k), t);
        m.handle(Event::Up(k), t);
        assert_eq!(m.state(t).buttons, 0);
        m.handle(Event::Down(keys::parse("F2").unwrap()), t);
        assert_eq!(m.profile_name(), "b");
    }

    #[test]
    fn mouse_moves_right_stick_and_recenters() {
        let c = cfg("");
        let mut m = Mapper::new(&c).unwrap();
        let t = Instant::now();
        m.handle(Event::MouseMove(20, 0), t);
        let s = m.state(t);
        assert!(s.right_x > 0);
        assert_eq!(s.right_y, 0);
        let later = t + Duration::from_millis(50);
        assert_eq!(m.state(later).right_x, 0);
    }

    #[test]
    fn ads_slows_the_mouse_while_l2_is_held() {
        let c = cfg("MouseRight = \"l2\"");
        let mut m = Mapper::new(&c).unwrap();
        let t = Instant::now();
        m.handle(Event::MouseMove(40, 0), t);
        let free = m.state(t).right_x;
        let mut m = Mapper::new(&c).unwrap();
        m.handle(Event::Down(keys::MOUSE_RIGHT), t);
        m.handle(Event::MouseMove(40, 0), t);
        let ads = m.state(t).right_x;
        assert!(free > 0 && ads > 0 && ads < free, "free {free} ads {ads}");
    }
}
