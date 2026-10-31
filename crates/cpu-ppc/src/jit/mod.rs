//! AArch64 basic-block JIT.
//!
//! Hot straight-line runs of integer PowerPC instructions are translated to
//! AArch64 and emitted into the dual-mapped, page-size-aware
//! [`JitArena`](kd_core::arena::JitArena) (`rw` view for writing, `rx` view
//! for execution, `dc cvau / ic ivau / dsb ish / isb` maintenance on emit).
//!
//! Compiled code receives a pointer to the 32 GPRs in `x0` and returns the
//! PowerPC address to continue at in `w0`. A block stops before the first
//! instruction the compiler does not handle (loads/stores, record forms,
//! compares, conditional branches...), which the interpreter then executes.
//!
//! Code generation is architecture-independent and unit-tested on every
//! host; only *executing* blocks requires an AArch64 CPU.

pub mod a64;

use crate::cpu::{rot_mask, Ppc, PpcBus};
use a64::*;
use std::collections::HashMap;

/// Longest block compiled (PowerPC instructions).
pub const MAX_BLOCK: usize = 64;

const X_STATE: u32 = 0;
const W_A: u32 = 1;
const W_B: u32 = 2;
const W_D: u32 = 3;

/// A translated block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub start: u32,
    /// AArch64 machine code (little-endian words).
    pub code: Vec<u32>,
    /// PowerPC instructions covered.
    pub ppc_len: u32,
}

impl Block {
    pub fn bytes(&self) -> Vec<u8> {
        self.code.iter().flat_map(|w| w.to_le_bytes()).collect()
    }
}

struct Emitter {
    code: Vec<u32>,
}

impl Emitter {
    fn load(&mut self, w: u32, r: usize) {
        self.code.push(ldr_w(w, X_STATE, r as u32 * 4));
    }
    fn store(&mut self, w: u32, r: usize) {
        self.code.push(str_w(w, X_STATE, r as u32 * 4));
    }
    fn imm(&mut self, w: u32, v: u32) {
        self.code.extend(mov_imm(w, v));
    }
    fn exit(&mut self, next_pc: u32) {
        self.imm(0, next_pc);
        self.code.push(ret());
    }
}

/// Translate one instruction. Returns `false` if it is not supported.
fn translate(e: &mut Emitter, insn: u32) -> bool {
    let d = ((insn >> 21) & 31) as usize;
    let a = ((insn >> 16) & 31) as usize;
    let b = ((insn >> 11) & 31) as usize;
    let simm = insn as i16 as i32 as u32;
    let uimm = insn & 0xFFFF;
    match insn >> 26 {
        14 | 15 => {
            let v = if insn >> 26 == 14 { simm } else { insn << 16 };
            if a == 0 {
                e.imm(W_D, v);
            } else {
                e.load(W_A, a);
                e.imm(W_B, v);
                e.code.push(add(W_D, W_A, W_B));
            }
            e.store(W_D, d);
        }
        7 => {
            e.load(W_A, a);
            e.imm(W_B, simm);
            e.code.push(mul(W_D, W_A, W_B));
            e.store(W_D, d);
        }
        24..=27 => {
            let v = if insn >> 26 & 1 == 0 { uimm } else { uimm << 16 };
            e.load(W_A, d);
            e.imm(W_B, v);
            e.code.push(if insn >> 26 <= 25 { orr(W_D, W_A, W_B) } else { eor(W_D, W_A, W_B) });
            e.store(W_D, a);
        }
        21 if insn & 1 == 0 => {
            let sh = b as u32;
            let mask = rot_mask((insn >> 6) & 31, (insn >> 1) & 31);
            e.load(W_A, d);
            if sh != 0 {
                e.code.push(ror_imm(W_A, W_A, 32 - sh));
            }
            match mask {
                u32::MAX => e.code.push(mov(W_D, W_A)),
                0 => e.code.push(movz(W_D, 0, 0)),
                m => e.code.push(and_imm(W_D, W_A, m).expect("rlwinm masks are contiguous runs")),
            }
            e.store(W_D, a);
        }
        31 if insn & 1 == 0 => {
            let xo = (insn >> 1) & 0x3FF;
            match xo {
                // XO-form arithmetic without OE
                266 | 40 | 235 | 104 => {
                    e.load(W_A, a);
                    if xo != 104 {
                        e.load(W_B, b);
                    }
                    e.code.push(match xo {
                        266 => add(W_D, W_A, W_B),
                        40 => sub(W_D, W_B, W_A),
                        235 => mul(W_D, W_A, W_B),
                        _ => neg(W_D, W_A),
                    });
                    e.store(W_D, d);
                }
                // logical: rS = d, result → rA
                28 | 60 | 124 | 284 | 316 | 412 | 444 | 476 => {
                    e.load(W_A, d);
                    e.load(W_B, b);
                    match xo {
                        28 => e.code.push(and(W_D, W_A, W_B)),
                        60 => e.code.push(bic(W_D, W_A, W_B)),
                        124 => {
                            e.code.push(orr(W_D, W_A, W_B));
                            e.code.push(mvn(W_D, W_D));
                        }
                        284 => e.code.push(eon(W_D, W_A, W_B)),
                        316 => e.code.push(eor(W_D, W_A, W_B)),
                        412 => e.code.push(orn(W_D, W_A, W_B)),
                        444 => e.code.push(orr(W_D, W_A, W_B)),
                        _ => {
                            e.code.push(and(W_D, W_A, W_B));
                            e.code.push(mvn(W_D, W_D));
                        }
                    }
                    e.store(W_D, a);
                }
                26 | 922 | 954 => {
                    e.load(W_A, d);
                    e.code.push(match xo {
                        26 => clz(W_D, W_A),
                        922 => sxth(W_D, W_A),
                        _ => sxtb(W_D, W_A),
                    });
                    e.store(W_D, a);
                }
                _ => return false,
            }
        }
        _ => return false,
    }
    true
}

