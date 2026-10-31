//! Gekko/Broadway interpreter, MMU, paired-single and JIT tests.
#![allow(clippy::vec_init_then_push)]

use cpu_ppc::cpu::*;
use cpu_ppc::jit;
use cpu_ppc::{Ppc, PpcBus, PpcModel};

/// 32 MiB of big-endian physical memory.
struct Mem {
    ram: Vec<u8>,
    zeroed: Vec<u32>,
}

impl Mem {
    fn new() -> Self {
        Self { ram: vec![0; 32 << 20], zeroed: vec![] }
    }
    fn load_code(&mut self, pa: u32, code: &[u32]) {
        for (i, w) in code.iter().enumerate() {
            self.write32(pa + i as u32 * 4, *w);
        }
    }
    fn r32(&self, pa: u32) -> u32 {
        let a = pa as usize;
        u32::from_be_bytes(self.ram[a..a + 4].try_into().unwrap())
    }
}

impl PpcBus for Mem {
    fn read8(&mut self, pa: u32) -> u8 {
        self.ram[pa as usize]
    }
    fn read16(&mut self, pa: u32) -> u16 {
        let a = pa as usize;
        u16::from_be_bytes([self.ram[a], self.ram[a + 1]])
    }
    fn read32(&mut self, pa: u32) -> u32 {
        self.r32(pa)
    }
    fn write8(&mut self, pa: u32, v: u8) {
        self.ram[pa as usize] = v;
    }
    fn write16(&mut self, pa: u32, v: u16) {
        let a = pa as usize;
        self.ram[a..a + 2].copy_from_slice(&v.to_be_bytes());
    }
    fn write32(&mut self, pa: u32, v: u32) {
        let a = pa as usize;
        self.ram[a..a + 4].copy_from_slice(&v.to_be_bytes());
    }
    fn zero_block(&mut self, pa: u32) {
        self.zeroed.push(pa);
        for i in 0..32 {
            self.ram[(pa + i) as usize] = 0;
        }
    }
}

// --- tiny PowerPC assembler -------------------------------------------------

fn d(op: u32, rd: u32, ra: u32, imm: u32) -> u32 {
    op << 26 | rd << 21 | ra << 16 | (imm & 0xFFFF)
}
fn x(rd: u32, ra: u32, rb: u32, xo: u32, rc: u32) -> u32 {
    31 << 26 | rd << 21 | ra << 16 | rb << 11 | xo << 1 | rc
}
fn addi(rd: u32, ra: u32, imm: i32) -> u32 {
    d(14, rd, ra, imm as u32)
}
fn addis(rd: u32, ra: u32, imm: u32) -> u32 {
    d(15, rd, ra, imm)
}
fn ori(ra: u32, rs: u32, imm: u32) -> u32 {
    d(24, rs, ra, imm)
}
fn li32(r: u32, v: u32) -> [u32; 2] {
    [addis(r, 0, v >> 16), ori(r, r, v & 0xFFFF)]
}
fn rlwinm(ra: u32, rs: u32, sh: u32, mb: u32, me: u32) -> u32 {
    21 << 26 | rs << 21 | ra << 16 | sh << 11 | mb << 6 | me << 1
}
fn mtspr(spr: u32, rs: u32) -> u32 {
    31 << 26 | rs << 21 | (spr & 0x1F) << 16 | (spr >> 5) << 11 | 467 << 1
}
fn bc(bo: u32, bi: u32, off: i32) -> u32 {
    16 << 26 | bo << 21 | bi << 16 | (off as u32 & 0xFFFC)
}
fn b(off: i32) -> u32 {
    18 << 26 | (off as u32 & 0x03FF_FFFC)
}
const BLR: u32 = 0x4E80_0020;
const SC: u32 = 0x4400_0002;
const RFI: u32 = 0x4C00_0064;

fn run(cpu: &mut Ppc, mem: &mut Mem, n: usize) {
    for _ in 0..n {
        cpu.step(mem);
    }
}

