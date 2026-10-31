//! # core-gcn
//!
//! Nintendo GameCube emulation for KD-Emulator: Gekko (`cpu-ppc`) on the
//! Flipper bus ([`bus`]), GX graphics (`gpu-gx-vk`), the audio DSP
//! (`dsp-gc`), DVD (ISO/GCM) with an HLE apploader so no IPL is needed,
//! controllers and the GBA link port ([`si`]), and EXI devices — RTC, SRAM
//! and memory cards ([`exi`]).
//!
//! Animal Crossing fit: GAFE01/GAFJ01/GAFP01/GAFU01/GAEJ01 boot with a
//! Memory Card 251 in slot A, the RTC driven by the time-travel
//! [`GuestClock`], and port 2 ready for a linked GBA running the island
//! or NES programs ([`GameCube::attach_gba`]).

pub mod bus;
pub mod disc;
pub mod exi;
pub mod si;

use bus::{FlipperBus, VideoInterface};
use cpu_ppc::cpu::{MSR_EE, MSR_FP, SPR_HID2};
use cpu_ppc::{Ppc, PpcBus, PpcModel};
use disc::{ApploaderHeader, Disc, DiscHeader, Dol, IsoDisc};
use exi::{IplDevice, MemoryCard};
use kd_core::animal_crossing::{self, AcSetup, MemoryCardModel};
use kd_core::{EmulatorCore, FrameView, GuestClock, InputState, StateResult, StereoFrame, System};

/// Gekko core clock / 60 fields.
pub const DEFAULT_CYCLES_PER_FIELD: u64 = 486_000_000 / 60;
/// Return address used when the HLE boot calls into guest code.
const HLE_RETURN: u32 = 0x8130_0000;

#[derive(Debug)]
pub enum BootError {
    Io(std::io::Error),
    BadDol,
    NotGameCube,
    ApploaderTimeout,
}

impl std::fmt::Display for BootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootError::Io(e) => write!(f, "I/O error: {e}"),
            BootError::BadDol => write!(f, "not a valid DOL executable"),
            BootError::NotGameCube => write!(f, "not a GameCube disc"),
            BootError::ApploaderTimeout => write!(f, "apploader did not finish"),
        }
    }
}

impl std::error::Error for BootError {}

impl From<std::io::Error> for BootError {
    fn from(e: std::io::Error) -> Self {
        BootError::Io(e)
    }
}

pub struct GameCube {
    pub cpu: Ppc,
    pub bus: FlipperBus,
    pub game_id: [u8; 6],
    pub ac_setup: AcSetup,
    pub cycles_per_field: u64,
    framebuffer: Vec<u32>,
    audio: Vec<StereoFrame>,
    pub fields: u64,
}

impl GameCube {
    /// A powered-on console with no software loaded (`model` selects
    /// Gekko or Broadway for the Wii core).
    pub fn new_machine(model: PpcModel) -> Result<Self, BootError> {
        let mut bus = FlipperBus::new()?;
        bus.exi[0].devices[1] = Some(Box::new(IplDevice::new()));
        let mut cpu = Ppc::new(model);
        cpu.write_spr(SPR_HID2, 0xE000_0000); // paired singles + locked cache
        Ok(Self {
            cpu,
            bus,
            game_id: [0; 6],
            ac_setup: animal_crossing::setup_for(System::GameCube),
            cycles_per_field: DEFAULT_CYCLES_PER_FIELD,
            framebuffer: vec![0xFF00_0000; 640 * 480],
            audio: Vec::new(),
            fields: 0,
        })
    }

    /// Boot a DOL executable directly.
    pub fn from_dol(data: &[u8]) -> Result<Self, BootError> {
        let mut gc = Self::new_machine(PpcModel::Gekko)?;
        gc.boot_dol(data)?;
        Ok(gc)
    }

