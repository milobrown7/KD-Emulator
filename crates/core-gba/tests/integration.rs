//! Programmatic GBA ROM integration tests (no commercial ROMs or BIOS).

use core_gba::cart::Header;
use core_gba::ppu::{bgr555_to_rgba, WIDTH};
use core_gba::Gba;
use cpu_arm::asm::{Assembler, Dp, AL, NE};
use kd_core::input::button;
use kd_core::{EmulatorCore, InputState};

const ROM_CODE: u32 = 0x0800_00C0;

/// Build a cartridge: branch at 0x08000000, valid header, code at 0xC0.
fn build_rom(extra: &[u8], code: impl FnOnce(&mut Assembler)) -> Vec<u8> {
    let mut rom = vec![0u8; 0xC0];
    let mut head = Assembler::new(0x0800_0000);
    head.b_abs(AL, ROM_CODE);
    rom[..4].copy_from_slice(&head.finish());
    rom[0xA0..0xA6].copy_from_slice(b"KDTEST");
    rom[0xAC..0xB0].copy_from_slice(b"AKDE");
    rom[0xB0..0xB2].copy_from_slice(b"01");
    rom[0xB2] = 0x96;
    rom[0xBD] = Header::complement(&rom);
    let mut a = Assembler::new(ROM_CODE);
    code(&mut a);
    rom.extend(a.finish());
    rom.extend_from_slice(extra);
    rom
}

/// Standard VBlank IRQ handler: acknowledge IF and BIOS_IF, then return to
/// the BIOS dispatcher. Also counts interrupts in IWRAM 0x03000010.
fn irq_handler(a: &mut Assembler) {
    a.label("irq");
    a.mov_imm(0, 0x0400_0000);
    a.add_imm(2, 0, 0x200);
    a.ldrh(1, 2, 2); // IF
    a.strh(1, 2, 2); // acknowledge everything pending
    a.load_const(3, 0x0300_7FF8);
    a.ldrh(2, 3, 0);
    a.dp_reg(AL, Dp::Orr, false, 2, 2, 1, 0, 0);
    a.strh(2, 3, 0);
    a.load_const(3, 0x0300_0010);
    a.ldr(2, 3, 0);
    a.add_imm(2, 2, 1);
    a.str(2, 3, 0);
    a.bx(14);
}

/// Program from the spec's Phase 2 verification: mode 3, VBlank IRQ through
/// the HLE BIOS, one red pixel per frame, frame counter in IWRAM.
fn sixty_frame_rom() -> Vec<u8> {
    // The handler lives at a fixed offset so its address can be loaded with
    // load_const.
    const HANDLER: u32 = 0x0800_0400;
    let mut rom = build_rom(&[], |a| {
        a.load_const(0, 0x0300_7FFC);
        a.load_const(1, HANDLER);
        a.str(1, 0, 0);
        a.mov_imm(0, 0x0400_0000);
        a.load_const(1, 0x0403);
        a.strh(1, 0, 0); // DISPCNT: mode 3 + BG2
        a.mov_imm(1, 8);
        a.strh(1, 0, 4); // DISPSTAT: VBlank IRQ
        a.add_imm(2, 0, 0x200);
        a.mov_imm(1, 1);
        a.strh(1, 2, 0); // IE = VBlank
        a.strh(1, 2, 8); // IME = 1
        a.mov_imm(8, 0);
        a.label("loop");
        a.swi(0x05_0000); // VBlankIntrWait
        a.add_imm(8, 8, 1);
        a.load_const(3, 0x0600_0000 + 10 * 480);
        a.dp_reg(AL, Dp::Add, false, 3, 3, 8, 0, 1); // + r8 * 2
        a.mov_imm(4, 0x1F);
        a.strh(4, 3, 0);
        a.load_const(5, 0x0300_0000);
        a.str(8, 5, 0);
        a.cmp_imm(8, 60);
        a.b(NE, "loop");
        a.label("done");
        a.b(AL, "done");
    });
    rom.resize((HANDLER - 0x0800_0000) as usize, 0);
    let mut h = Assembler::new(HANDLER);
    irq_handler(&mut h);
    rom.extend(h.finish());
    rom
}

fn iwram32(gba: &Gba, off: usize) -> u32 {
    u32::from_le_bytes(gba.bus.iwram[off..off + 4].try_into().unwrap())
}

