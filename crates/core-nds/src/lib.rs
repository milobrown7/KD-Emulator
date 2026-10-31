//! # core-nds
//!
//! Nintendo DS emulation for KD-Emulator.
//!
//! * ARM946E-S (67 MHz) and ARM7TDMI (33 MHz) from `cpu-arm`, run in
//!   lockstep slices against one master clock ([`bus`]).
//! * IPCSYNC + 16-word IPCFIFOs ([`ipc`]).
//! * 9-bank VRAM mapper ([`vram`]), two 2D engines ([`engine2d`]) and the
//!   3D geometry engine / rasteriser ([`gpu3d`]).
//! * SPI touchscreen, synthesised firmware ([`spi`]), RTC ([`rtc`]), game
//!   card and save chip ([`cart`]).
//! * HLE BIOS and direct boot — no Nintendo BIOS or firmware files needed.
//!
//! Animal Crossing fit: Wild World is recognised by its game code; it gets
//! the right FLASH save chip, a stylus-first control layout and an RTC that
//! follows the time-travel [`kd_core::GuestClock`].

#![allow(clippy::needless_range_loop)] // hardware loops index parallel arrays

pub mod bios;
pub mod bus;
pub mod cart;
pub mod engine2d;
pub mod gpu3d;
pub mod ipc;
pub mod rtc;
pub mod spi;
pub mod spu;
pub mod vram;

use bus::{Bus7, Bus9, Shared, ARM7, ARM9, FRAME_CYCLES};
use cpu_arm::cp15::{CTRL_DTCM_ENABLE, CTRL_ITCM_ENABLE};
use cpu_arm::{Access, Arm, Bus, CpuModel, MODE_IRQ, MODE_SVC, MODE_SYS};
use kd_core::animal_crossing::{self, AcSetup};
use kd_core::input::button;
use kd_core::{
    EmulatorCore, FrameView, GuestClock, InputState, StateReader, StateResult, StateWriter, StereoFrame, System,
};

/// Lockstep slice length in master (ARM9) cycles.
const SLICE: u64 = 64;
pub const SAMPLE_RATE: u32 = spu::OUTPUT_RATE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootError {
    TooSmall,
    BadBinary(&'static str),
}

impl std::fmt::Display for BootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootError::TooSmall => write!(f, "file is too small to be a DS ROM"),
            BootError::BadBinary(which) => write!(f, "{which} binary lies outside the ROM"),
        }
    }
}

impl std::error::Error for BootError {}

pub struct Nds {
    pub arm9: Arm,
    pub arm7: Arm,
    pub s: Shared,
    pub ac_setup: AcSetup,
}

impl Nds {
    /// Direct-boot a ROM (no BIOS/firmware required).
    pub fn new(rom: Vec<u8>) -> Result<Self, BootError> {
        if rom.len() < 0x200 {
            return Err(BootError::TooSmall);
        }
        let mut nds = Self {
            arm9: Arm::new(CpuModel::Arm946Es),
            arm7: Arm::new(CpuModel::Arm7Tdmi),
            s: Shared::new(rom, bios::stub9(), bios::stub7()),
            ac_setup: animal_crossing::setup_for(System::Nds),
        };
        nds.direct_boot()?;
        Ok(nds)
    }