    /// Boot a disc image (.iso/.gcm) through the HLE apploader.
    pub fn from_disc(image: Vec<u8>) -> Result<Self, BootError> {
        Self::boot_disc(Box::new(IsoDisc::new(image)))
    }

    /// Boot any [`Disc`] (e.g. one streamed from a file descriptor).
    pub fn boot_disc(disc: Box<dyn Disc>) -> Result<Self, BootError> {
        let mut gc = Self::new_machine(PpcModel::Gekko)?;
        gc.insert_disc(disc)?;
        gc.run_apploader()?;
        Ok(gc)
    }

    pub fn insert_disc(&mut self, mut disc: Box<dyn Disc>) -> Result<DiscHeader, BootError> {
        let header = DiscHeader::read(disc.as_mut())?;
        self.game_id = header.game_id;
        self.bus.disc = Some(disc);
        self.apply_animal_crossing();
        Ok(header)
    }

    /// Memory Card 251 in slot A for Animal Crossing (a 59-block card for
    /// everything else).
    fn apply_animal_crossing(&mut self) {
        let is_ac = animal_crossing::lookup(System::GameCube, &self.game_id).is_some();
        let model =
            if is_ac { self.ac_setup.memory_card.unwrap_or(MemoryCardModel::Card251) } else { MemoryCardModel::Card59 };
        self.bus.exi[0].devices[0] = Some(Box::new(MemoryCard::new(model)));
    }

    /// Plug a GBA into controller port 2 (JoyBus).
    pub fn attach_gba(&mut self, gba: core_gba::Gba) {
        let port = &mut self.bus.si.ports[1];
        port.connected = true;
        port.joybus = Some(Box::new(si::GbaLink { gba }));
    }

    fn write_virtual(&mut self, va: u32, data: &[u8]) {
        let ram = self.bus.ram();
        let pa = (va & 0x01FF_FFFF) as usize;
        if pa + data.len() <= ram.len() {
            ram[pa..pa + data.len()].copy_from_slice(data);
        }
    }

    fn write_u32(&mut self, va: u32, v: u32) {
        self.write_virtual(va, &v.to_be_bytes());
    }

    fn read_u32(&mut self, va: u32) -> u32 {
        self.bus.read32(va & 0x01FF_FFFF)
    }

    /// Low-memory globals the IPL leaves for the OS.
    fn setup_os_globals(&mut self) {
        let id = self.game_id;
        self.write_virtual(0x8000_0000, &id);
        self.write_u32(0x8000_0020, 0x0D15_EA5E);
        self.write_u32(0x8000_0024, 1);
        self.write_u32(0x8000_0028, bus::MEM1_SIZE as u32);
        self.write_u32(0x8000_002C, 1);
        self.write_u32(0x8000_0034, 0x817F_E8C0);
        self.write_u32(0x8000_00CC, 0); // NTSC
        self.write_u32(0x8000_00F0, bus::MEM1_SIZE as u32);
        self.write_u32(0x8000_00F8, 162_000_000);
        self.write_u32(0x8000_00FC, 486_000_000);
        // `rfi` at every exception vector until the OS installs handlers.
        for v in (0x100..0x1400).step_by(0x100) {
            self.write_u32(0x8000_0000 + v, 0x4C00_0064);
        }
        // Return trampoline for HLE calls: `b .`
        self.write_u32(HLE_RETURN, 0x4800_0000);
    }

    fn prepare_cpu(&mut self) {
        self.cpu.setup_os_bats();
        self.cpu.msr = (self.cpu.msr | MSR_FP) & !MSR_EE;
        self.cpu.gpr[1] = 0x816F_FFF0;
    }

