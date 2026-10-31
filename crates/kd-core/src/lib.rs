//! # kd-core
//!
//! Shared runtime services for every KD-Emulator console core:
//!
//! * [`scheduler`] — deterministic min-heap event scheduler on a `u64` master clock.
//! * [`savestate`] — versioned, checksummed, sectioned save states.
//! * [`audio`] — lock-free SPSC ring buffer and dynamic-rate resampler for AAudio.
//! * [`arena`] — `memfd`-backed guest RAM and dual-mapped W^X JIT arenas that
//!   respect the host page size (4 KB or 16 KB).
//! * [`topology`] — Tensor G1–G5 cluster detection, `sched_setaffinity`
//!   pinning and AArch64 feature detection.
//! * [`thermal`] — Vulkan vendor profiles and ADPF thermal policy.
//! * [`clock`] — guest RTCs with per-game time travel.
//! * [`animal_crossing`] — the Animal Crossing profile for each system.
//! * [`input`] — front-end input snapshot.

pub mod animal_crossing;
pub mod arena;
pub mod audio;
pub mod clock;
pub mod input;
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
    GameCube = 3,
    Wii = 4,
}

impl System {
    pub fn name(self) -> &'static str {
        match self {
            System::Gba => "Game Boy Advance",
            System::Nds => "Nintendo DS",
            System::GameCube => "GameCube",
            System::Wii => "Wii",
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
}