fn boot(code: &[u32]) -> (Ppc, Mem) {
    let mut mem = Mem::new();
    mem.load_code(0x3000, code);
    let mut cpu = Ppc::new(PpcModel::Gekko);
    cpu.msr = MSR_FP;
    cpu.pc = 0x3000;
    (cpu, mem)
}

#[test]
fn integer_arithmetic_and_cr0() {
    let mut code = vec![];
    code.extend(li32(3, 0x7FFF_FFFF));
    code.push(addi(4, 0, 1));
    code.push(x(5, 3, 4, 266 | 0x200, 1)); // addo. r5, r3, r4 → overflow, negative
    code.push(x(6, 4, 3, 40, 0)); // subf r6, r4, r3 → r3 - r4
    code.extend(li32(7, 0xFFFF_FFFF));
    code.push(x(8, 7, 4, 10, 0)); // addc r8 = r7 + r4 → 0, CA
    code.push(x(9, 0, 0, 202, 0)); // addze r9 = r0 + CA = 1  (r0 is 0)
    code.push(d(7, 10, 3, 3)); // mulli r10 = r3 * 3
    code.push(x(11, 7, 4, 491, 0)); // divw r11 = -1 / 1
    code.push(rlwinm(12, 3, 8, 24, 31)); // r12 = (r3 rotl 8) & 0xFF = 0x7F
    code.push(x(7, 13, 0, 26, 0)); // cntlzw r13, r7 = 0
    code.push(x(4, 14, 0, 26, 0)); // cntlzw r14, r4 = 31
    code.extend(li32(15, 0xFFFF_FFF1));
    code.push(x(15, 16, 2, 824, 0)); // srawi r16, r15, 2 → -4, CA (bits shifted out)
    let (mut cpu, mut mem) = boot(&code);
    run(&mut cpu, &mut mem, code.len());
    assert_eq!(cpu.gpr[5], 0x8000_0000);
    assert_eq!(cpu.cr_field(0), 0b1001, "LT + SO");
    assert_ne!(cpu.xer() & XER_OV, 0);
    assert_eq!(cpu.gpr[6], 0x7FFF_FFFE);
    assert_eq!(cpu.gpr[8], 0);
    assert_eq!(cpu.gpr[9], 1);
    assert_eq!(cpu.gpr[10], 0x7FFF_FFFF_u32.wrapping_mul(3));
    assert_eq!(cpu.gpr[11], u32::MAX);
    assert_eq!(cpu.gpr[12], 0x7F);
    assert_eq!(cpu.gpr[13], 0);
    assert_eq!(cpu.gpr[14], 31);
    assert_eq!(cpu.gpr[16] as i32, -4);
    assert_ne!(cpu.xer() & XER_CA, 0);
}

#[test]
fn counted_loop_and_subroutine() {
    // r3 = sum 1..=10 via bdnz; then bl to a routine that doubles r3.
    let mut code = vec![];
    code.push(addi(4, 0, 10));
    code.push(mtspr(9, 4)); // mtctr r4
    code.push(addi(3, 0, 0));
    code.push(x(3, 3, 4, 266, 0)); // loop: add r3, r3, r4
    code.push(addi(4, 4, -1));
    code.push(bc(16, 0, -8)); // bdnz loop
    code.push(b(8) | 1); // bl double
    code.push(b(0)); // spin
    code.push(x(3, 3, 3, 266, 0)); // double: add r3, r3, r3
    code.push(BLR);
    let (mut cpu, mut mem) = boot(&code);
    run(&mut cpu, &mut mem, 200);
    assert_eq!(cpu.gpr[3], 110);
    assert_eq!(cpu.pc, 0x3000 + 7 * 4);
}