    pub fn boot_dol(&mut self, data: &[u8]) -> Result<(), BootError> {
        let dol = Dol::parse(data).ok_or(BootError::BadDol)?;
        self.prepare_cpu();
        self.setup_os_globals();
        for (addr, bytes) in &dol.sections {
            self.write_virtual(*addr, bytes);
        }
        let zeros = vec![0u8; dol.bss_size as usize];
        if dol.bss_size > 0 && dol.bss_size < bus::MEM1_SIZE as u32 {
            // BSS overlapping loaded sections must not be cleared.
            let overlaps = dol
                .sections
                .iter()
                .any(|(a, b)| *a < dol.bss_addr + dol.bss_size && dol.bss_addr < *a + b.len() as u32);
            if !overlaps {
                self.write_virtual(dol.bss_addr, &zeros);
            }
        }
        self.cpu.pc = dol.entry;
        Ok(())
    }

    /// Call guest code at `addr` with `args` in r3..; returns r3.
    fn hle_call(&mut self, addr: u32, args: &[u32], max_steps: u64) -> Result<u32, BootError> {
        for (i, &a) in args.iter().enumerate() {
            self.cpu.gpr[3 + i] = a;
        }
        self.cpu.set_lr(HLE_RETURN);
        self.cpu.pc = addr;
        for _ in 0..max_steps {
            if self.cpu.pc == HLE_RETURN {
                return Ok(self.cpu.gpr[3]);
            }
            self.cpu.step(&mut self.bus);
        }
        Err(BootError::ApploaderTimeout)
    }

    /// Run the disc's apploader the way the IPL does.
    pub fn run_apploader(&mut self) -> Result<(), BootError> {
        let disc = self.bus.disc.as_mut().ok_or(BootError::NotGameCube)?;
        let header = ApploaderHeader::read(disc.as_mut())?;
        let mut body = vec![0u8; (header.size + header.trailer) as usize];
        disc.read(ApploaderHeader::OFFSET + 0x20, &mut body)?;
        self.prepare_cpu();
        self.setup_os_globals();
        self.write_virtual(0x8120_0000, &body);
        // A `blr` for the apploader's report callback.
        let report = HLE_RETURN + 4;
        self.write_u32(report, 0x4E80_0020);
        let ptrs = 0x8130_0010;
        self.hle_call(header.entry, &[ptrs, ptrs + 4, ptrs + 8], 1_000_000)?;
        let (init, main, close) = (self.read_u32(ptrs), self.read_u32(ptrs + 4), self.read_u32(ptrs + 8));
        self.hle_call(init, &[report], 1_000_000)?;
        let out = 0x8130_0020;
        for _ in 0..10_000 {
            let more = self.hle_call(main, &[out, out + 4, out + 8], 10_000_000)?;
            if more == 0 {
                break;
            }
            let (dst, len, off) = (self.read_u32(out), self.read_u32(out + 4), self.read_u32(out + 8));
            let mut buf = vec![0u8; len as usize];
            if let Some(d) = self.bus.disc.as_mut() {
                d.read(off as u64, &mut buf)?;
            }
            self.write_virtual(dst, &buf);
        }
        let entry = self.hle_call(close, &[], 1_000_000)?;
        self.cpu.gpr[1] = 0x816F_FFF0;
        self.cpu.pc = entry;
        Ok(())
    }

