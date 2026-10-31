//! # cpu-ppc
//!
//! IBM Gekko (GameCube) and Broadway (Wii) — PowerPC 750CL derivatives.
//!
//! * [`cpu::Ppc`] — full user/supervisor interpreter: integer, branch,
//!   condition register, FPU, the 750 paired-single extension
//!   (`ps_*`, `psq_l`/`psq_st` with GQR quantisation), SPRs, exceptions,
//!   timebase and decrementer.
//! * [`mmu`] — BAT and hashed-page-table translation behind a two-level
//!   4 KiB software TLB, independent of the host's 16 KiB pages.
//! * [`paired`] — quantisation and NEON `float32x2_t` paired-single paths.
//! * [`jit`] — AArch64 basic-block compiler emitting into the dual-mapped
//!   W^X [`kd_core::arena::JitArena`].

pub mod cpu;
pub mod jit;
pub mod mmu;
pub mod paired;

pub use cpu::{Ppc, PpcBus, PpcModel};