#[test]
fn big_endian_loads_and_stores() {
    let mut code = vec![];
    code.extend(li32(3, 0x0000_8000));
    code.extend(li32(4, 0x1234_8678));
    code.push(d(36, 4, 3, 0)); // stw r4, 0(r3)
    code.push(d(34, 5, 3, 0)); // lbz r5, 0(r3)
    code.push(d(40, 6, 3, 2)); // lhz r6, 2(r3)
    code.push(d(42, 7, 3, 2)); // lha r7, 2(r3)
    code.push(x(8, 0, 3, 534, 0)); // lwbrx r8, 0, r3
    code.push(d(37, 4, 3, 8)); // stwu r4, 8(r3) → r3 += 8
    code.push(d(47, 29, 3, 16)); // stmw r29, 16(r3)
    code.push(d(32, 9, 3, -8i32 as u32)); // lwz r9, -8(r3)
    let (mut cpu, mut mem) = boot(&code);
    cpu.gpr[29] = 0xAAAA_0001;
    cpu.gpr[30] = 0xAAAA_0002;
    cpu.gpr[31] = 0xAAAA_0003;
    run(&mut cpu, &mut mem, code.len());
    assert_eq!(&mem.ram[0x8000..0x8004], &[0x12, 0x34, 0x86, 0x78]);
    assert_eq!(cpu.gpr[5], 0x12);
    assert_eq!(cpu.gpr[6], 0x8678);
    assert_eq!(cpu.gpr[7], 0xFFFF_8678);
    assert_eq!(cpu.gpr[8], 0x7886_3412);
    assert_eq!(cpu.gpr[3], 0x8008);
    assert_eq!(mem.r32(0x8008 + 16 + 8), 0xAAAA_0003);
    assert_eq!(cpu.gpr[9], 0x1234_8678);
}

#[test]
fn os_bats_translate_cached_and_uncached_views() {
    let mut code = vec![];
    code.extend(li32(3, 0x8000_5000));
    code.extend(li32(4, 0xC000_5000));
    code.extend(li32(5, 0xCAFE_F00D));
    code.push(d(36, 5, 3, 0)); // stw r5, 0(r3)  → physical 0x5000
    code.push(d(32, 6, 4, 0)); // lwz r6, 0(r4)  → same physical word
    let mut mem = Mem::new();
    mem.load_code(0x3000, &code);
    let mut cpu = Ppc::new(PpcModel::Gekko);
    cpu.setup_os_bats();
    cpu.pc = 0x8000_3000; // code runs through the cached mirror
    run(&mut cpu, &mut mem, code.len());
    assert_eq!(mem.r32(0x5000), 0xCAFE_F00D);
    assert_eq!(cpu.gpr[6], 0xCAFE_F00D);
    assert!(cpu.itlb.hits > 0 && cpu.dtlb.misses > 0);
}

#[test]
fn unmapped_access_raises_dsi() {
    let mut code = vec![];
    code.extend(li32(3, 0x4000_0000)); // no BAT, empty page table
    code.push(d(32, 4, 3, 0));
    let mut mem = Mem::new();
    mem.load_code(0x3000, &code);
    let mut cpu = Ppc::new(PpcModel::Gekko);
    cpu.setup_os_bats();
    cpu.write_spr(SPR_SDR1 as u32 as usize, 0x0010_0000);
    cpu.pc = 0x8000_3000;
    run(&mut cpu, &mut mem, 3);
    assert_eq!(cpu.pc, 0x300);
    assert_eq!(cpu.spr[SPR_DAR], 0x4000_0000);
    assert_eq!(cpu.spr[SPR_SRR0], 0x8000_3008);
}

#[test]
fn syscall_and_rfi() {
    let (mut cpu, mut mem) = boot(&[SC, addi(3, 0, 7)]);
    // handler: r4 = 0x55; rfi
    mem.load_code(0xC00, &[addi(4, 0, 0x55), RFI]);
    run(&mut cpu, &mut mem, 4);
    assert_eq!(cpu.gpr[4], 0x55);
    assert_eq!(cpu.gpr[3], 7);
    assert_eq!(cpu.pc, 0x3008);
}

#[test]
fn decrementer_interrupt() {
    let (mut cpu, mut mem) = boot(&[b(0)]);
    mem.load_code(0x900, &[addi(30, 0, 9), b(0)]);
    cpu.msr |= MSR_EE;
    cpu.spr[SPR_DEC] = 2;
    run(&mut cpu, &mut mem, (TB_RATIO * 4) as usize);
    assert_eq!(cpu.gpr[30], 9);
    assert_eq!(cpu.spr[SPR_SRR0], 0x3000);
}

