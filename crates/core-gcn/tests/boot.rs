//! Phase 6 (GameCube) verification: synthesized DOL and disc images.

use core_gcn::disc::{Dol, GC_MAGIC};
use core_gcn::GameCube;
use kd_core::animal_crossing::MemoryCardModel;
use kd_core::{EmulatorCore, InputState};

// --- tiny PowerPC assembler ---------------------------------------------------

fn d(op: u32, rd: u32, ra: u32, imm: u32) -> u32 {
    op << 26 | rd << 21 | ra << 16 | (imm & 0xFFFF)
}
fn lis(r: u32, v: u32) -> u32 {
    d(15, r, 0, v >> 16)
}
fn ori(ra: u32, rs: u32, imm: u32) -> u32 {
    d(24, rs, ra, imm)
}
fn li(r: u32, v: i32) -> u32 {
    d(14, r, 0, v as u32)
}
fn stw(rs: u32, ra: u32, off: i32) -> u32 {
    d(36, rs, ra, off as u32)
}
fn lwz(rd: u32, ra: u32, off: i32) -> u32 {
    d(32, rd, ra, off as u32)
}
fn cmpwi(ra: u32, v: i32) -> u32 {
    d(11, 0, ra, v as u32)
}
const BLR: u32 = 0x4E80_0020;
const SPIN: u32 = 0x4800_0000; // b .

fn load32(code: &mut Vec<u32>, r: u32, v: u32) {
    code.push(lis(r, v));
    code.push(ori(r, r, v & 0xFFFF));
}

fn words_to_bytes(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_be_bytes()).collect()
}

// --- GX command stream -----------------------------------------------------------

fn bp(s: &mut Vec<u8>, reg: u8, v: u32) {
    s.push(0x61);
    s.extend(((reg as u32) << 24 | (v & 0xFF_FFFF)).to_be_bytes());
}
fn cp(s: &mut Vec<u8>, reg: u8, v: u32) {
    s.extend([0x08, reg]);
    s.extend(v.to_be_bytes());
}
fn xf(s: &mut Vec<u8>, addr: u16, vals: &[u32]) {
    s.push(0x10);
    s.extend((((vals.len() as u32 - 1) << 16) | addr as u32).to_be_bytes());
    for v in vals {
        s.extend(v.to_be_bytes());
    }
}

/// Draw a green triangle and copy the EFB to an XFB at physical 0x100000.
fn gx_stream() -> Vec<u8> {
    let f = |v: f32| v.to_bits();
    let mut s = vec![];
    cp(&mut s, 0x50, 1 << 9 | 1 << 13);
    cp(&mut s, 0x70, 1 | 4 << 1 | 5 << 14);
    xf(&mut s, 0x0000, &[f(1.0), 0, 0, 0, 0, f(1.0), 0, 0, 0, 0, f(1.0), 0]);
    xf(&mut s, 0x101A, &[f(320.0), f(-240.0), f(16_777_215.0), f(662.0), f(582.0), f(16_777_215.0)]);
    xf(&mut s, 0x1020, &[f(1.0), 0, f(1.0), 0, f(-1.0), 0, 1]);
    xf(&mut s, 0x100E, &[1, 1]);
    bp(&mut s, 0x00, 0);
    bp(&mut s, 0x28, 0);
    bp(&mut s, 0xC0, 0xFFFA);
    bp(&mut s, 0xC1, 7 << 13 | 7 << 10 | 7 << 7 | 5 << 4);
    s.push(0x90);
    s.extend(3u16.to_be_bytes());
    for (x, y) in [(-0.5f32, 0.5f32), (0.5, 0.5), (0.0, -0.5)] {
        for v in [x, y, 0.0] {
            s.extend(v.to_be_bytes());
        }
        s.extend([0x00, 0xFF, 0x00, 0xFF]);
    }
    bp(&mut s, 0x49, 0);
    bp(&mut s, 0x4A, 639 | 479 << 10);
    bp(&mut s, 0x4B, 0x10_0000 >> 5);
    bp(&mut s, 0x4D, 1280 >> 5);
    bp(&mut s, 0x52, 1 << 14);
    while s.len() % 4 != 0 {
        s.push(0); // GX NOPs
    }
    s
}

/// PowerPC program: stream the GX commands into the gather pipe, point the
/// VI at the XFB, count frames in RAM and spin.
fn render_program() -> Vec<u32> {
    let mut c = vec![];
    load32(&mut c, 3, 0xCC00_8000);
    for chunk in gx_stream().chunks(4) {
        let w = u32::from_be_bytes(chunk.try_into().unwrap());
        load32(&mut c, 4, w);
        c.push(stw(4, 3, 0));
    }
    load32(&mut c, 3, 0xCC00_201C);
    load32(&mut c, 4, 1 << 28 | (0x10_0000 >> 5));
    c.push(stw(4, 3, 0)); // VI TFBL
    load32(&mut c, 3, 0x8000_4000);
    load32(&mut c, 4, 0x600D_F00D);
    c.push(stw(4, 3, 0));
    c.push(SPIN);
    c
}

fn assert_green_triangle(gc: &GameCube) {
    assert_eq!(gc.bus.last_xfb, Some((0x10_0000, 640, 480)));
    let fb = gc.frame().pixels;
    let centre = fb[240 * 640 + 320];
    let (r, g, b) = (centre & 0xFF, (centre >> 8) & 0xFF, (centre >> 16) & 0xFF);
    assert!(r < 16 && g > 235 && b < 16, "centre should be green, got {r},{g},{b}");
    let corner = fb[10 * 640 + 10];
    assert!(corner & 0xFF_FFFF < 0x10_1010, "corner should be black, got {corner:#x}");
}