    /// Emulate one video field.
    pub fn run_field(&mut self) {
        let per_line = (self.cycles_per_field / VideoInterface::LINES_PER_FIELD as u64).max(1);
        for _ in 0..VideoInterface::LINES_PER_FIELD {
            for _ in 0..per_line {
                self.cpu.external_interrupt = self.bus.cpu_interrupt();
                self.cpu.step(&mut self.bus);
            }
            self.bus.flush_gather();
            self.bus.vi.next_line();
            // ~32 kHz AI DMA pull: 8 samples per 32-byte block.
            let mem1 = &self.bus.mem1;
            struct View<'a>(&'a [u8]);
            impl dsp_gc::MainMemory for View<'_> {
                fn read8(&self, a: u32) -> u8 {
                    self.0.get((a & 0x01FF_FFFF) as usize).copied().unwrap_or(0)
                }
                fn write8(&mut self, _: u32, _: u8) {}
            }
            if self.bus.vi.line % 2 == 0 {
                self.bus.dsp.ai_dma_block(&View(mem1.as_slice()));
            }
        }
        self.fields += 1;
        self.present();
        for s in std::mem::take(&mut self.bus.dsp.ai_samples) {
            self.audio.push(StereoFrame::new(s[0], s[1]));
        }
    }

    /// Convert the displayed XFB (YUYV) to RGBA8888.
    fn present(&mut self) {
        let (addr, w, h) = match self.bus.last_xfb {
            Some((a, w, h)) if a == self.bus.vi.xfb_address() || self.bus.vi.xfb_address() == 0 => {
                (a, w as usize, h as usize)
            }
            _ => (self.bus.vi.xfb_address(), 640, 480),
        };
        let ram = self.bus.mem1.as_slice();
        let (w, h) = (w.min(640), h.min(480));
        for y in 0..h {
            for x in (0..w).step_by(2) {
                let a = (addr as usize & 0x01FF_FFFF) + y * w * 2 + x * 2;
                let Some(px) = ram.get(a..a + 4) else { continue };
                let (y0, u, y1, v) = (px[0] as f32, px[1] as f32 - 128.0, px[2] as f32, px[3] as f32 - 128.0);
                for (i, yy) in [y0, y1].into_iter().enumerate() {
                    let c = 1.164 * (yy - 16.0);
                    let r = (c + 1.596 * v).clamp(0.0, 255.0) as u32;
                    let g = (c - 0.392 * u - 0.813 * v).clamp(0.0, 255.0) as u32;
                    let b = (c + 2.017 * u).clamp(0.0, 255.0) as u32;
                    self.framebuffer[y * 640 + x + i] = r | g << 8 | b << 16 | 0xFF00_0000;
                }
            }
        }
    }

    pub fn set_input(&mut self, input: &InputState) {
        self.bus.si.ports[0].input = *input;
    }
}

impl EmulatorCore for GameCube {
    fn system(&self) -> System {
        System::GameCube
    }

    fn game_code(&self) -> [u8; 6] {
        self.game_id
    }

    fn run_frame(&mut self, input: &InputState) {
        self.set_input(input);
        self.run_field();
    }

    fn frame(&self) -> FrameView<'_> {
        FrameView { width: 640, height: 480, pixels: &self.framebuffer }
    }

    fn audio_sample_rate(&self) -> u32 {
        32_000
    }

    fn drain_audio(&mut self, out: &mut Vec<StereoFrame>) {
        out.append(&mut self.audio);
    }

    fn reset(&mut self) {
        self.cpu = Ppc::new(self.cpu.model);
        self.prepare_cpu();
    }

    fn save_state(&self) -> Vec<u8> {
        use kd_core::{SaveState, StateWriter};
        let mut w = StateWriter::new();
        w.begin_section(*b"PPC0");
        self.cpu.save_state(&mut w);
        w.end_section();
        w.bytes(self.bus.mem1.as_slice());
        w.finish(System::GameCube as u8, self.game_id)
    }

    fn load_state(&mut self, data: &[u8]) -> StateResult<()> {
        use kd_core::{SaveState, StateReader};
        let (_, mut r) = StateReader::open(data, System::GameCube as u8)?;
        let mut cpu_r = r.expect_section(*b"PPC0")?;
        self.cpu.load_state(&mut cpu_r)?;
        let mem = r.bytes()?;
        let ram = self.bus.mem1.as_mut_slice();
        if mem.len() == ram.len() {
            ram.copy_from_slice(mem);
        }
        Ok(())
    }

    fn backup(&self) -> Option<&[u8]> {
        None
    }

    fn load_backup(&mut self, _data: &[u8]) {}

    fn clock(&mut self) -> &mut GuestClock {
        &mut self.bus.clock
    }
}
