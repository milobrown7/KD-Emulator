//! Phase 6 (Wii) verification: a Broadway program talks to IOS through the
//! Hollywood IPC mailbox.

use core_gcn::disc::Dol;
use core_wii::containers::WbfsDisc;
use core_wii::Wii;
use kd_core::animal_crossing::ControlLayout;
use kd_core::{EmulatorCore, InputState};

fn d(op: u32, rd: u32, ra: u32, imm: u32) -> u32 {
    op << 26 | rd << 21 | ra << 16 | (imm & 0xFFFF)
}
fn lis(r: u32, v: u32) -> u32 {
    d(15, r, 0, v >> 16)
}
fn ori(ra: u32, rs: u32, imm: u32) -> u32 {
    d(24, rs, ra, imm)
}
fn stw(rs: u32, ra: u32, off: i32) -> u32 {
    d(36, rs, ra, off as u32)
}
fn lwz(rd: u32, ra: u32, off: i32) -> u32 {
    d(32, rd, ra, off as u32)
}
fn andi_dot(ra: u32, rs: u32, imm: u32) -> u32 {
    d(28, rs, ra, imm)
}
fn beq(off: i32) -> u32 {
    16 << 26 | 12 << 21 | 2 << 16 | (off as u32 & 0xFFFC)
}
fn load32(c: &mut Vec<u32>, r: u32, v: u32) {
    c.push(lis(r, v));
    c.push(ori(r, r, v & 0xFFFF));
}
fn store_word(c: &mut Vec<u32>, addr: u32, v: u32) {
    load32(c, 4, v);
    load32(c, 5, addr);
    c.push(stw(4, 5, 0));
}

/// Send the request at physical `req` and wait for Y2; results land at
/// `save` (ARMMSG) and `save+4` (request result).
fn ipc_call(c: &mut Vec<u32>, req: u32, save: u32) {
    load32(c, 30, 0xCD00_0000);
    store_word(c, 0xCD00_0000, req); // PPCMSG
    store_word(c, 0xCD00_0004, 0x6); // ack Y1|Y2 left from earlier calls
    store_word(c, 0xCD00_0004, 0x1); // X1: execute
    // poll: lwz r6, 4(r30); andi. r6, r6, 2; beq poll
    c.push(lwz(6, 30, 4));
    c.push(andi_dot(6, 6, 2));
    c.push(beq(-8));
    c.push(lwz(7, 30, 8));
    load32(c, 8, 0x8000_0000 | save);
    c.push(stw(7, 8, 0));
    load32(c, 9, 0x8000_0000 | req);
    c.push(lwz(7, 9, 4));
    c.push(stw(7, 8, 4));
}

fn program() -> Vec<u32> {
    let mut c = vec![];
    // "/dev/fs\0" at 0x4000
    store_word(&mut c, 0x8000_4000, u32::from_be_bytes(*b"/dev"));
    store_word(&mut c, 0x8000_4004, u32::from_be_bytes(*b"/fs\0"));
    // open request at 0x4100: cmd 1, path 0x4000, mode 0
    store_word(&mut c, 0x8000_4100, 1);
    store_word(&mut c, 0x8000_410C, 0x4000);
    store_word(&mut c, 0x8000_4110, 0);
    ipc_call(&mut c, 0x4100, 0x4200);
    // ioctl GetStats (2) on the returned fd, output to 0x4400
    load32(&mut c, 8, 0x8000_4204);
    c.push(lwz(7, 8, 0));
    load32(&mut c, 9, 0x8000_4300);
    c.push(stw(7, 9, 8)); // fd
    store_word(&mut c, 0x8000_4300, 6);
    store_word(&mut c, 0x8000_430C, 2);
    store_word(&mut c, 0x8000_4318, 0x4400);
    store_word(&mut c, 0x8000_431C, 0x1C);
    ipc_call(&mut c, 0x4300, 0x4210);
    c.push(0x4800_0000); // b .
    c
}

#[test]
fn ios_ipc_open_and_ioctl_from_broadway() {
    let code: Vec<u8> = program().iter().flat_map(|w| w.to_be_bytes()).collect();
    let dol = Dol::build(&[(0x8000_3400, &code)], &[], 0x8000_3400);
    let mut wii = Wii::from_dol(&dol).expect("boots");
    wii.gc.cycles_per_field = 20_000;
    for _ in 0..3 {
        wii.run_frame(&InputState::default());
    }
    let w = |wii: &mut Wii, a: u32| wii.gc.bus.read_word(a);
    assert_eq!(w(&mut wii, 0x4200), 0x4100, "ARMMSG echoes the open request");
    let fd = w(&mut wii, 0x4204) as i32;
    assert!(fd >= 0, "open(/dev/fs) returned {fd}");
    assert_eq!(w(&mut wii, 0x4210), 0x4300);
    assert_eq!(w(&mut wii, 0x4214), 0, "GetStats succeeded");
    assert_eq!(w(&mut wii, 0x4400), 0x4000, "NAND cluster size");
    assert_eq!(wii.ios().requests, 2);
}

#[test]
fn city_folk_wbfs_disc_profile() {
    let mut disc = vec![0u8; 0x40_0000];
    disc[..6].copy_from_slice(b"RUUE01");
    disc[0x18..0x1C].copy_from_slice(&core_gcn::disc::WII_MAGIC.to_be_bytes());
    let wbfs = WbfsDisc::new(WbfsDisc::build(&disc, 9, 21)).unwrap();
    let mut wii = Wii::new_machine().unwrap();
    let header = wii.insert_disc(Box::new(wbfs)).unwrap();
    assert!(header.is_wii);
    let ac = wii.animal_crossing().expect("City Folk recognised");
    assert_eq!(ac.title, "Animal Crossing: City Folk");
    assert_eq!(wii.ac_setup.layout, ControlLayout::Pointer);
    assert!(wii.ios().sd_inserted, "SD card ready for photos and patterns");
    assert_eq!(wii.ios().title_id & 0xFFFF_FFFF, u32::from_be_bytes(*b"RUUE") as u64);
    // pointer input becomes IR dots in the Wii Remote report
    wii.input = InputState { pointer: Some((0.0, 0.0)), ..Default::default() };
    let report = wii.wiimote_report(0x33);
    assert_eq!(report[..2], [0xA1, 0x33]);
    assert_ne!(report[7..19], [0xFF; 12], "IR objects present");
}
