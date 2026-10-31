//! Wii Remote HID input report synthesis (core buttons, accelerometer, IR
//! camera, Nunchuk and Classic Controller extensions), driven by the
//! front-end's touch overlay and `TYPE_GAME_ROTATION_VECTOR` sensor.

use kd_core::input::button;
use kd_core::InputState;

/// The two core-button bytes.
pub fn core_buttons(i: &InputState) -> [u8; 2] {
    let mut b0 = 0u8;
    let mut b1 = 0u8;
    let set = |cond: bool, bit: u8, b: &mut u8| {
        if cond {
            *b |= bit;
        }
    };
    set(i.pressed(button::LEFT), 0x01, &mut b0);
    set(i.pressed(button::RIGHT), 0x02, &mut b0);
    set(i.pressed(button::DOWN), 0x04, &mut b0);
    set(i.pressed(button::UP), 0x08, &mut b0);
    set(i.pressed(button::PLUS) || i.pressed(button::START), 0x10, &mut b0);
    set(i.pressed(button::TWO), 0x01, &mut b1);
    set(i.pressed(button::ONE), 0x02, &mut b1);
    set(i.pressed(button::B), 0x04, &mut b1);
    set(i.pressed(button::A), 0x08, &mut b1);
    set(i.pressed(button::MINUS) || i.pressed(button::SELECT), 0x10, &mut b1);
    set(i.pressed(button::HOME), 0x80, &mut b1);
    [b0, b1]
}

/// 10-bit accelerometer values (0x200 = 0 g, ~0x60 per g) packed as the
/// three high bytes; the two low bits ride in the button bytes.
pub fn accel(i: &InputState) -> ([u8; 3], [u8; 2]) {
    let conv = |g: f32| (0x200 as f32 + g * 0x60 as f32).clamp(0.0, 1023.0) as u16;
    let (x, y, z) = (conv(i.accel[0]), conv(i.accel[1]), conv(i.accel[2]));
    let hi = [(x >> 2) as u8, (y >> 2) as u8, (z >> 2) as u8];
    let lo = [((x & 3) << 5) as u8, (((y >> 1) & 1) << 5 | ((z >> 1) & 1) << 6) as u8];
    (hi, lo)
}

/// IR camera dots (1024x768) for the sensor-bar LEDs, or `None` when the
/// pointer is off-screen.
pub fn ir_dots(i: &InputState) -> Option<[(u16, u16); 2]> {
    let (px, py) = i.pointer?;
    // The camera sees the bar mirrored: pointing right moves the dots left.
    let cx = 512.0 - px.clamp(-1.0, 1.0) * 400.0;
    let cy = 384.0 + py.clamp(-1.0, 1.0) * 300.0;
    let half_gap = 100.0;
    Some([((cx - half_gap) as u16, cy as u16), ((cx + half_gap) as u16, cy as u16)])
}

/// Extended IR format: 4 objects × 3 bytes.
pub fn ir_extended(i: &InputState) -> [u8; 12] {
    let mut out = [0xFFu8; 12];
    if let Some(dots) = ir_dots(i) {
        for (n, (x, y)) in dots.iter().enumerate() {
            let size = 3u8;
            out[n * 3] = *x as u8;
            out[n * 3 + 1] = *y as u8;
            out[n * 3 + 2] = (((*y >> 8) as u8 & 3) << 6) | (((*x >> 8) as u8 & 3) << 4) | size;
        }
    }
    out
}

/// Basic IR format: 2 pairs × 5 bytes.
pub fn ir_basic(i: &InputState) -> [u8; 10] {
    let mut out = [0xFFu8; 10];
    if let Some([(x1, y1), (x2, y2)]) = ir_dots(i) {
        out[0] = x1 as u8;
        out[1] = y1 as u8;
        out[2] = ((y1 >> 8) as u8 & 3) << 6
            | ((x1 >> 8) as u8 & 3) << 4
            | ((y2 >> 8) as u8 & 3) << 2
            | ((x2 >> 8) as u8 & 3);
        out[3] = x2 as u8;
        out[4] = y2 as u8;
    }
    out
}

/// Nunchuk extension block (6 bytes, unencrypted mode).
pub fn nunchuk(i: &InputState) -> [u8; 6] {
    let stick = |v: f32| (128.0 + v.clamp(-1.0, 1.0) * 100.0) as u8;
    let acc = |g: f32| (0x200 as f32 + g * 0x60 as f32).clamp(0.0, 1023.0) as u16;
    let (ax, ay, az) = (acc(0.0), acc(0.0), acc(1.0));
    let c = !i.pressed(button::C) as u8;
    let z = !i.pressed(button::Z) as u8;
    [
        stick(i.stick.0),
        stick(i.stick.1),
        (ax >> 2) as u8,
        (ay >> 2) as u8,
        (az >> 2) as u8,
        ((az & 3) << 6 | (ay & 3) << 4 | (ax & 3) << 2) as u8 | c << 1 | z,
    ]
}

