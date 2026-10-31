//! # gpu-gx-vk
//!
//! The GameCube/Wii "Flipper/Hollywood" GX pipeline:
//!
//! * [`fifo`] — command FIFO / display-list parser.
//! * [`vertex`] — CP vertex descriptors, attribute tables and decoding.
//! * [`xf`] — transform unit: matrices, projection, viewport.
//! * [`texture`] — all GX texture formats (incl. CMPR and paletted).
//! * [`tev`] — TEV configuration, CPU evaluator and GLSL generation.
//! * [`shader`] — GLSL → SPIR-V (naga), OpKill/HSR checks, vendor
//!   compute shaders and the uber-shader.
//! * [`soft`] / [`gx`] — software reference renderer and the GX state
//!   machine (EFB, XFB and texture copies).
//! * [`profile`] / [`vulkan`] — PowerVR DXT vs Mali renderer profile,
//!   pipeline-cache persistence and Vulkan bring-up.

pub mod fifo;
pub mod gx;
pub mod profile;
pub mod shader;
pub mod soft;
pub mod tev;
pub mod texture;
pub mod vertex;
pub mod vulkan;
pub mod xf;

pub use gx::{Gx, GxEvent, GxMemory};