#[test]
fn floating_point_single_and_double() {
    let mut code = vec![];
    code.extend(li32(3, 0x9000));
    code.push(d(48, 1, 3, 0)); // lfs f1, 0(r3)  = 1.5
    code.push(d(48, 2, 3, 4)); // lfs f2, 4(r3)  = 0.1
    code.push(59 << 26 | 3 << 21 | 1 << 16 | 2 << 11 | 21 << 1); // fadds f3, f1, f2
    code.push(63 << 26 | 4 << 21 | 1 << 16 | 2 << 11 | 21 << 1); // fadd f4, f1, f2
    code.push(63 << 26 | 2 << 23 | 1 << 16 | 2 << 11); // fcmpu cr2, f1, f2
    code.push(63 << 26 | 5 << 21 | 4 << 11 | 15 << 1); // fctiwz f5, f4
    code.push(31 << 26 | 5 << 21 | 3 << 16 | 983 << 1); // stfiwx f5, 0, r3... (rA=3, rB=0)
    code.push(d(52, 3, 3, 8)); // stfs f3, 8(r3)
    let (mut cpu, mut mem) = boot(&code);
    mem.write32(0x9000, 1.5f32.to_bits());
    mem.write32(0x9004, 0.1f32.to_bits());
    run(&mut cpu, &mut mem, code.len());
    let single = (1.5f32 + 0.1f32) as f64;
    assert_eq!(cpu.fpr[3], [single, single], "single results fill both slots");
    assert_eq!(cpu.fpr[4][0], 1.5 + 0.1f32 as f64);
    assert_eq!(cpu.cr_field(2), 0b0100, "1.5 > 0.1");
    assert_eq!(mem.r32(0x9000), 1, "fctiwz(1.6) = 1 stored by stfiwx");
    assert_eq!(mem.r32(0x9008), (1.5f32 + 0.1f32).to_bits());
}

#[test]
fn paired_single_quantized_loads_math_and_stores() {
    let mut code = vec![];
    // GQR2: load s16 scale 8, store u8 scale 0
    code.extend(li32(3, (8 << 24) | (7 << 16) | 4));
    code.push(mtspr(914, 3));
    code.extend(li32(4, 0xA000));
    code.push(56 << 26 | 1 << 21 | 4 << 16 | 2 << 12); // psq_l f1, 0(r4), W=0, I=2
    code.push(56 << 26 | 2 << 21 | 4 << 16 | 1 << 15 | 2 << 12 | 4); // psq_l f2, 4(r4), W=1, I=2
    code.push(4 << 26 | 3 << 21 | 1 << 16 | 2 << 11 | 21 << 1); // ps_add f3, f1, f2
    code.push(4 << 26 | 4 << 21 | 1 << 16 | 2 << 11 | 560 << 1); // ps_merge01 f4, f1, f2
    code.push(4 << 26 | 5 << 21 | 1 << 16 | 2 << 11 | 3 << 6 | 10 << 1); // ps_sum0 f5, f1, f2, f3
    code.push(60 << 26 | 3 << 21 | 4 << 16 | 2 << 12 | 8); // psq_st f3, 8(r4), I=2 (u8)
    let (mut cpu, mut mem) = boot(&code);
    mem.write16(0xA000, 0x0180); // 1.5
    mem.write16(0xA002, 0xFF00); // -1.0
    mem.write16(0xA004, 0x0400); // 4.0
    run(&mut cpu, &mut mem, code.len());
    assert_eq!(cpu.fpr[1], [1.5, -1.0]);
    assert_eq!(cpu.fpr[2], [4.0, 1.0], "W=1 loads 1.0 into ps1");
    assert_eq!(cpu.fpr[3], [5.5, 0.0]);
    assert_eq!(cpu.fpr[4], [1.5, 1.0]);
    assert_eq!(cpu.fpr[5], [1.5 + 1.0, 0.0], "ps_sum0: a.ps0 + b.ps1, c.ps1");
    assert_eq!(&mem.ram[0xA008..0xA00A], &[5, 0], "u8 quantised store truncates");
}