#[test]
fn sixty_frames_of_vblank_driven_drawing() {
    let mut gba = Gba::new(sixty_frame_rom());
    assert_eq!(&gba.game_code(), b"AKDE01");
    assert!(gba.header().checksum_ok);
    let input = InputState::default();
    for _ in 0..62 {
        gba.run_frame(&input);
    }
    assert_eq!(iwram32(&gba, 0), 60, "frame counter");
    let irqs = iwram32(&gba, 0x10);
    assert!((60..=62).contains(&irqs), "VBlank IRQs taken: {irqs}");
    let red = bgr555_to_rgba(0x001F);
    let black = bgr555_to_rgba(0);
    let fb = gba.frame().pixels;
    for x in 1..=60 {
        assert_eq!(fb[10 * WIDTH + x], red, "pixel {x}");
    }
    assert_eq!(fb[10 * WIDTH + 61], black);
    assert_eq!(fb[11 * WIDTH + 5], black);
    // The CPU sits in the final `b done` loop, not halted.
    assert!(!gba.cpu.halted);
}

#[test]
fn halted_cpu_skips_time_quickly() {
    // With the CPU halted between VBlanks, a frame should take very few
    // executed instructions.
    let mut gba = Gba::new(sixty_frame_rom());
    let input = InputState::default();
    gba.run_frame(&input);
    let before = gba.cpu.retired;
    gba.run_frame(&input);
    let per_frame = gba.cpu.retired - before;
    assert!(per_frame < 200, "{per_frame} instructions in a halted frame");
}

#[test]
fn immediate_dma_copies_rom_to_ewram() {
    const DATA: u32 = 0x0800_0800;
    let mut rom = build_rom(&[], |a| {
        a.load_const(0, 0x0400_00D4); // DMA3SAD
        a.load_const(1, DATA);
        a.str(1, 0, 0);
        a.load_const(1, 0x0200_1000);
        a.str(1, 0, 4);
        a.load_const(1, 0x8400_0010); // enable, 32-bit, 16 units
        a.str(1, 0, 8);
        a.label("x");
        a.b(AL, "x");
    });
    rom.resize((DATA - 0x0800_0000) as usize, 0);
    for i in 0..16u32 {
        rom.extend((i * 0x0101_0101).to_le_bytes());
    }
    let mut gba = Gba::new(rom);
    gba.run_cycles(10_000);
    for i in 0..16usize {
        let v = u32::from_le_bytes(gba.bus.ewram[0x1000 + i * 4..0x1004 + i * 4].try_into().unwrap());
        assert_eq!(v, i as u32 * 0x0101_0101);
    }
    assert_eq!(gba.bus.dma[3].cnt_h & 0x8000, 0, "non-repeating DMA disables itself");
}

#[test]
fn timer_cascade_and_irq() {
    const HANDLER: u32 = 0x0800_0400;
    let mut rom = build_rom(&[], |a| {
        a.load_const(0, 0x0300_7FFC);
        a.load_const(1, HANDLER);
        a.str(1, 0, 0);
        a.mov_imm(0, 0x0400_0000);
        a.add_imm(2, 0, 0x200);
        a.mov_imm(1, 0x10); // IE = timer 1
        a.strh(1, 2, 0);
        a.mov_imm(1, 1);
        a.strh(1, 2, 8);
        // TM1: cascade, IRQ, reload 0xFFFC (overflows every 4 TM0 overflows)
        a.add_imm(3, 0, 0x100);
        a.load_const(1, 0xFFFC);
        a.strh(1, 3, 4);
        a.mov_imm(1, 0xC4);
        a.strh(1, 3, 6);
        // TM0: reload 0xFF00, prescaler 1, start
        a.load_const(1, 0xFF00);
        a.strh(1, 3, 0);
        a.mov_imm(1, 0x80);
        a.strh(1, 3, 2);
        a.label("spin");
        a.b(AL, "spin");
    });
    rom.resize((HANDLER - 0x0800_0000) as usize, 0);
    let mut h = Assembler::new(HANDLER);
    irq_handler(&mut h);
    rom.extend(h.finish());
    let mut gba = Gba::new(rom);
    // TM0 overflows every 256 cycles; TM1 every 4 of those = 1024 cycles.
    gba.run_cycles(1024 * 20 + 600);
    let irqs = iwram32(&gba, 0x10);
    assert!((19..=21).contains(&irqs), "timer IRQs: {irqs}");
}

#[test]
fn keypad_is_active_low() {
    let rom = build_rom(&[], |a| {
        a.mov_imm(0, 0x0400_0000);
        a.load_const(5, 0x0300_0000);
        a.label("poll");
        a.add_imm(1, 0, 0x100);
        a.ldrh(2, 1, 0x30);
        a.str(2, 5, 0);
        a.b(AL, "poll");
    });
    let mut gba = Gba::new(rom);
    let input = InputState { buttons: button::A | button::START, ..Default::default() };
    gba.run_frame(&input);
    assert_eq!(iwram32(&gba, 0), 0x3FF & !0b1001);
}