/// Classic Controller extension block (6 bytes; buttons active low).
pub fn classic(i: &InputState) -> [u8; 6] {
    let lx = ((i.stick.0.clamp(-1.0, 1.0) * 31.0) + 32.0) as u8 & 0x3F;
    let ly = ((i.stick.1.clamp(-1.0, 1.0) * 31.0) + 32.0) as u8 & 0x3F;
    let rx = ((i.c_stick.0.clamp(-1.0, 1.0) * 15.0) + 16.0) as u8 & 0x1F;
    let ry = ((i.c_stick.1.clamp(-1.0, 1.0) * 15.0) + 16.0) as u8 & 0x1F;
    let lt = (i.triggers.0.clamp(0.0, 1.0) * 31.0) as u8;
    let rt = (i.triggers.1.clamp(0.0, 1.0) * 31.0) as u8;
    let mut b: u16 = 0;
    let map = [
        (button::RIGHT, 1 << 15),
        (button::DOWN, 1 << 14),
        (button::L, 1 << 13),
        (button::MINUS, 1 << 12),
        (button::HOME, 1 << 11),
        (button::PLUS, 1 << 10),
        (button::R, 1 << 9),
        (button::Z, 1 << 7),
        (button::B, 1 << 6),
        (button::Y, 1 << 5),
        (button::A, 1 << 4),
        (button::X, 1 << 3),
        (button::LEFT, 1 << 1),
        (button::UP, 1),
    ];
    for (k, bit) in map {
        if i.pressed(k) {
            b |= bit;
        }
    }
    let b = !b;
    [
        (rx >> 3) << 6 | lx,
        ((rx >> 1) & 3) << 6 | ly,
        (rx & 1) << 7 | (lt >> 3) << 5 | ry,
        (lt & 7) << 5 | rt,
        (b >> 8) as u8,
        b as u8,
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extension {
    None,
    Nunchuk,
    Classic,
}

/// Build a full input report (`0xA1` HID data prefix included).
pub fn input_report(mode: u8, ext: Extension, i: &InputState) -> Vec<u8> {
    let btn = core_buttons(i);
    let (acc, lo) = accel(i);
    let ext_bytes: [u8; 6] = match ext {
        Extension::Nunchuk => nunchuk(i),
        Extension::Classic => classic(i),
        Extension::None => [0xFF; 6],
    };
    let mut r = vec![0xA1, mode];
    let core = [btn[0] | lo[0], btn[1] | lo[1]];
    match mode {
        0x30 => r.extend(btn),
        0x31 => {
            r.extend(core);
            r.extend(acc);
        }
        0x33 => {
            r.extend(core);
            r.extend(acc);
            r.extend(ir_extended(i));
        }
        0x35 => {
            r.extend(core);
            r.extend(acc);
            r.extend(ext_bytes);
            r.extend([0u8; 10]);
        }
        0x37 => {
            r.extend(core);
            r.extend(acc);
            r.extend(ir_basic(i));
            r.extend(ext_bytes);
        }
        _ => r.extend(btn),
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_button_bits() {
        let i = InputState { buttons: button::A | button::LEFT | button::HOME, ..Default::default() };
        assert_eq!(core_buttons(&i), [0x01, 0x88]);
    }

    #[test]
    fn report_lengths() {
        let i = InputState::default();
        assert_eq!(input_report(0x30, Extension::None, &i).len(), 4);
        assert_eq!(input_report(0x31, Extension::None, &i).len(), 7);
        assert_eq!(input_report(0x33, Extension::None, &i).len(), 19);
        assert_eq!(input_report(0x37, Extension::Nunchuk, &i).len(), 23);
    }

    #[test]
    fn pointer_produces_sensor_bar_dots() {
        let centre = InputState { pointer: Some((0.0, 0.0)), ..Default::default() };
        assert_eq!(ir_dots(&centre), Some([(412, 384), (612, 384)]));
        let right = InputState { pointer: Some((0.5, 0.0)), ..Default::default() };
        assert!(ir_dots(&right).unwrap()[0].0 < 412, "camera image is mirrored");
        assert_eq!(ir_extended(&InputState::default()), [0xFF; 12], "no dots off-screen");
    }

    #[test]
    fn nunchuk_buttons_are_active_low() {
        let i = InputState { buttons: button::C, stick: (1.0, 0.0), ..Default::default() };
        let n = nunchuk(&i);
        assert_eq!(n[0], 228);
        assert_eq!(n[5] & 3, 0b01, "C pressed (0), Z released (1)");
    }

    #[test]
    fn classic_controller_neutral() {
        let c = classic(&InputState::default());
        assert_eq!(c[0] & 0x3F, 32);
        assert_eq!([c[4], c[5]], [0xFF, 0xFF], "nothing pressed");
    }
}