#[test]
fn dcbz_clears_cache_block() {
    let mut code = vec![];
    code.extend(li32(3, 0xB010));
    code.push(x(0, 0, 3, 1014, 0)); // dcbz 0, r3
    let (mut cpu, mut mem) = boot(&code);
    mem.ram[0xB000..0xB040].fill(0xEE);
    run(&mut cpu, &mut mem, code.len());
    assert_eq!(mem.zeroed, vec![0xB000]);
    assert!(mem.ram[0xB000..0xB020].iter().all(|&v| v == 0));
    assert_eq!(mem.ram[0xB020], 0xEE);
}

#[test]
fn save_state_round_trip() {
    use kd_core::{SaveState, StateReader, StateWriter};
    let (mut cpu, mut mem) = boot(&[addi(3, 0, 5), mtspr(912, 3)]);
    cpu.setup_os_bats();
    cpu.pc = 0x8000_3000;
    run(&mut cpu, &mut mem, 2);
    let mut w = StateWriter::new();
    cpu.save_state(&mut w);
    let data = w.into_payload();
    let mut back = Ppc::new(PpcModel::Gekko);
    back.load_state(&mut StateReader::new(&data)).unwrap();
    assert_eq!(back.gpr, cpu.gpr);
    assert_eq!(back.gqr(0), 5);
    assert_eq!(back.dbat, cpu.dbat);
    assert_eq!(back.pc, cpu.pc);
}

// --- JIT verification through a small AArch64 reference interpreter ----------

/// Executes the AArch64 subset the JIT emits. `state` is the 32-word GPR
/// array addressed through x0.
fn run_a64(code: &[u32], state: &mut [u32; 32]) -> u32 {
    let mut x = [0u64; 32];
    let rd = |w: u32| (w & 31) as usize;
    let rn = |w: u32| ((w >> 5) & 31) as usize;
    let rm = |w: u32| ((w >> 16) & 31) as usize;
    let get = |x: &[u64; 32], r: usize| if r == 31 { 0 } else { x[r] as u32 };
    for &w in code {
        let d = rd(w);
        let res: Option<u32> = match w {
            _ if w & 0xFF80_0000 == 0x5280_0000 => Some(((w >> 5) & 0xFFFF) << (((w >> 21) & 3) * 16)),
            _ if w & 0xFF80_0000 == 0x7280_0000 => {
                let sh = ((w >> 21) & 3) * 16;
                Some((get(&x, d) & !(0xFFFF << sh)) | ((w >> 5) & 0xFFFF) << sh)
            }
            _ if w & 0xFFC0_0000 == 0xB940_0000 => {
                assert_eq!(rn(w), 0);
                Some(state[((w >> 10) & 0xFFF) as usize])
            }
            _ if w & 0xFFC0_0000 == 0xB900_0000 => {
                state[((w >> 10) & 0xFFF) as usize] = get(&x, d);
                None
            }
            _ if w == 0xD65F_03C0 => return get(&x, 0),
            _ if w & 0xFFE0_FC00 == 0x1B00_7C00 => Some(get(&x, rn(w)).wrapping_mul(get(&x, rm(w)))),
            _ if w & 0xFFE0_0000 == 0x1380_0000 => {
                assert_eq!(rn(w), rm(w));
                Some(get(&x, rn(w)).rotate_right((w >> 10) & 31))
            }
            _ if w & 0xFFFF_FC00 == 0x5AC0_1000 => Some(get(&x, rn(w)).leading_zeros()),
            _ if w & 0xFFFF_FC00 == 0x1300_1C00 => Some(get(&x, rn(w)) as i8 as i32 as u32),
            _ if w & 0xFFFF_FC00 == 0x1300_3C00 => Some(get(&x, rn(w)) as i16 as i32 as u32),
            _ if w & 0xFF80_0000 == 0x1200_0000 => {
                let immr = (w >> 16) & 63;
                let imms = (w >> 10) & 63;
                let mask = ((1u64 << (imms + 1)) - 1) as u32;
                Some(get(&x, rn(w)) & mask.rotate_right(immr))
            }
            _ => {
                let (a, b) = (get(&x, rn(w)), get(&x, rm(w)));
                assert_eq!(w & 0x0000_FC00, 0, "unshifted register form expected: {w:#010x}");
                Some(match w & 0xFFE0_0000 {
                    0x0B00_0000 => a.wrapping_add(b),
                    0x4B00_0000 => a.wrapping_sub(b),
                    0x0A00_0000 => a & b,
                    0x2A00_0000 => a | b,
                    0x4A00_0000 => a ^ b,
                    0x0A20_0000 => a & !b,
                    0x2A20_0000 => a | !b,
                    0x4A20_0000 => a ^ !b,
                    _ => panic!("unsupported AArch64 encoding {w:#010x}"),
                })
            }
        };
        if let (Some(v), true) = (res, d != 31) {
            x[d] = v as u64;
        }
    }
    panic!("block did not return");
}

