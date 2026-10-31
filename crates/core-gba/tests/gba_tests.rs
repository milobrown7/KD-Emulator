//! jsmolka's gba-tests (MIT, vendored in `tests/roms/gba-tests`).
//!
//! Each ROM runs its tests, then leaves the number of the first failure in a
//! register (0 = all passed), prints the result and idles in a tight loop.
//! Set `GBA_TEST_DUMP=<dir>` to write each final screen as raw RGBA.

use core_gba::Gba;
use kd_core::EmulatorCore;

fn run(name: &str) -> Gba {
    let path = format!("{}/tests/roms/gba-tests/{name}.gba", env!("CARGO_MANIFEST_DIR"));
    let mut gba = Gba::new(std::fs::read(&path).unwrap());
    for _ in 0..120 {
        gba.run_frame_inner();
    }
    if let Ok(dir) = std::env::var("GBA_TEST_DUMP") {
        let bytes: Vec<u8> = gba.frame().pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
        std::fs::write(format!("{dir}/{name}.rgba"), bytes).unwrap();
    }
    gba
}

/// Run a ROM and return its result register, checking it finished.
fn result(name: &str, reg: usize) -> u32 {
    let mut gba = run(name);
    let pc = gba.cpu.r[15];
    gba.run_frame_inner();
    assert_eq!(gba.cpu.r[15], pc, "{name}: still running after 121 frames");
    gba.cpu.r[reg]
}

#[test]
fn arm() {
    assert_eq!(result("arm", 12), 0, "first failed ARM test");
}

#[test]
fn thumb() {
    // The Thumb ROM keeps its result in r7.
    assert_eq!(result("thumb", 7), 0, "first failed Thumb test");
}

#[test]
fn memory() {
    assert_eq!(result("memory", 12), 0, "first failed memory test");
}

#[test]
fn bios() {
    assert_eq!(result("bios", 12), 0, "first failed BIOS test");
}

#[test]
fn nes() {
    assert_eq!(result("nes", 12), 0, "first failed NES-mode test");
}

#[test]
fn unsafe_rom() {
    assert_eq!(result("unsafe", 12), 0, "first failed unsafe test");
}

#[test]
fn saves() {
    for name in ["none", "sram", "flash64", "flash128"] {
        assert_eq!(result(name, 12), 0, "{name}: first failed save test");
    }
}

#[test]
fn ppu_demos_render() {
    // hello/shades/stripes are visual checks; make sure they draw something
    // with more than one colour and the dumps can be inspected by eye.
    for name in ["hello", "shades", "stripes"] {
        let gba = run(name);
        let colours: std::collections::HashSet<u32> = gba.frame().pixels.iter().copied().collect();
        assert!(colours.len() > 1, "{name} drew a blank screen");
    }
}
