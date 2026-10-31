use cpu_arm::asm::{self, Assembler, Dp, AL, LR, NE, PC, SP};
use cpu_arm::*;

/// Flat 1 MiB RAM at address 0, mirrored.
struct TestBus {
    mem: Vec<u8>,
    cycles: u64,
    swis: Vec<u32>,
    hle: bool,
}

impl TestBus {
    fn new() -> Self {
        Self { mem: vec![0; 1 << 20], cycles: 0, swis: Vec::new(), hle: false }
    }
    fn load(&mut self, addr: u32, bytes: &[u8]) {
        let a = addr as usize & 0xF_FFFF;
        self.mem[a..a + bytes.len()].copy_from_slice(bytes);
    }
    fn load_thumb(&mut self, addr: u32, code: &[u16]) {
        let bytes: Vec<u8> = code.iter().flat_map(|h| h.to_le_bytes()).collect();
        self.load(addr, &bytes);
    }
    fn r32(&self, addr: u32) -> u32 {
        let a = addr as usize & 0xF_FFFC;
        u32::from_le_bytes(self.mem[a..a + 4].try_into().unwrap())
    }
}

impl Bus for TestBus {
    fn read8(&mut self, addr: u32, _: Access) -> u8 {
        self.cycles += 1;
        self.mem[addr as usize & 0xF_FFFF]
    }
    fn read16(&mut self, addr: u32, _: Access) -> u16 {
        self.cycles += 1;
        let a = addr as usize & 0xF_FFFE;
        u16::from_le_bytes([self.mem[a], self.mem[a + 1]])
    }
    fn read32(&mut self, addr: u32, _: Access) -> u32 {
        self.cycles += 1;
        self.r32(addr)
    }
    fn write8(&mut self, addr: u32, v: u8, _: Access) {
        self.cycles += 1;
        self.mem[addr as usize & 0xF_FFFF] = v;
    }
    fn write16(&mut self, addr: u32, v: u16, _: Access) {
        self.cycles += 1;
        let a = addr as usize & 0xF_FFFE;
        self.mem[a..a + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn write32(&mut self, addr: u32, v: u32, _: Access) {
        self.cycles += 1;
        let a = addr as usize & 0xF_FFFC;
        self.mem[a..a + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn idle(&mut self, c: u32) {
        self.cycles += c as u64;
    }
    fn hle_swi(&mut self, cpu: &mut Arm, comment: u32) -> SwiOutcome {
        if !self.hle {
            return SwiOutcome::NotHandled;
        }
        self.swis.push(comment);
        cpu.r[0] = cpu.r[0].wrapping_mul(2);
        SwiOutcome::Handled
    }
}

const CODE: u32 = 0x1000;
/// `B .` sentinel; tests run until the CPU reaches it.
fn finish(a: &mut Assembler) -> Vec<u8> {
    a.label("__end").b(AL, "__end");
    a.finish()
}

fn run_arm(model: CpuModel, build: impl FnOnce(&mut Assembler)) -> (Arm, TestBus) {
    let mut a = Assembler::new(CODE);
    build(&mut a);
    let code = finish(&mut a);
    let end = CODE + code.len() as u32 - 4;
    let mut bus = TestBus::new();
    bus.load(CODE, &code);
    let mut cpu = Arm::new(model);
    cpu.boot(&mut bus, CODE, MODE_SYS, false);
    cpu.r[SP as usize] = 0x8000;
    run_until(&mut cpu, &mut bus, end);
    (cpu, bus)
}

fn run_until(cpu: &mut Arm, bus: &mut TestBus, end: u32) {
    for _ in 0..100_000 {
        if cpu.pc() == end && !cpu.thumb() {
            return;
        }
        cpu.step(bus);
    }
    panic!("program did not finish; pc = {:#x}", cpu.pc());
}

#[test]
fn add_sub_flags() {
    let (cpu, _) = run_arm(CpuModel::Arm7Tdmi, |a| {
        a.mov_imm(0, 0x7FFF_FFFF);
        a.dp_imm(AL, Dp::Add, true, 1, 0, 1); // overflow → N V
        a.mrs_cpsr(2);
        a.mov_imm(3, 0);
        a.dp_imm(AL, Dp::Sub, true, 4, 3, 1); // 0 - 1 → borrow (C=0), N
        a.mrs_cpsr(5);
        a.mov_imm(6, 5);
        a.dp_imm(AL, Dp::Sub, true, 7, 6, 5); // 5-5 → Z C
        a.mrs_cpsr(8);
    });
    assert_eq!(cpu.r[1], 0x8000_0000);
    assert_eq!(cpu.r[2] >> 28, 0b1001); // N . . V
    assert_eq!(cpu.r[4], 0xFFFF_FFFF);
    assert_eq!(cpu.r[5] >> 28, 0b1000); // N, no carry
    assert_eq!(cpu.r[8] >> 28, 0b0110); // Z C
}

#[test]
fn adc_sbc_64bit_chain() {
    // 0x00000001_FFFFFFFF + 0x00000000_00000001 = 0x00000002_00000000
    let (cpu, _) = run_arm(CpuModel::Arm7Tdmi, |a| {
        a.mov_imm(0, 0xFFFF_FFFF).mov_imm(1, 1).mov_imm(2, 1).mov_imm(3, 0);
        a.dp_reg(AL, Dp::Add, true, 4, 0, 2, 0, 0);
        a.dp_reg(AL, Dp::Adc, false, 5, 1, 3, 0, 0);
        // and back: subtract with borrow
        a.dp_reg(AL, Dp::Sub, true, 6, 4, 2, 0, 0);
        a.dp_reg(AL, Dp::Sbc, false, 7, 5, 3, 0, 0);
    });
    assert_eq!((cpu.r[5], cpu.r[4]), (2, 0));
    assert_eq!((cpu.r[7], cpu.r[6]), (1, 0xFFFF_FFFF));
}

#[test]
fn pc_reads_ahead() {
    let (cpu, _) = run_arm(CpuModel::Arm7Tdmi, |a| {
        a.mov(0, PC); // CODE + 8
        a.mov_imm(2, 0);
        // add r1, r2, pc, lsl r2 → register shift: PC reads +12
        a.word(0xE082_121F);
    });
    assert_eq!(cpu.r[0], CODE + 8);
    assert_eq!(cpu.r[1], CODE + 8 + 12);
}

#[test]
fn unaligned_loads_arm7_vs_arm9() {
    let program = |a: &mut Assembler| {
        a.load_const(0, 0x2000);
        a.load_const(1, 0x4433_2211);
        a.str(1, 0, 0);
        a.ldr(2, 0, 1); // rotate by 8
        a.ldrh(3, 0, 1); // ARM7: ror 8 of halfword at 0x2000
        a.word(0xE1D040F1); // ldrsh r4, [r0, #1]
    };
    let (c7, _) = run_arm(CpuModel::Arm7Tdmi, program);
    assert_eq!(c7.r[2], 0x1144_3322);
    assert_eq!(c7.r[3], 0x1100_0022);
    assert_eq!(c7.r[4], 0x22); // sign-extended byte 0x22
    let (c9, _) = run_arm(CpuModel::Arm946Es, program);
    assert_eq!(c9.r[2], 0x1144_3322);
    assert_eq!(c9.r[3], 0x2211);
    assert_eq!(c9.r[4], 0x2211);
}

#[test]
fn ldm_stm_and_stack() {
    let (cpu, bus) = run_arm(CpuModel::Arm7Tdmi, |a| {
        for r in 0..8 {
            a.mov_imm(r, r * 0x11);
        }
        a.push(0b1111_1111 | 1 << LR);
        for r in 0..8 {
            a.mov_imm(r, 0);
        }
        a.pop(0b1111_1111);
    });
    assert_eq!(cpu.r[SP as usize], 0x8000 - 4);
    for r in 0..8 {
        assert_eq!(cpu.r[r], r as u32 * 0x11);
    }
    assert_eq!(bus.r32(0x8000 - 36), 0);
    assert_eq!(bus.r32(0x8000 - 4), cpu.r[LR as usize]);
}

#[test]
fn stm_base_in_list_arm7_rule() {
    // stmia r1!, {r0, r1}: r1 is not first → ARM7 stores the new base.
    let program = |a: &mut Assembler| {
        a.load_const(1, 0x3000).mov_imm(0, 7);
        a.word(0xE8A1_0003);
    };
    let (cpu7, bus7) = run_arm(CpuModel::Arm7Tdmi, program);
    assert_eq!(cpu7.r[1], 0x3008);
    assert_eq!(bus7.r32(0x3004), 0x3008);
    let (_, bus9) = run_arm(CpuModel::Arm946Es, program);
    assert_eq!(bus9.r32(0x3004), 0x3000);
}

#[test]
fn multiplies() {
    let (cpu, _) = run_arm(CpuModel::Arm7Tdmi, |a| {
        a.mov_imm(0, 0xFFFF_FFFE); // -2
        a.mov_imm(1, 3);
        a.mul(2, 0, 1);
        a.word(0xE0C5_4190); // smull r4, r5, r0, r1
        a.word(0xE087_6190); // umull r6, r7, r0, r1
    });
    assert_eq!(cpu.r[2], (-6i32) as u32);
    assert_eq!((cpu.r[5], cpu.r[4]), (0xFFFF_FFFF, 0xFFFF_FFFA));
    assert_eq!((cpu.r[7], cpu.r[6]), (2, 0xFFFF_FFFA));
}

#[test]
fn banked_registers_survive_mode_switches() {
    let (cpu, _) = run_arm(CpuModel::Arm7Tdmi, |a| {
        a.mov_imm(SP, 0x7000);
        a.msr_cpsr_c(0xD2); // IRQ mode, IRQ/FIQ masked
        a.mov_imm(SP, 0x6000);
        a.mov_imm(LR, 0x1234);
        a.msr_cpsr_c(0xD1); // FIQ
        a.mov_imm(8, 0xF8);
        a.mov_imm(SP, 0x5000);
        a.msr_cpsr_c(0x1F); // SYS
        a.mov_imm(8, 0x08);
    });
    assert_eq!(cpu.mode(), MODE_SYS);
    assert_eq!(cpu.r[13], 0x7000);
    assert_eq!(cpu.r[8], 0x08);
    assert_eq!(cpu.banked_sp_lr(MODE_IRQ), [0x6000, 0x1234]);
    assert_eq!(cpu.banked_sp_lr(MODE_FIQ)[0], 0x5000);
}

#[test]
fn irq_entry_and_return() {
    let mut bus = TestBus::new();
    // IRQ vector: increment r10 and return.
    let mut v = Assembler::new(0x18);
    v.add_imm(10, 10, 1).irq_return();
    bus.load(0x18, &v.finish());

    let mut a = Assembler::new(CODE);
    a.mov_imm(10, 0);
    a.label("loop").add_imm(0, 0, 1).cmp_imm(0, 100).b(NE, "loop");
    let code = finish(&mut a);
    let end = CODE + code.len() as u32 - 4;
    bus.load(CODE, &code);

    let mut cpu = Arm::new(CpuModel::Arm7Tdmi);
    cpu.boot(&mut bus, CODE, MODE_SYS, false);
    cpu.set_banked_sp(MODE_IRQ, 0x7F00);
    for i in 0.. {
        if cpu.pc() == end {
            break;
        }
        // raise the line for exactly one instruction every 50 steps
        cpu.irq_line = i % 50 == 25;
        if cpu.irq_line && cpu.cpsr & FLAG_I == 0 {
            cpu.step(&mut bus);
            cpu.irq_line = false;
            continue;
        }
        cpu.step(&mut bus);
    }
    assert_eq!(cpu.r[0], 100, "loop must complete exactly despite interrupts");
    assert!(cpu.r[10] >= 2, "interrupts were taken: {}", cpu.r[10]);
    assert_eq!(cpu.mode(), MODE_SYS);
}

#[test]
fn swi_exception_and_hle_hook() {
    // Exception path
    let mut bus = TestBus::new();
    let mut v = Assembler::new(0x08);
    v.mov_imm(9, 0x99).word(0xE1B0_F00E); // movs pc, lr
    bus.load(0x08, &v.finish());
    let mut a = Assembler::new(CODE);
    a.swi(0x12).mov_imm(1, 1);
    let code = finish(&mut a);
    let end = CODE + code.len() as u32 - 4;
    bus.load(CODE, &code);
    let mut cpu = Arm::new(CpuModel::Arm7Tdmi);
    cpu.boot(&mut bus, CODE, MODE_USR, false);
    run_until(&mut cpu, &mut bus, end);
    assert_eq!((cpu.r[9], cpu.r[1]), (0x99, 1));
    assert_eq!(cpu.mode(), MODE_USR);

    // HLE path never enters the vector
    let (cpu, bus) = {
        let mut bus = TestBus::new();
        bus.hle = true;
        let mut a = Assembler::new(CODE);
        a.mov_imm(0, 21).swi(0x0B_0000);
        let code = finish(&mut a);
        let end = CODE + code.len() as u32 - 4;
        bus.load(CODE, &code);
        let mut cpu = Arm::new(CpuModel::Arm7Tdmi);
        cpu.boot(&mut bus, CODE, MODE_SYS, false);
        run_until(&mut cpu, &mut bus, end);
        (cpu, bus)
    };
    assert_eq!(cpu.r[0], 42);
    assert_eq!(bus.swis, vec![0x0B_0000]);
}

#[test]
fn thumb_program_with_bl_and_interworking() {
    let mut bus = TestBus::new();
    // ARM: switch to Thumb at 0x2000 (bit 0 set), Thumb returns with BX LR.
    let mut a = Assembler::new(CODE);
    a.load_const(0, 0x2001);
    a.mov(LR, PC); // LR = address of the instruction after bx (CODE+8+...)
    a.bx(0);
    let code = finish(&mut a);
    let end = CODE + code.len() as u32 - 4;
    bus.load(CODE, &code);

    use asm::thumb as t;
    let bl = t::bl(0x2010 - (0x2008 + 4)); // call sub at 0x2010 from 0x2008
    let thumb = [
        t::push(0, true),     // 0x2000 push {lr}
        t::mov_imm(1, 10),    // 0x2002
        t::mov_imm(2, 0),     // 0x2004
        t::mov_imm(3, 0),     // 0x2006
        bl[0],                // 0x2008
        bl[1],                // 0x200A
        t::pop(1, false),     // 0x200C pop {r0}  (the saved LR)
        t::bx(0),             // 0x200E return to ARM
        // sub: r2 = sum 1..=r1
        t::add_reg(2, 2, 1),  // 0x2010
        t::sub_imm(1, 1),     // 0x2012 subs r1, #1
        t::b_cond(1, -4),     // 0x2014 bne 0x2010
        t::bx(14),            // 0x2016 bx lr (Thumb)
    ];
    bus.load_thumb(0x2000, &thumb);

    let mut cpu = Arm::new(CpuModel::Arm7Tdmi);
    cpu.boot(&mut bus, CODE, MODE_SYS, false);
    cpu.r[13] = 0x8000;
    // `mov lr, pc` makes LR point at `b __end`'s neighbour: the instruction
    // after `bx` is the end sentinel.
    run_until(&mut cpu, &mut bus, end);
    assert_eq!(cpu.r[2], 55);
    assert!(!cpu.thumb());
}

#[test]
fn thumb_alu_flags() {
    use asm::thumb as t;
    let mut bus = TestBus::new();
    let code = [
        t::mov_imm(0, 0x80),
        t::lsl(0, 0, 24),    // 0x80000000, C=0
        t::lsl(1, 0, 1),     // 0, C=1 Z=1
        t::alu(0xF, 2, 1),   // mvn r2, r1 → 0xFFFFFFFF, N
        t::alu(0x9, 3, 2),   // neg r3, r2 → 1
        t::mov_imm(4, 3),
        t::mov_imm(5, 7),
        t::alu(0xD, 4, 5),   // mul r4, r5 → 21
        t::b(-2),            // self loop
    ];
    bus.load_thumb(0x2000, &code);
    let mut cpu = Arm::new(CpuModel::Arm7Tdmi);
    cpu.boot(&mut bus, 0x2000, MODE_SYS, true);
    for _ in 0..8 {
        cpu.step(&mut bus);
    }
    assert_eq!(cpu.r[0], 0x8000_0000);
    assert_eq!(cpu.r[1], 0);
    assert_eq!(cpu.r[2], 0xFFFF_FFFF);
    assert_eq!(cpu.r[3], 1);
    assert_eq!(cpu.r[4], 21);
}

#[test]
fn armv5_extensions() {
    let (cpu, bus) = run_arm(CpuModel::Arm946Es, |a| {
        a.mov_imm(1, 0x0001_0000);
        a.word(0xE16F_2F11); // clz r2, r1 → 15
        a.mov_imm(3, 0x7FFF_FFFF);
        a.mov_imm(4, 1);
        a.word(0xE104_5053); // qadd r5, r3, r4 → saturate
        a.mrs_cpsr(6);
        a.load_const(7, 0xFFFE_0003); // top = -2, bottom = 3
        a.word(0xE168_0787); // smulbb r8, r7, r7 → 9
        a.word(0xE169_07E7); // smultt r9, r7, r7 → 4
        a.load_const(10, 0x3000);
        a.mov_imm(0, 0x11);
        a.mov_imm(1, 0x22);
        a.word(0xE1CA_00F0); // strd r0, r1, [r10]
        a.word(0xE1CA_20D0); // ldrd r2, r3, [r10]  (overwrites r2/r3)
    });
    assert_eq!(cpu.r[5], 0x7FFF_FFFF);
    assert_ne!(cpu.r[6] & FLAG_Q, 0);
    assert_eq!(cpu.r[8], 9);
    assert_eq!(cpu.r[9], 4);
    assert_eq!((bus.r32(0x3000), bus.r32(0x3004)), (0x11, 0x22));
    assert_eq!((cpu.r[2], cpu.r[3]), (0x11, 0x22));
}

#[test]
fn armv5_blx_and_interworking_load() {
    let mut bus = TestBus::new();
    let mut a = Assembler::new(CODE);
    a.load_const(0, 0x2001);
    a.word(0xE12F_FF30); // blx r0 → Thumb at 0x2000
    a.mov_imm(5, 0x55);
    let code = finish(&mut a);
    let end = CODE + code.len() as u32 - 4;
    bus.load(CODE, &code);
    use asm::thumb as t;
    // Thumb: r4 = 0x44, then pop pc from a stack slot holding LR (bit0 = 0 → ARM on v5)
    bus.load_thumb(0x2000, &[t::mov_imm(4, 0x44), t::push(0, true), t::pop(0, true)]);
    let mut cpu = Arm::new(CpuModel::Arm946Es);
    cpu.boot(&mut bus, CODE, MODE_SYS, false);
    cpu.r[13] = 0x8000;
    run_until(&mut cpu, &mut bus, end);
    assert_eq!((cpu.r[4], cpu.r[5]), (0x44, 0x55));
}

#[test]
fn halt_and_repeat_swi() {
    struct WaitBus {
        inner: TestBus,
        flag: bool,
    }
    impl Bus for WaitBus {
        fn read8(&mut self, a: u32, x: Access) -> u8 {
            self.inner.read8(a, x)
        }
        fn read16(&mut self, a: u32, x: Access) -> u16 {
            self.inner.read16(a, x)
        }
        fn read32(&mut self, a: u32, x: Access) -> u32 {
            self.inner.read32(a, x)
        }
        fn write8(&mut self, a: u32, v: u8, x: Access) {
            self.inner.write8(a, v, x)
        }
        fn write16(&mut self, a: u32, v: u16, x: Access) {
            self.inner.write16(a, v, x)
        }
        fn write32(&mut self, a: u32, v: u32, x: Access) {
            self.inner.write32(a, v, x)
        }
        fn idle(&mut self, c: u32) {
            self.inner.idle(c)
        }
        fn hle_swi(&mut self, cpu: &mut Arm, _: u32) -> SwiOutcome {
            if self.flag {
                cpu.r[0] = 0xD0;
                SwiOutcome::Handled
            } else {
                SwiOutcome::HaltAndRepeat
            }
        }
    }
    let mut bus = WaitBus { inner: TestBus::new(), flag: false };
    let mut a = Assembler::new(CODE);
    a.swi(5).mov_imm(1, 1);
    let code = finish(&mut a);
    let end = CODE + code.len() as u32 - 4;
    bus.inner.load(CODE, &code);
    let mut cpu = Arm::new(CpuModel::Arm7Tdmi);
    cpu.boot(&mut bus, CODE, MODE_SYS, false);
    cpu.step(&mut bus);
    assert!(cpu.halted);
    cpu.step(&mut bus); // no-op while halted
    assert_eq!(cpu.pc(), CODE);
    bus.flag = true;
    cpu.wake();
    for _ in 0..3 {
        cpu.step(&mut bus);
    }
    assert_eq!(cpu.pc(), end);
    assert_eq!((cpu.r[0], cpu.r[1]), (0xD0, 1));
}

#[test]
fn save_state_round_trip() {
    use kd_core::{SaveState, StateReader, StateWriter};
    let (cpu, _) = run_arm(CpuModel::Arm7Tdmi, |a| {
        a.mov_imm(3, 0x33);
        a.msr_cpsr_c(0xD2);
        a.mov_imm(SP, 0x6000);
        a.msr_cpsr_c(0x1F);
    });
    let mut w = StateWriter::new();
    cpu.save_state(&mut w);
    let data = w.into_payload();
    let mut restored = Arm::new(CpuModel::Arm7Tdmi);
    restored.load_state(&mut StateReader::new(&data)).unwrap();
    assert_eq!(restored.r, cpu.r);
    assert_eq!(restored.cpsr, cpu.cpsr);
    assert_eq!(restored.banked_sp_lr(MODE_IRQ), [0x6000, cpu.banked_sp_lr(MODE_IRQ)[1]]);
}