#[test]
fn jit_blocks_match_the_interpreter() {
    let mut code = vec![];
    code.extend(li32(3, 0x1234_5678));
    code.push(addi(4, 3, -100));
    code.push(x(5, 3, 4, 266, 0)); // add
    code.push(x(6, 4, 5, 40, 0)); // subf
    code.push(x(5, 7, 6, 28, 0)); // and r7 = r5 & r6
    code.push(x(5, 8, 6, 124, 0)); // nor
    code.push(x(3, 9, 4, 284, 0)); // eqv
    code.push(x(9, 10, 3, 60, 0)); // andc
    code.push(x(10, 11, 3, 412, 0)); // orc
    code.push(d(7, 12, 11, 7)); // mulli
    code.push(x(12, 13, 4, 235, 0)); // mullw
    code.push(x(13, 14, 0, 104, 0)); // neg
    code.push(rlwinm(15, 14, 13, 4, 27));
    code.push(rlwinm(16, 3, 0, 28, 3)); // wrapping mask
    code.push(x(15, 17, 0, 26, 0)); // cntlzw
    code.push(x(16, 18, 0, 954, 0)); // extsb
    code.push(x(3, 19, 0, 922, 0)); // extsh
    code.push(d(27, 3, 20, 0xBEEF)); // xoris
    code.push(d(26, 20, 21, 0x1357)); // xori
    code.push(b(0x40)); // ends the block

    let fetch = |pc: u32| code.get(((pc - 0x3000) / 4) as usize).copied();
    let block = jit::compile(0x3000, fetch).expect("compiles");
    assert_eq!(block.ppc_len as usize, code.len());

    for seed in [0u32, 1, 0xDEAD_BEEF, 0x8000_0000, 0x7FFF_FFFF] {
        let (mut cpu, mut mem) = boot(&code);
        for (i, r) in cpu.gpr.iter_mut().enumerate() {
            *r = seed.wrapping_mul(i as u32 + 1).rotate_left(i as u32);
        }
        let mut regs = cpu.gpr;
        run(&mut cpu, &mut mem, code.len());
        let next = run_a64(&block.code, &mut regs);
        assert_eq!(regs, cpu.gpr, "seed {seed:#x}");
        assert_eq!(next, cpu.pc);
        assert_eq!(next, 0x3000 + (code.len() as u32 - 1) * 4 + 0x40);
    }
}

#[test]
fn jit_stops_before_unsupported_instructions() {
    let code = [addi(3, 0, 1), d(32, 4, 3, 0), addi(5, 0, 2)]; // lwz is not compiled
    let block = jit::compile(0x100, |pc| code.get(((pc - 0x100) / 4) as usize).copied()).unwrap();
    assert_eq!(block.ppc_len, 1);
    let mut regs = [0u32; 32];
    assert_eq!(run_a64(&block.code, &mut regs), 0x104);
    assert_eq!(regs[3], 1);
    assert!(jit::compile(0x100, |_| Some(d(32, 4, 3, 0))).is_none());
}
