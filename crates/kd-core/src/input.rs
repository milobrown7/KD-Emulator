//! Front-end input snapshot shared by all cores.
//!
//! The Android layer (touch overlay, hardware gamepads, sensors) fills one
//! [`InputState`] per frame; each core maps it onto its own hardware.

/// Logical buttons. Cores ignore buttons their hardware does not have.
pub mod button {
    pub const A: u32 = 1 << 0;
    pub const B: u32 = 1 << 1;
    pub const X: u32 = 1 << 2;
    pub const Y: u32 = 1 << 3;
    pub const L: u32 = 1 << 4;
    pub const R: u32 = 1 << 5;
    pub const Z: u32 = 1 << 6;
    pub const START: u32 = 1 << 7;
    pub const SELECT: u32 = 1 << 8;
    pub const UP: u32 = 1 << 9;
    pub const DOWN: u32 = 1 << 10;
    pub const LEFT: u32 = 1 << 11;
    pub const RIGHT: u32 = 1 << 12;
    /// Wii Remote buttons not covered above.
    pub const ONE: u32 = 1 << 13;
    pub const TWO: u32 = 1 << 14;
    pub const PLUS: u32 = 1 << 15;
    pub const MINUS: u32 = 1 << 16;
    pub const HOME: u32 = 1 << 17;
    /// Nunchuk.
    pub const C: u32 = 1 << 18;
    /// NDS lid (closed when set).
    pub const LID_CLOSED: u32 = 1 << 19;
    /// DS debug button.
    pub const DEBUG: u32 = 1 << 20;
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct InputState {
    /// Bitmask of [`button`] constants.
    pub buttons: u32,
    /// Main analog stick, -1.0..=1.0 (GameCube control stick / Nunchuk).
    pub stick: (f32, f32),
    /// GameCube C-stick.
    pub c_stick: (f32, f32),
    /// Analog triggers 0.0..=1.0.
    pub triggers: (f32, f32),
    /// NDS touchscreen point in screen pixels (0..256, 0..192).
    pub touch: Option<(u16, u16)>,
    /// Device tilt in g along screen X/Y (GBA tilt cartridges).
    pub tilt: (f32, f32),
    /// Wii Remote IR pointer, normalised -1.0..=1.0, `None` when off-screen.
    pub pointer: Option<(f32, f32)>,
    /// Accelerometer in g (Wii Remote), from `TYPE_GAME_ROTATION_VECTOR`.
    pub accel: [f32; 3],
    /// Ambient light 0..=255 for GBA solar sensor cartridges.
    pub light: u8,
}

impl InputState {
    #[inline]
    pub fn pressed(&self, b: u32) -> bool {
        self.buttons & b != 0
    }
}
