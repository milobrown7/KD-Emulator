//! # core-gba
//!
//! Game Boy Advance emulation for KD-Emulator.
//!
//! * ARM7TDMI from `cpu-arm`, direct boot through an HLE BIOS ([`bios`]).
//! * Full 28-bit memory map with wait states and prefetch ([`bus`]).
//! * Scanline PPU: modes 0–5, affine, sprites, windows, blending ([`ppu`]).
//! * 4 PSG + 2 DirectSound FIFO channels ([`apu`]).
//! * SRAM / Flash / EEPROM backup, GPIO RTC + solar sensor, tilt ([`cart`]).
//! * Animal Crossing fit: JoyBus link port ([`sio`]) and multiboot boot for
//!   the GameCube Animal Crossing island/NES companion programs, plus a
//!   time-travel RTC driven by [`kd_core::GuestClock`].

pub mod apu;
pub mod bios;
pub mod bus;
pub mod cart;
pub mod ppu;
pub mod sio;

use bus::GbaBus;
use cart::Cartridge;
use cpu_arm::{Arm, CpuModel, MODE_SYS};
use kd_core::animal_crossing::{self, AcSetup};
use kd_core::input::button;
use kd_core::{
    EmulatorCore, FrameView, GuestClock, InputState, SaveState, StateReader, StateResult, StateWriter, StereoFrame,
    System,
};

/// Multiboot images are loaded at the start of EWRAM and entered here.
pub const MULTIBOOT_BASE: u32 = 0x0200_0000;
pub const MULTIBOOT_ENTRY: u32 = 0x0200_00C0;

pub struct Gba {
    pub cpu: Arm,
    pub bus: GbaBus,
    multiboot: bool,
    /// The Animal Crossing setup for this system (link port, clock, layout).
    pub ac_setup: AcSetup,
}

impl Gba {
    /// Boot a cartridge ROM.
    pub fn new(rom: Vec<u8>) -> Self {
        let mut gba = Self::with_cart(Cartridge::new(rom), false);
        gba.direct_boot(0x0800_0000);
        gba
    }

    /// Boot a multiboot image as if it had been received over the link
    /// cable — how the GameCube Animal Crossing sends its island and NES
    /// programs to the GBA.
    pub fn new_multiboot(image: &[u8]) -> Self {
        let mut gba = Self::with_cart(Cartridge::new(Vec::new()), true);
        let n = image.len().min(gba.bus.ewram.len());
        gba.bus.ewram[..n].copy_from_slice(&image[..n]);
        // JoyBus mode so the program can keep talking to the GameCube.
        gba.bus.sio.rcnt = 0xC000;
        gba.direct_boot(MULTIBOOT_ENTRY);
        gba
    }

    fn with_cart(cart: Cartridge, multiboot: bool) -> Self {
        let bus = GbaBus::new(cart, bios::stub_image());
        Self { cpu: Arm::new(CpuModel::Arm7Tdmi), bus, multiboot, ac_setup: animal_crossing::setup_for(System::Gba) }
    }

    /// Register state the BIOS leaves behind before jumping to the game.
    fn direct_boot(&mut self, entry: u32) {
        bios::init_stacks(&mut self.cpu);
        self.cpu.boot(&mut self.bus, entry, MODE_SYS, false);
        self.cpu.r[13] = 0x0300_7F00;
        self.bus.io_write16(0x134, 0x8000); // RCNT after BIOS
        if self.multiboot {
            self.bus.sio.rcnt = 0xC000;
            self.bus.iwram[0x7FFA] = 1; // SoftReset returns to EWRAM
        }
        self.bus.postflg = 1;
    }

    pub fn header(&self) -> &cart::Header {
        &self.bus.cart.header
    }

    /// Run until the PPU enters VBlank (one video frame).
    pub fn run_frame_inner(&mut self) {
        self.bus.frame_done = false;
        let limit = self.bus.now() + bus::CYCLES_PER_FRAME * 2;
        while !self.bus.frame_done && self.bus.now() < limit {
            if self.cpu.halted {
                if self.bus.wake_condition() {
                    self.cpu.wake();
                } else {
                    self.bus.skip_to_next_event();
                }
            } else {
                self.cpu.step(&mut self.bus);
            }
            self.bus.process_events();
            if self.bus.halt_request {
                self.bus.halt_request = false;
                self.cpu.halted = true;
            }
            self.cpu.irq_line = self.bus.irq_pending();
        }
    }

