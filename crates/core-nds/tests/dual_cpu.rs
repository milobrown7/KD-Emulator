//! Phase 3 verification: a DS ROM assembled in the test. The ARM9 renders a
//! red 3D triangle every frame (VBlank-synchronised through the HLE BIOS)
//! and sends a value to the ARM7 over the IPC FIFO; the ARM7 answers through
//! shared main RAM and IPCSYNC.

use core_nds::bus::ARM9;
use core_nds::engine2d::bgr555_to_rgba;
use core_nds::Nds;
use cpu_arm::asm::{Assembler, Dp, AL, NE};
use kd_core::{EmulatorCore, InputState};

const ARM9_BASE: u32 = 0x0200_0000;
const ARM7_BASE: u32 = 0x0380_0000;
const HANDLER9: u32 = 0x0200_0800;

fn fx(v: f32) -> u32 {
    ((v * 4096.0) as i32) as u32 & 0xFFFF
}

fn store(a: &mut Assembler, addr: u32, value: u32) {
    a.load_const(0, addr);
    a.load_const(1, value);
    a.str(1, 0, 0);
}

fn arm9_program() -> Vec<u8> {
    let mut a = Assembler::new(ARM9_BASE);
    store(&mut a, 0x0300_3FFC, HANDLER9); // IRQ handler pointer in DTCM
    store(&mut a, 0x0400_0000, 0x0001_0108); // DISPCNT A: display on, BG0 = 3D
    store(&mut a, 0x0400_0060, 0); // DISP3DCNT
    store(&mut a, 0x0400_0004, 8); // DISPSTAT: VBlank IRQ
    store(&mut a, 0x0400_0210, 1); // IE = VBlank
    store(&mut a, 0x0400_0208, 1); // IME
    store(&mut a, 0x0400_0184, 0x8000); // IPC FIFO on
    store(&mut a, 0x0400_0188, 41); // send 41 to the ARM7
    store(&mut a, 0x0400_0350, 0x001F_7C00); // clear: blue, opaque
    store(&mut a, 0x0400_0354, 0x7FFF);
    a.label("frame");
    store(&mut a, 0x0400_0440, 0); // MTX_MODE projection
    store(&mut a, 0x0400_0454, 0); // IDENTITY
    store(&mut a, 0x0400_0440, 2); // MTX_MODE position & vector
    store(&mut a, 0x0400_0454, 0);
    store(&mut a, 0x0400_0580, 0xBFFF_0000); // VIEWPORT 0,0,255,191
    store(&mut a, 0x0400_04A4, 0x001F_00C0); // POLYGON_ATTR
    store(&mut a, 0x0400_0500, 0); // BEGIN_VTXS triangles
    store(&mut a, 0x0400_0480, 0x001F); // COLOR red
    for (x, y) in [(-0.5f32, -0.5f32), (0.5, -0.5), (0.0, 0.5)] {
        store(&mut a, 0x0400_048C, fx(x) | fx(y) << 16);
        store(&mut a, 0x0400_048C, 0);
    }
    store(&mut a, 0x0400_0504, 0); // END_VTXS
    store(&mut a, 0x0400_0540, 0); // SWAP_BUFFERS
    a.swi(0x05_0000); // VBlankIntrWait
    a.load_const(0, 0x0220_0010);
    a.ldr(1, 0, 0);
    a.add_imm(1, 1, 1);
    a.str(1, 0, 0);
    a.b(AL, "frame");
    let mut code = a.finish();
    code.resize((HANDLER9 - ARM9_BASE) as usize, 0);

    let mut h = Assembler::new(HANDLER9);
    h.mov_imm(0, 0x0400_0000);
    h.add_imm(0, 0, 0x210);
    h.ldr(1, 0, 4); // IF
    h.str(1, 0, 4); // acknowledge
    h.load_const(2, 0x0300_3FF8); // BIOS_IF in DTCM
    h.ldr(3, 2, 0);
    h.dp_reg(AL, Dp::Orr, false, 3, 3, 1, 0, 0);
    h.str(3, 2, 0);
    h.bx(14);
    code.extend(h.finish());
    code
}

fn arm7_program() -> Vec<u8> {
    let mut a = Assembler::new(ARM7_BASE);
    a.mov_imm(0, 0x0400_0000);
    a.load_const(1, 0x8000);
    a.add_imm(2, 0, 0x100);
    a.strh(1, 2, 0x84); // IPCFIFOCNT: enable
    a.label("wait");
    a.ldrh(1, 2, 0x84);
    a.dp_imm(AL, Dp::Tst, true, 0, 1, 0x100); // receive FIFO empty?
    a.b(NE, "wait");
    a.load_const(2, 0x0410_0000);
    a.ldr(3, 2, 0); // IPCFIFORECV
    a.add_imm(3, 3, 1);
    a.load_const(4, 0x0220_0000);
    a.str(3, 4, 0);
    a.load_const(1, 0x0700);
    a.load_const(2, 0x0400_0180);
    a.strh(1, 2, 0); // IPCSYNC output = 7
    a.label("spin");
    a.b(AL, "spin");
    a.finish()
}