    fn direct_boot(&mut self) -> Result<(), BootError> {
        let h = self.s.card.header.clone();
        let rom_len = self.s.card.rom.len();
        let fits = |off: u32, size: u32| (off as usize).checked_add(size as usize).is_some_and(|e| e <= rom_len);
        if !fits(h.arm9_rom_offset, h.arm9_size) {
            return Err(BootError::BadBinary("ARM9"));
        }
        if !fits(h.arm7_rom_offset, h.arm7_size) {
            return Err(BootError::BadBinary("ARM7"));
        }

        // CP15 as the firmware leaves it: DTCM at 0x03000000, ITCM enabled.
        let cp = &mut self.s.cp15;
        cp.write(0, 9, 1, 0, 0x0300_000A);
        cp.write(0, 9, 1, 1, 0x0000_0020);
        let ctrl = cp.control | CTRL_DTCM_ENABLE | CTRL_ITCM_ENABLE;
        cp.write(0, 1, 0, 0, ctrl);
        self.s.wramcnt = 3;

        let arm9_bin: Vec<u8> = self.s.card.rom[h.arm9_rom_offset as usize..][..h.arm9_size as usize].to_vec();
        let arm7_bin: Vec<u8> = self.s.card.rom[h.arm7_rom_offset as usize..][..h.arm7_size as usize].to_vec();
        {
            let mut b9 = Bus9 { s: &mut self.s };
            for (i, &byte) in arm9_bin.iter().enumerate() {
                b9.write8(h.arm9_ram_addr + i as u32, byte, Access::Seq);
            }
            // Cartridge header and boot information block.
            let header: Vec<u8> = b9.s.card.rom[..0x170].to_vec();
            for (i, &byte) in header.iter().enumerate() {
                b9.write8(0x027F_FE00 + i as u32, byte, Access::Seq);
            }
            let chip = b9.s.card.chip_id;
            for base in [0x027F_F800u32, 0x027F_FC00] {
                b9.write32(base, chip, Access::Seq);
                b9.write32(base + 4, chip, Access::Seq);
            }
            b9.write16(0x027F_F808, h.header_crc, Access::Seq);
            b9.write16(0x027F_F850, 0x5835, Access::Seq);
            b9.write16(0x027F_FC10, 0x5835, Access::Seq);
            b9.write16(0x027F_FC30, 0xFFFF, Access::Seq);
            b9.write16(0x027F_FC40, 1, Access::Seq);
            let user = spi::user_settings("KD");
            for (i, &byte) in user.iter().enumerate() {
                b9.write8(0x027F_FC80 + i as u32, byte, Access::Seq);
            }
        }
        {
            let mut b7 = Bus7 { s: &mut self.s };
            for (i, &byte) in arm7_bin.iter().enumerate() {
                b7.write8(h.arm7_ram_addr + i as u32, byte, Access::Seq);
            }
        }

        let arm9 = &mut self.arm9;
        arm9.set_banked_sp(MODE_SVC, 0x0300_3FC0);
        arm9.set_banked_sp(MODE_IRQ, 0x0300_3F80);
        arm9.boot(&mut Bus9 { s: &mut self.s }, h.arm9_entry, MODE_SYS, false);
        arm9.r[13] = 0x0300_2F7C;
        arm9.r[12] = h.arm9_entry;
        arm9.r[14] = h.arm9_entry;

        let arm7 = &mut self.arm7;
        arm7.set_banked_sp(MODE_SVC, 0x0380_FFC0);
        arm7.set_banked_sp(MODE_IRQ, 0x0380_FF80);
        arm7.boot(&mut Bus7 { s: &mut self.s }, h.arm7_entry, MODE_SYS, false);
        arm7.r[13] = 0x0380_FD80;
        arm7.r[12] = h.arm7_entry;
        arm7.r[14] = h.arm7_entry;
        self.s.time = [0, 0];
        Ok(())
    }

    /// Run both CPUs until the next VBlank.
    pub fn run_frame_inner(&mut self) {
        self.s.frame_done = false;
        let limit = self.s.sched.now() + FRAME_CYCLES * 2;
        while !self.s.frame_done && self.s.sched.now() < limit {
            let now = self.s.sched.now();
            let next = self.s.sched.next_deadline().unwrap_or(u64::MAX);
            let target = next.min(now + SLICE).max(now + 1);
            self.run_cpu(ARM9, target);
            self.run_cpu(ARM7, target);
            self.s.sched.advance_to(target);
            while let Some((at, ev)) = self.s.sched.pop_due() {
                self.s.handle_event(at, ev);
            }
        }
    }

    fn run_cpu(&mut self, cpu: usize, target: u64) {
        while self.s.time[cpu] < target {
            let arm = if cpu == ARM9 { &mut self.arm9 } else { &mut self.arm7 };
            if arm.halted {
                if self.s.wake_condition(cpu) {
                    arm.wake();
                } else {
                    self.s.time[cpu] = target;
                    break;
                }
            }
            arm.irq_line = self.s.irq_line(cpu);
            if cpu == ARM9 {
                arm.step(&mut Bus9 { s: &mut self.s });
            } else {
                arm.step(&mut Bus7 { s: &mut self.s });
            }
            if self.s.io[cpu].halt_request {
                self.s.io[cpu].halt_request = false;
                let arm = if cpu == ARM9 { &mut self.arm9 } else { &mut self.arm7 };
                arm.halted = true;
            }
        }
    }

