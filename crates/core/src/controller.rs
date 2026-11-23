//! Controller state and its wire formats (feedback state and feedback history events).
//! Port of chiaki-ng `lib/src/controller.c` and `lib/src/feedback.c`.

pub mod button {
    pub const CROSS: u32 = 1 << 0;
    pub const CIRCLE: u32 = 1 << 1;
    pub const SQUARE: u32 = 1 << 2;
    pub const TRIANGLE: u32 = 1 << 3;
    pub const DPAD_LEFT: u32 = 1 << 4;
    pub const DPAD_RIGHT: u32 = 1 << 5;
    pub const DPAD_UP: u32 = 1 << 6;
    pub const DPAD_DOWN: u32 = 1 << 7;
    pub const L1: u32 = 1 << 8;
    pub const R1: u32 = 1 << 9;
    pub const L3: u32 = 1 << 10;
    pub const R3: u32 = 1 << 11;
    pub const OPTIONS: u32 = 1 << 12;
    pub const SHARE: u32 = 1 << 13;
    pub const TOUCHPAD: u32 = 1 << 14;
    pub const PS: u32 = 1 << 15;
    pub const COUNT: u32 = 16;
}

pub const TOUCHPAD_WIDTH: u16 = 1920;
pub const TOUCHPAD_HEIGHT: u16 = 942;
pub const TOUCHES_MAX: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Touch {
    pub x: u16,
    pub y: u16,
    /// -1 = not touching
    pub id: i8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControllerState {
    pub buttons: u32,
    pub l2: u8,
    pub r2: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
    pub touches: [Touch; TOUCHES_MAX],
    pub gyro: [f32; 3],
    pub accel: [f32; 3],
    /// x, y, z, w
    pub orient: [f32; 4],
}

impl Default for ControllerState {
    fn default() -> Self {
        Self {
            buttons: 0,
            l2: 0,
            r2: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
            touches: [Touch { x: 0, y: 0, id: -1 }; TOUCHES_MAX],
            gyro: [0.0; 3],
            accel: [0.0, 1.0, 0.0],
            orient: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

impl ControllerState {
    /// Whether anything carried by the feedback *state* packet (sticks and motion) differs.
    pub fn feedback_state_differs(&self, other: &Self) -> bool {
        self.left_x != other.left_x
            || self.left_y != other.left_y
            || self.right_x != other.right_x
            || self.right_y != other.right_y
            || self.gyro != other.gyro
            || self.accel != other.accel
            || self.orient != other.orient
    }
}

const GYRO_MIN: f32 = -30.0;
const GYRO_MAX: f32 = 30.0;
const ACCEL_MIN: f32 = -5.0;
const ACCEL_MAX: f32 = 5.0;

fn compress_quat(q: &[f32; 4]) -> u32 {
    let sqrt1_2 = std::f32::consts::FRAC_1_SQRT_2;
    let mut largest = 0usize;
    for i in 1..4 {
        if q[i].abs() > q[largest].abs() {
            largest = i;
        }
    }
    let mut r: u32 = (if q[largest] < 0.0 { 1 } else { 0 }) | ((largest as u32) << 1);
    for i in 0..3 {
        let qi = if i < largest { i } else { i + 1 };
        let v = (q[qi].clamp(-sqrt1_2, sqrt1_2) + sqrt1_2) * (0x1ff as f32 / (2.0 * sqrt1_2));
        r |= (v as u32) << (3 + i * 9);
    }
    r
}

fn scale_u16(v: f32, min: f32, max: f32) -> u16 {
    (0xffff as f32 * ((v - min) / (max - min))).clamp(0.0, 65535.0) as u16
}

pub const FEEDBACK_STATE_SIZE_V12: usize = 0x1c;

/// Feedback state payload for Takion protocol version 12 (PS5).
pub fn format_feedback_state_v12(state: &ControllerState) -> [u8; FEEDBACK_STATE_SIZE_V12] {
    let mut buf = [0u8; FEEDBACK_STATE_SIZE_V12];
    buf[0] = 0xa0;
    let vals = [
        scale_u16(state.gyro[0], GYRO_MIN, GYRO_MAX),
        scale_u16(state.gyro[1], GYRO_MIN, GYRO_MAX),
        scale_u16(state.gyro[2], GYRO_MIN, GYRO_MAX),
        scale_u16(state.accel[0], ACCEL_MIN, ACCEL_MAX),
        scale_u16(state.accel[1], ACCEL_MIN, ACCEL_MAX),
        scale_u16(state.accel[2], ACCEL_MIN, ACCEL_MAX),
    ];
    for (i, v) in vals.iter().enumerate() {
        buf[1 + i * 2..3 + i * 2].copy_from_slice(&v.to_le_bytes());
    }
    buf[0xd..0x11].copy_from_slice(&compress_quat(&state.orient).to_le_bytes());
    buf[0x11..0x13].copy_from_slice(&(state.left_x as u16).to_be_bytes());
    buf[0x13..0x15].copy_from_slice(&(state.left_y as u16).to_be_bytes());
    buf[0x15..0x17].copy_from_slice(&(state.right_x as u16).to_be_bytes());
    buf[0x17..0x19].copy_from_slice(&(state.right_y as u16).to_be_bytes());
    buf[0x19] = 0;
    buf[0x1a] = 0;
    buf[0x1b] = 1;
    buf
}

#[derive(Clone, Copy, Debug)]
pub struct HistoryEvent {
    pub buf: [u8; 5],
    pub len: usize,
}

const ANALOG_L2: u32 = 1 << 16;
const ANALOG_R2: u32 = 1 << 17;

fn button_event(button: u32, state: u8) -> Option<HistoryEvent> {
    let mut ev = HistoryEvent {
        buf: [0x80, 0, 0, 0, 0],
        len: 2,
    };
    let pressed = state != 0;
    let code = match button {
        button::CROSS => 0x88,
        button::CIRCLE => 0x89,
        button::SQUARE => 0x8a,
        button::TRIANGLE => 0x8b,
        button::DPAD_LEFT => 0x82,
        button::DPAD_RIGHT => 0x83,
        button::DPAD_UP => 0x80,
        button::DPAD_DOWN => 0x81,
        button::L1 => 0x84,
        button::R1 => 0x85,
        ANALOG_L2 => 0x86,
        ANALOG_R2 => 0x87,
        // these use no third byte for the state
        button::L3 => {
            ev.buf[1] = if pressed { 0xaf } else { 0x8f };
            return Some(ev);
        }
        button::R3 => {
            ev.buf[1] = if pressed { 0xb0 } else { 0x90 };
            return Some(ev);
        }
        button::OPTIONS => {
            ev.buf[1] = if pressed { 0xac } else { 0x8c };
            return Some(ev);
        }
        button::SHARE => {
            ev.buf[1] = if pressed { 0xad } else { 0x8d };
            return Some(ev);
        }
        button::TOUCHPAD => {
            ev.buf[1] = if pressed { 0xb1 } else { 0x91 };
            return Some(ev);
        }
        button::PS => {
            ev.buf[1] = if pressed { 0xae } else { 0x8e };
            return Some(ev);
        }
        _ => return None,
    };
    ev.buf[1] = code;
    ev.buf[2] = state;
    ev.len = 3;
    Some(ev)
}

fn touch_event(down: bool, id: u8, x: u16, y: u16) -> HistoryEvent {
    HistoryEvent {
        buf: [
            if down { 0xd0 } else { 0xc0 },
            id & 0x7f,
            (x >> 4) as u8,
            (((x & 0xf) << 4) as u8) | ((y >> 8) as u8),
            y as u8,
        ],
        len: 5,
    }
}

/// Appends the history events describing the change from `prev` to `now`, in the order
/// chiaki-ng uses: releases, touches, presses, then analog triggers.
pub fn record_history(prev: &ControllerState, now: &ControllerState, out: &mut Vec<HistoryEvent>) {
    let buttons = |out: &mut Vec<HistoryEvent>, pressed: bool| {
        for i in 0..button::COUNT {
            let id = 1u32 << i;
            let p = prev.buttons & id != 0;
            let n = now.buttons & id != 0;
            if p == n || n != pressed {
                continue;
            }
            if let Some(ev) = button_event(id, if n { 0xff } else { 0 }) {
                out.push(ev);
            }
        }
    };

    buttons(out, false);

    for i in 0..TOUCHES_MAX {
        let (p, n) = (prev.touches[i], now.touches[i]);
        if p.id != n.id && p.id >= 0 {
            out.push(touch_event(false, p.id as u8, p.x, p.y));
        } else if n.id >= 0 && (p.id != n.id || p.x != n.x || p.y != n.y) {
            out.push(touch_event(true, n.id as u8, n.x, n.y));
        }
    }

    buttons(out, true);

    if prev.l2 != now.l2 {
        out.extend(button_event(ANALOG_L2, now.l2));
    }
    if prev.r2 != now.r2 {
        out.extend(button_event(ANALOG_R2, now.r2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_feedback_state_layout() {
        let buf = format_feedback_state_v12(&ControllerState::default());
        assert_eq!(buf[0], 0xa0);
        assert_eq!(buf[0x1b], 1);
        // sticks centered
        assert_eq!(&buf[0x11..0x19], &[0u8; 8]);
    }

    #[test]
    fn history_press_release() {
        let a = ControllerState::default();
        let mut b = a;
        b.buttons = button::CROSS | button::PS;
        b.r2 = 0xff;
        let mut ev = Vec::new();
        record_history(&a, &b, &mut ev);
        assert_eq!(ev.len(), 3);
        assert_eq!(&ev[0].buf[..ev[0].len], &[0x80, 0x88, 0xff]);
        assert_eq!(&ev[1].buf[..ev[1].len], &[0x80, 0xae]);
        assert_eq!(&ev[2].buf[..ev[2].len], &[0x80, 0x87, 0xff]);
    }
}