#[test]
fn sram_backup_written_by_game() {
    let mut extra = b"SRAM_V113".to_vec();
    extra.resize(16, 0);
    let rom = build_rom(&extra, |a| {
        a.load_const(0, 0x0E00_0000);
        a.mov_imm(1, 0x4B);
        a.strb(1, 0, 0x12);
        a.mov_imm(1, 0x44);
        a.strb(1, 0, 0x13);
        a.label("x");
        a.b(AL, "x");
    });
    let mut gba = Gba::new(rom);
    gba.run_cycles(5_000);
    let save = gba.backup().unwrap();
    assert_eq!(save.len(), 0x8000);
    assert_eq!(&save[0x12..0x14], b"KD");
}

#[test]
fn save_state_is_deterministic() {
    let mut a = Gba::new(sixty_frame_rom());
    let input = InputState::default();
    for _ in 0..20 {
        a.run_frame(&input);
    }
    let state = a.save_state();
    for _ in 0..25 {
        a.run_frame(&input);
    }
    let mut b = Gba::new(sixty_frame_rom());
    b.load_state(&state).unwrap();
    for _ in 0..25 {
        b.run_frame(&input);
    }
    assert_eq!(a.bus.iwram, b.bus.iwram);
    assert_eq!(a.frame().pixels, b.frame().pixels);
    assert_eq!(a.cpu.r, b.cpu.r);
    assert_eq!(a.bus.now(), b.bus.now());
}

#[test]
fn multiboot_image_boots_from_ewram_in_joybus_mode() {
    // Multiboot header: 0xC0 bytes, code at 0x020000C0.
    let mut image = vec![0u8; 0xC0];
    let mut code = Assembler::new(0x0200_00C0);
    code.load_const(0, 0x0300_0020);
    code.load_const(1, 0xAC1D_2001);
    code.str(1, 0, 0);
    code.label("x");
    code.b(AL, "x");
    image.extend(code.finish());
    let mut gba = Gba::new_multiboot(&image);
    assert!(gba.bus.sio.joybus_mode());
    gba.run_cycles(2_000);
    assert_eq!(iwram32(&gba, 0x20), 0xAC1D_2001);
    // The GameCube side can now talk to it.
    let resp = gba.bus.sio.host_command(core_gba::sio::CMD_STATUS, 0);
    assert_eq!(&resp[..2], &[0x00, 0x04]);
    assert!(gba.ac_setup.joybus_client);
}

#[test]
fn bios_div_and_cpuset() {
    let rom = build_rom(&[], |a| {
        a.load_const(0, (-100i32) as u32);
        a.mov_imm(1, 7);
        a.swi(0x06_0000); // Div
        a.load_const(5, 0x0300_0000);
        a.str(0, 5, 0);
        a.str(1, 5, 4);
        a.str(3, 5, 8);
        // CpuSet fill 8 halfwords of 0xBEEF into EWRAM
        a.load_const(0, 0x0300_0040);
        a.load_const(1, 0xBEEF);
        a.strh(1, 0, 0);
        a.load_const(1, 0x0200_0000);
        a.load_const(2, (1 << 24) | 8);
        a.swi(0x0B_0000);
        a.label("x");
        a.b(AL, "x");
    });
    let mut gba = Gba::new(rom);
    gba.run_cycles(20_000);
    assert_eq!(iwram32(&gba, 0) as i32, -14);
    assert_eq!(iwram32(&gba, 4) as i32, -2);
    assert_eq!(iwram32(&gba, 8), 14);
    for i in 0..8 {
        assert_eq!(u16::from_le_bytes([gba.bus.ewram[i * 2], gba.bus.ewram[i * 2 + 1]]), 0xBEEF);
    }
    assert_eq!(gba.bus.ewram[16], 0);
}

#[test]
fn lz77_decompression() {
    // "ABCABCABCABC" (12 bytes): literals A B C, then a back-reference
    // (len 9, disp 3).
    const DATA: u32 = 0x0800_0800;
    let mut rom = build_rom(&[], |a| {
        a.load_const(0, DATA);
        a.load_const(1, 0x0200_0100);
        a.swi(0x11_0000);
        a.label("x");
        a.b(AL, "x");
    });
    rom.resize((DATA - 0x0800_0000) as usize, 0);
    let mut lz = vec![0x10, 12, 0, 0];
    lz.push(0b0001_0000); // 3 literals then one reference
    lz.extend_from_slice(b"ABC");
    lz.push(((9 - 3) << 4) as u8);
    lz.push(2); // disp - 1
    rom.extend(lz);
    let mut gba = Gba::new(rom);
    gba.run_cycles(20_000);
    assert_eq!(&gba.bus.ewram[0x100..0x10C], b"ABCABCABCABC");
}