    /// Feed host microphone audio (signed 16-bit mono, any rate near 16 kHz).
    pub fn push_mic(&mut self, samples: &[i16]) {
        self.s.spi.push_mic(samples);
    }

    pub fn set_input(&mut self, input: &InputState) {
        let map = [
            (button::A, 0),
            (button::B, 1),
            (button::SELECT, 2),
            (button::START, 3),
            (button::RIGHT, 4),
            (button::LEFT, 5),
            (button::UP, 6),
            (button::DOWN, 7),
            (button::R, 8),
            (button::L, 9),
        ];
        let pressed = map.iter().filter(|(b, _)| input.pressed(*b)).fold(0u16, |acc, (_, bit)| acc | 1 << bit);
        self.s.set_keys(pressed, input.pressed(button::X), input.pressed(button::Y), input.pressed(button::LID_CLOSED));
        self.s.set_touch(input.touch.map(|(x, y)| (x.min(255), y.min(191))));
    }
}

impl EmulatorCore for Nds {
    fn system(&self) -> System {
        System::Nds
    }

    fn peek(&mut self, addr: u32, size: u8) -> u32 {
        if addr >> 24 == 0x02 {
            return kd_core::cheats::read_le(&self.s.main_ram, addr as usize & 0x3F_FFFF, size);
        }
        let saved = self.s.time[ARM9];
        let mut b9 = Bus9 { s: &mut self.s };
        let v = match size {
            1 => b9.read8(addr, Access::NonSeq) as u32,
            2 => b9.read16(addr & !1, Access::NonSeq) as u32,
            _ => b9.read32(addr & !3, Access::NonSeq),
        };
        self.s.time[ARM9] = saved;
        v
    }

    fn poke(&mut self, addr: u32, value: u32, size: u8) {
        if addr >> 24 == 0x02 {
            return kd_core::cheats::write_le(&mut self.s.main_ram, addr as usize & 0x3F_FFFF, value, size);
        }
        let saved = self.s.time[ARM9];
        let mut b9 = Bus9 { s: &mut self.s };
        match size {
            1 => b9.write8(addr, value as u8, Access::NonSeq),
            2 => b9.write16(addr & !1, value as u16, Access::NonSeq),
            _ => b9.write32(addr & !3, value, Access::NonSeq),
        }
        self.s.time[ARM9] = saved;
    }

    fn push_mic(&mut self, samples: &[i16]) {
        self.s.spi.push_mic(samples);
    }

    fn game_code(&self) -> [u8; 6] {
        let h = &self.s.card.header;
        [h.game_code[0], h.game_code[1], h.game_code[2], h.game_code[3], h.maker[0], h.maker[1]]
    }

    fn run_frame(&mut self, input: &InputState) {
        self.set_input(input);
        self.run_frame_inner();
    }

    fn frame(&self) -> FrameView<'_> {
        FrameView { width: 256, height: 384, pixels: &self.s.framebuffer }
    }

    fn audio_sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn drain_audio(&mut self, out: &mut Vec<StereoFrame>) {
        out.append(&mut self.s.spu.out);
    }

    fn reset(&mut self) {
        let rom = std::mem::take(&mut self.s.card.rom);
        let save = std::mem::take(&mut self.s.card.save.data);
        let clock = self.s.clock;
        if let Ok(fresh) = Nds::new(rom) {
            *self = fresh;
            self.s.clock = clock;
            self.load_backup(&save);
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let mut w = StateWriter::new();
        w.section(*b"ARM9", &self.arm9);
        w.section(*b"ARM7", &self.arm7);
        w.section(*b"SHRD", &self.s);
        w.finish(System::Nds as u8, self.game_code())
    }

    fn load_state(&mut self, data: &[u8]) -> StateResult<()> {
        let (_, mut r) = StateReader::open(data, System::Nds as u8)?;
        r.section(*b"ARM9", &mut self.arm9)?;
        r.section(*b"ARM7", &mut self.arm7)?;
        r.section(*b"SHRD", &mut self.s)?;
        Ok(())
    }

    fn backup(&self) -> Option<&[u8]> {
        let d = &self.s.card.save.data;
        (!d.is_empty()).then_some(d.as_slice())
    }

    fn load_backup(&mut self, data: &[u8]) {
        self.s.card.load_save(data);
    }

    fn clock(&mut self) -> &mut GuestClock {
        &mut self.s.clock
    }
}