#[test]
fn synthesized_dol_renders_through_gx_and_vi() {
    let code = words_to_bytes(&render_program());
    let dol = Dol::build(&[(0x8000_3100, &code)], &[], 0x8000_3100);
    let mut gc = GameCube::from_dol(&dol).expect("boots");
    gc.cycles_per_field = 20_000;
    let input = InputState::default();
    for _ in 0..3 {
        gc.run_frame(&input);
    }
    assert_eq!(gc.bus.read_word(0x4000), 0x600D_F00D, "program ran to completion");
    assert_eq!(gc.bus.gx.draws, 1);
    assert_green_triangle(&gc);
    assert_eq!(gc.bus.vi.xfb_address(), 0x10_0000);
}

/// Disc with the Animal Crossing (NTSC) ID, an apploader written in
/// PowerPC assembly, and the render program as its payload.
fn synthesized_disc() -> Vec<u8> {
    const APPLOADER: u32 = 0x8120_0000;
    const PAYLOAD_OFF: u32 = 0x1_0000;
    let payload = words_to_bytes(&render_program());

    let mut a: Vec<u32> = vec![];
    // entry(r3=&init, r4=&main, r5=&close)
    let entry_len = 2 * 3 + 3 + 1;
    let init_at = APPLOADER + entry_len * 4;
    let main_at = init_at + 4;
    let main_len = 20;
    let close_at = main_at + main_len * 4;
    for (ptr_reg, target) in [(3, init_at), (4, main_at), (5, close_at)] {
        load32(&mut a, 6, target);
        a.push(stw(6, ptr_reg, 0));
    }
    a.push(BLR);
    assert_eq!(a.len() as u32, entry_len);
    // init(report)
    a.push(BLR);
    // main(r3=&dst, r4=&len, r5=&offset): one transfer, then done
    let main_start = a.len();
    load32(&mut a, 7, 0x8130_0100);
    a.push(lwz(8, 7, 0));
    a.push(cmpwi(8, 0));
    a.push(16 << 26 | 4 << 21 | 2 << 16 | (14 * 4)); // bne → done
    a.push(li(8, 1));
    a.push(stw(8, 7, 0));
    load32(&mut a, 6, 0x8000_3100);
    a.push(stw(6, 3, 0));
    load32(&mut a, 6, payload.len() as u32);
    a.push(stw(6, 4, 0));
    load32(&mut a, 6, PAYLOAD_OFF);
    a.push(stw(6, 5, 0));
    a.push(li(3, 1));
    a.push(BLR);
    a.push(li(3, 0)); // done:
    a.push(BLR);
    assert_eq!((a.len() - main_start) as u32, main_len);
    // close() → entry point
    load32(&mut a, 3, 0x8000_3100);
    a.push(BLR);
    let body = words_to_bytes(&a);

    let mut img = vec![0u8; PAYLOAD_OFF as usize + payload.len()];
    img[..6].copy_from_slice(b"GAFE01");
    img[0x1C..0x20].copy_from_slice(&GC_MAGIC.to_be_bytes());
    img[0x20..0x2F].copy_from_slice(b"Animal Crossing");
    let h = 0x2440;
    img[h..h + 10].copy_from_slice(b"2002/09/16");
    img[h + 0x10..h + 0x14].copy_from_slice(&APPLOADER.to_be_bytes());
    img[h + 0x14..h + 0x18].copy_from_slice(&(body.len() as u32).to_be_bytes());
    img[0x2460..0x2460 + body.len()].copy_from_slice(&body);
    img[PAYLOAD_OFF as usize..].copy_from_slice(&payload);
    img
}

#[test]
fn animal_crossing_disc_boots_through_hle_apploader() {
    let mut gc = GameCube::from_disc(synthesized_disc()).expect("apploader runs");
    assert_eq!(&gc.game_code(), b"GAFE01");
    let ac = gc.animal_crossing().expect("recognised as Animal Crossing");
    assert_eq!(ac.title, "Animal Crossing");
    // Memory Card 251 in slot A
    assert_eq!(gc.ac_setup.memory_card, Some(MemoryCardModel::Card251));
    assert_eq!(gc.bus.read_word(0x0000_0000), u32::from_be_bytes(*b"GAFE"), "disc ID in low memory");
    assert_eq!(gc.cpu.pc, 0x8000_3100);
    gc.cycles_per_field = 20_000;
    for _ in 0..3 {
        gc.run_frame(&InputState::default());
    }
    assert_green_triangle(&gc);
}

#[test]
fn gba_link_on_port_two() {
    let code = words_to_bytes(&[SPIN]);
    let dol = Dol::build(&[(0x8000_3100, &code)], &[], 0x8000_3100);
    let mut gc = GameCube::from_dol(&dol).unwrap();
    gc.attach_gba(core_gba::Gba::new_multiboot(&[0u8; 0xC4]));
    // GameCube writes 0x15 (JoyBus write) with a payload through SI
    use cpu_ppc::PpcBus;
    gc.bus.write32(0x0C00_6480, 0x15DE_ADBE);
    gc.bus.write32(0x0C00_6484, 0xEF00_0000);
    gc.bus.write32(0x0C00_6434, 1 | 1 << 1 | 5 << 16 | 1 << 8);
    let port = &gc.bus.si.ports[1];
    assert!(port.joybus.is_some());
    assert_ne!(gc.bus.si.comcsr & (1 << 31), 0, "transfer completed");
}