    /// Run for at least `cycles` CPU cycles (tests, link-cable sync).
    pub fn run_cycles(&mut self, cycles: u64) {
        let end = self.bus.now() + cycles;
        while self.bus.now() < end {
            if self.cpu.halted {
                if self.bus.wake_condition() {
                    self.cpu.wake();
                } else {
                    self.bus.skip_to_next_event();
                    if self.bus.now() > end {
                        break;
                    }
                }
            } else {
                self.cpu.step(&mut self.bus);
            }
            self.bus.process_events();
            if self.bus.halt_request {
                self.bus.halt_request = false;
                self.cpu.halted = true;
            }
            self.cpu.irq_line = self.bus.irq_pending();
        }
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
        let mut pressed = 0u16;
        for (b, bit) in map {
            if input.pressed(b) {
                pressed |= 1 << bit;
            }
        }
        self.bus.set_keys(pressed);
        self.bus.cart.tilt.input = input.tilt;
        self.bus.cart.gpio.light = input.light;
    }
}

impl EmulatorCore for Gba {
    fn system(&self) -> System {
        System::Gba
    }

    fn peek(&mut self, addr: u32, size: u8) -> u32 {
        use cpu_arm::{Access, Bus};
        use kd_core::cheats::read_le;
        let a = addr as usize;
        match addr >> 24 {
            0x02 => read_le(&self.bus.ewram, a & 0x3_FFFF, size),
            0x03 => read_le(&self.bus.iwram, a & 0x7FFF, size),
            _ => match size {
                1 => self.bus.read8(addr, Access::NonSeq) as u32,
                2 => self.bus.read16(addr & !1, Access::NonSeq) as u32,
                _ => self.bus.read32(addr & !3, Access::NonSeq),
            },
        }
    }

    fn poke(&mut self, addr: u32, value: u32, size: u8) {
        use cpu_arm::{Access, Bus};
        use kd_core::cheats::write_le;
        let a = addr as usize;
        match addr >> 24 {
            0x02 => write_le(&mut self.bus.ewram, a & 0x3_FFFF, value, size),
            0x03 => write_le(&mut self.bus.iwram, a & 0x7FFF, value, size),
            _ => match size {
                1 => self.bus.write8(addr, value as u8, Access::NonSeq),
                2 => self.bus.write16(addr & !1, value as u16, Access::NonSeq),
                _ => self.bus.write32(addr & !3, value, Access::NonSeq),
            },
        }
    }

    fn game_code(&self) -> [u8; 6] {
        let h = &self.bus.cart.header;
        [h.game_code[0], h.game_code[1], h.game_code[2], h.game_code[3], h.maker[0], h.maker[1]]
    }

    fn run_frame(&mut self, input: &InputState) {
        self.set_input(input);
        self.run_frame_inner();
    }

    fn frame(&self) -> FrameView<'_> {
        FrameView { width: ppu::WIDTH as u32, height: ppu::HEIGHT as u32, pixels: &self.bus.ppu.framebuffer }
    }

    fn audio_sample_rate(&self) -> u32 {
        apu::SAMPLE_RATE
    }

    fn drain_audio(&mut self, out: &mut Vec<StereoFrame>) {
        out.append(&mut self.bus.apu.out);
    }

    fn reset(&mut self) {
        let rom = std::mem::take(&mut self.bus.cart.rom);
        let backup = std::mem::take(&mut self.bus.cart.backup.data);
        let clock = self.bus.clock;
        let ewram = if self.multiboot { Some(self.bus.ewram.to_vec()) } else { None };
        *self = match ewram {
            Some(img) => Self::new_multiboot(&img),
            None => Self::new(rom),
        };
        self.bus.clock = clock;
        self.load_backup(&backup);
    }

    fn save_state(&self) -> Vec<u8> {
        let mut w = StateWriter::new();
        w.section(*b"CPU7", &self.cpu);
        w.section(*b"BUS0", &self.bus);
        w.finish(System::Gba as u8, self.game_code())
    }

    fn load_state(&mut self, data: &[u8]) -> StateResult<()> {
        let (_, mut r) = StateReader::open(data, System::Gba as u8)?;
        r.section(*b"CPU7", &mut self.cpu)?;
        r.section(*b"BUS0", &mut self.bus)?;
        Ok(())
    }

    fn backup(&self) -> Option<&[u8]> {
        let d = &self.bus.cart.backup.data;
        (!d.is_empty()).then_some(d.as_slice())
    }

    fn load_backup(&mut self, data: &[u8]) {
        let d = &mut self.bus.cart.backup.data;
        let n = d.len().min(data.len());
        d[..n].copy_from_slice(&data[..n]);
    }

    fn clock(&mut self) -> &mut GuestClock {
        &mut self.bus.clock
    }
}

impl SaveState for Gba {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.bus.save_state(w);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        self.cpu.load_state(r)?;
        self.bus.load_state(r)
    }
}
