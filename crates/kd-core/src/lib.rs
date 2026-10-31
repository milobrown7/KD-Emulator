//! # kd-core
//!
//! Shared runtime services for every KD-Emulator console core:
//!
//! * [`scheduler`] — deterministic min-heap event scheduler on a `u64` master clock.
//! * [`savestate`] — versioned, checksummed, sectioned save states.
//! * [`audio`] — lock-free SPSC ring buffer and dynamic-rate resampler for AAudio.
//! * [`arena`] — host page size (4 KB or 16 KB).
//! * [`topology`] — Tensor G1–G5 cluster detection, `sched_setaffinity`
//!   pinning and AArch64 feature detection.
//! * [`thermal`] — ADPF thermal policy.
//! * [`clock`] — guest RTCs with per-game time travel.
//! * [`animal_crossing`] — the Animal Crossing profile for each system.
//! * [`input`] — front-end input snapshot.
//! * [`cheats`] — Action Replay, GameShark, CodeBreaker and raw codes.
//! * [`rewind`] — delta-compressed rewind history.

pub mod animal_crossing;
pub mod arena;
pub mod audio;
pub mod cheats;
pub mod clock;
pub mod input;
pub mod rewind;
pub mod savestate;
pub mod scheduler;
pub mod thermal;
pub mod topology;

pub use audio::StereoFrame;
pub use clock::GuestClock;
pub use input::InputState;
pub use savestate::{SaveState, StateError, StateReader, StateResult, StateWriter};
pub use scheduler::{EventKind, Scheduler};

/// Consoles supported by KD-Emulator. The discriminant is stored in save
/// states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum System {
    Gba = 1,
    Nds = 2,
}

impl System {
    pub fn name(self) -> &'static str {
        match self {
            System::Gba => "Game Boy Advance",
            System::Nds => "Nintendo DS",
        }
    }
}

/// A rendered frame in RGBA8888 (byte order R, G, B, A — matches
/// `WINDOW_FORMAT_RGBA_8888`). Dual-screen systems stack screens vertically.
#[derive(Debug, Clone, Copy)]
pub struct FrameView<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u32],
}

/// The interface the JNI layer drives. Every console core implements it.
pub trait EmulatorCore: Send {
    fn system(&self) -> System;
    /// Product code from the ROM/disc header (`b"GAFE01"`, `b"ADME\0\0"`, ...).
    fn game_code(&self) -> [u8; 6];
    /// Emulate one guest video frame.
    fn run_frame(&mut self, input: &InputState);
    fn frame(&self) -> FrameView<'_>;
    /// Native output sample rate of [`EmulatorCore::drain_audio`].
    fn audio_sample_rate(&self) -> u32;
    /// Move generated audio into `out`.
    fn drain_audio(&mut self, out: &mut Vec<StereoFrame>);
    fn reset(&mut self);
    fn save_state(&self) -> Vec<u8>;
    fn load_state(&mut self, data: &[u8]) -> StateResult<()>;
    /// Battery-backed save memory, if the game has any.
    fn backup(&self) -> Option<&[u8]>;
    fn load_backup(&mut self, data: &[u8]);
    /// The guest RTC (Animal Crossing time travel lives here).
    fn clock(&mut self) -> &mut GuestClock;
    /// Animal Crossing title detected at boot, if any.
    fn animal_crossing(&self) -> Option<&'static animal_crossing::AcTitle> {
        animal_crossing::lookup(self.system(), &self.game_code())
    }
    /// Read `size` (1, 2 or 4) bytes from the main CPU's address space, for
    /// cheats and the debugger. RAM reads have no side effects.
    fn peek(&mut self, addr: u32, size: u8) -> u32;
    /// Write `size` bytes to the main CPU's address space (cheats).
    fn poke(&mut self, addr: u32, value: u32, size: u8);
    /// Host microphone audio (signed 16-bit mono). Only the DS has a mic.
    fn push_mic(&mut self, _samples: &[i16]) {}
}

/// Adapts a core to [`cheats::CheatMemory`].
pub struct CoreMemory<'a>(pub &'a mut dyn EmulatorCore);

impl cheats::CheatMemory for CoreMemory<'_> {
    fn read(&mut self, addr: u32, size: u8) -> u32 {
        self.0.peek(addr, size)
    }
    fn write(&mut self, addr: u32, value: u32, size: u8) {
        self.0.poke(addr, value, size)
    }
}