fn build_rom(game_code: &[u8; 4]) -> Vec<u8> {
    let arm9 = arm9_program();
    let arm7 = arm7_program();
    let mut rom = vec![0u8; 0x200];
    rom[..8].copy_from_slice(b"KDTEST3D");
    rom[0x0C..0x10].copy_from_slice(game_code);
    rom[0x10..0x12].copy_from_slice(b"01");
    let put = |rom: &mut Vec<u8>, at: usize, v: u32| rom[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut rom, 0x20, 0x200);
    put(&mut rom, 0x24, ARM9_BASE);
    put(&mut rom, 0x28, ARM9_BASE);
    put(&mut rom, 0x2C, arm9.len() as u32);
    let arm7_off = (0x200 + arm9.len()).next_multiple_of(0x200);
    put(&mut rom, 0x30, arm7_off as u32);
    put(&mut rom, 0x34, ARM7_BASE);
    put(&mut rom, 0x38, ARM7_BASE);
    put(&mut rom, 0x3C, arm7.len() as u32);
    rom.extend(&arm9);
    rom.resize(arm7_off, 0);
    rom.extend(&arm7);
    rom.resize(rom.len().next_multiple_of(0x1000), 0);
    rom
}

fn ram32(nds: &Nds, addr: u32) -> u32 {
    let a = (addr & 0x3F_FFFF) as usize;
    u32::from_le_bytes(nds.s.main_ram[a..a + 4].try_into().unwrap())
}

#[test]
fn dual_cpu_3d_triangle() {
    let mut nds = Nds::new(build_rom(b"KD3E")).expect("boots");
    assert!(nds.animal_crossing().is_none());
    let input = InputState::default();
    for _ in 0..6 {
        nds.run_frame(&input);
    }
    // ARM7 received 41 from the ARM9 over the FIFO and answered 42.
    assert_eq!(ram32(&nds, 0x0220_0000), 42);
    assert_eq!(nds.s.ipc.read_sync(ARM9) & 0xF, 7);
    // ARM9 woke from VBlankIntrWait every frame.
    let frames = ram32(&nds, 0x0220_0010);
    assert!((4..=6).contains(&frames), "frames counted: {frames}");
    // The 3D engine drew the triangle and engine A composited it on top.
    assert!(nds.s.gpu3d.rendered_polygons >= 1);
    let fb = nds.frame().pixels;
    let red = bgr555_to_rgba(0x001F);
    let blue = bgr555_to_rgba(0x7C00);
    assert_eq!(fb[96 * 256 + 128], red, "triangle centre");
    assert_eq!(fb[5 * 256 + 5], blue, "clear colour");
    assert_eq!(fb[60 * 256 + 128], red, "near the apex");
    assert_eq!(fb[60 * 256 + 70], blue);
    // Engine B is switched off (DISPCNT display mode 0): white bottom screen.
    assert_eq!(fb[(192 + 10) * 256 + 10], 0xFFFF_FFFF);
}

#[test]
fn save_state_round_trip_is_deterministic() {
    let input = InputState::default();
    let mut a = Nds::new(build_rom(b"KD3E")).unwrap();
    for _ in 0..3 {
        a.run_frame(&input);
    }
    let state = a.save_state();
    for _ in 0..3 {
        a.run_frame(&input);
    }
    let mut b = Nds::new(build_rom(b"KD3E")).unwrap();
    b.load_state(&state).unwrap();
    for _ in 0..3 {
        b.run_frame(&input);
    }
    assert_eq!(a.frame().pixels, b.frame().pixels);
    assert_eq!(a.s.main_ram, b.s.main_ram);
    assert_eq!(a.arm9.r, b.arm9.r);
    assert_eq!(a.arm7.r, b.arm7.r);
}

#[test]
fn animal_crossing_wild_world_profile() {
    use kd_core::animal_crossing::ControlLayout;
    use kd_core::clock::DateTime;
    let mut nds = Nds::new(build_rom(b"ADME")).unwrap();
    let ac = nds.animal_crossing().expect("Wild World is recognised");
    assert_eq!(ac.title, "Animal Crossing: Wild World");
    assert_eq!(nds.ac_setup.layout, ControlLayout::StylusFirst);
    assert_eq!(nds.backup().unwrap().len(), 0x40000, "256 KiB FLASH save");
    // Time travel moves the RTC the game reads.
    let xmas = DateTime { year: 2005, month: 12, day: 24, hour: 20, minute: 0, second: 0, weekday: 6 };
    nds.clock().travel_to(xmas);
    assert_eq!(nds.s.clock.now().day, 24);
}

#[test]
fn rejects_truncated_roms() {
    assert!(Nds::new(vec![0; 16]).is_err());
    let mut rom = build_rom(b"KD3E");
    rom[0x2C..0x30].copy_from_slice(&0x0100_0000u32.to_le_bytes());
    assert!(Nds::new(rom).is_err());
}