/// Compile the block starting at `start`, reading instructions with
/// `fetch`. Returns `None` if the very first instruction is unsupported.
pub fn compile(start: u32, mut fetch: impl FnMut(u32) -> Option<u32>) -> Option<Block> {
    let mut e = Emitter { code: Vec::with_capacity(MAX_BLOCK * 4) };
    let mut pc = start;
    let mut n = 0u32;
    while (n as usize) < MAX_BLOCK {
        let Some(insn) = fetch(pc) else { break };
        // Unconditional relative branch without link ends the block.
        if insn >> 26 == 18 && insn & 3 == 0 {
            let li = (((insn & 0x03FF_FFFC) << 6) as i32 >> 6) as u32;
            n += 1;
            e.exit(pc.wrapping_add(li));
            return Some(Block { start, code: e.code, ppc_len: n });
        }
        if !translate(&mut e, insn) {
            break;
        }
        n += 1;
        pc = pc.wrapping_add(4);
    }
    if n == 0 {
        return None;
    }
    e.exit(pc);
    Some(Block { start, code: e.code, ppc_len: n })
}

/// Compiled-block cache backed by a [`kd_core::arena::JitArena`].
pub struct JitCache {
    #[cfg(target_arch = "aarch64")]
    arena: kd_core::arena::JitArena,
    #[cfg(target_arch = "aarch64")]
    entries: HashMap<u32, (*const u8, u32)>,
    /// Blocks known not to compile, so they are not retried every time.
    rejects: HashMap<u32, ()>,
    pub compiled: u64,
}

// SAFETY: code pointers refer into the arena owned by this cache.
unsafe impl Send for JitCache {}

impl JitCache {
    pub fn new(size: usize) -> std::io::Result<Self> {
        let _ = size;
        Ok(Self {
            #[cfg(target_arch = "aarch64")]
            arena: kd_core::arena::JitArena::new(size)?,
            #[cfg(target_arch = "aarch64")]
            entries: HashMap::new(),
            rejects: HashMap::new(),
            compiled: 0,
        })
    }

    /// Whether this host can execute compiled blocks.
    pub const fn available() -> bool {
        cfg!(target_arch = "aarch64")
    }

    /// Drop all compiled code (e.g. after `icbi` or a BAT change).
    pub fn invalidate_all(&mut self) {
        #[cfg(target_arch = "aarch64")]
        {
            self.entries.clear();
            self.arena.reset();
        }
        self.rejects.clear();
    }

    /// Try to run a compiled block at `cpu.pc`. Returns the number of
    /// PowerPC instructions executed (0 = fall back to the interpreter).
    pub fn run<B: PpcBus>(&mut self, cpu: &mut Ppc, bus: &mut B) -> u32 {
        if self.rejects.contains_key(&cpu.pc) {
            return 0;
        }
        #[cfg(target_arch = "aarch64")]
        {
            let entry = match self.entries.get(&cpu.pc) {
                Some(&e) => e,
                None => {
                    let start = cpu.pc;
                    let block = compile(start, |pc| {
                        let pa = cpu.translate(bus, pc, true, false)?;
                        Some(bus.read32(pa))
                    });
                    let Some(block) = block else {
                        self.rejects.insert(start, ());
                        return 0;
                    };
                    let Some(ptr) = self.arena.emit(&block.bytes(), 16).or_else(|| {
                        self.entries.clear();
                        self.arena.reset();
                        self.arena.emit(&block.bytes(), 16)
                    }) else {
                        return 0;
                    };
                    self.compiled += 1;
                    self.entries.insert(start, (ptr, block.ppc_len));
                    (ptr, block.ppc_len)
                }
            };
            // SAFETY: the block was generated for this ABI and only touches
            // the 32-word GPR array passed in x0.
            let f: extern "C" fn(*mut u32) -> u32 = unsafe { std::mem::transmute(entry.0) };
            cpu.pc = f(cpu.gpr.as_mut_ptr());
            cpu.retired += entry.1 as u64;
            entry.1
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = bus;
            self.rejects.insert(cpu.pc, ());
            0
        }
    }
}
