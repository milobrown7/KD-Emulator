//! # core-wii
//!
//! Nintendo Wii emulation for KD-Emulator, layered on `core-gcn`:
//! Broadway (Gekko + 8 BATs), Hollywood registers, 64 MiB MEM2 on a
//! 16 KB-safe arena, the Starlet IOS HLE IPC router ([`ios`]), WBFS/WAD
//! containers ([`containers`]) and Wii Remote HID report synthesis
//! ([`hid`]).
//!
//! Animal Crossing fit: City Folk / Let's Go to the City use the pointer
//! control layout (IR from touch drag or the rotation-vector sensor plus
//! Nunchuk stick), a virtual SD card for photos and patterns, and the
//! time-travel clock.

pub mod containers;
pub mod hid;
pub mod ios;

use core_gcn::bus::HollywoodHooks;
use core_gcn::disc::{Disc, DiscHeader};
use core_gcn::{BootError, GameCube};
use cpu_ppc::PpcModel;
use ios::{Ios, IosMemory, IpcRegs};
use kd_core::animal_crossing::{self, AcSetup};
use kd_core::arena::PageAwareArena;
use kd_core::{EmulatorCore, FrameView, GuestClock, InputState, StateResult, StereoFrame, System};

pub const MEM2_SIZE: usize = 64 * 1024 * 1024;

/// Physical memory as IOS sees it (MEM1 at 0, MEM2 at 0x10000000).
struct WiiMem<'a> {
    mem1: &'a mut [u8],
    mem2: &'a mut [u8],
}

impl WiiMem<'_> {
    fn slice(&self, addr: u32) -> Option<(&[u8], usize)> {
        match addr >> 28 {
            0 => Some((&*self.mem1, (addr & 0x01FF_FFFF) as usize)),
            1 => Some((&*self.mem2, (addr & 0x03FF_FFFF) as usize)),
            _ => None,
        }
    }
}

impl IosMemory for WiiMem<'_> {
    fn read(&self, addr: u32, len: usize) -> Vec<u8> {
        match self.slice(addr) {
            Some((m, a)) if a + len <= m.len() => m[a..a + len].to_vec(),
            _ => vec![0; len],
        }
    }
    fn write(&mut self, addr: u32, data: &[u8]) {
        let (m, a): (&mut [u8], usize) = match addr >> 28 {
            0 => (self.mem1, (addr & 0x01FF_FFFF) as usize),
            1 => (self.mem2, (addr & 0x03FF_FFFF) as usize),
            _ => return,
        };
        if a + data.len() <= m.len() {
            m[a..a + data.len()].copy_from_slice(data);
        }
    }
}

/// Hollywood: IPC mailbox, interrupt flags, MEM2 and the IOS router.
pub struct Hollywood {
    pub mem2: PageAwareArena,
    pub ipc: IpcRegs,
    pub ios: Ios,
    regs: Vec<u32>,
}

impl Hollywood {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            mem2: PageAwareArena::new(c"kd_wii_mem2", MEM2_SIZE)?,
            ipc: IpcRegs::default(),
            ios: Ios::new(),
            regs: vec![0; 0x100],
        })
    }
}

impl HollywoodHooks for Hollywood {
    fn read32(&mut self, off: u32, _mem1: &mut [u8]) -> u32 {
        match off {
            0x00 => self.ipc.ppcmsg,
            0x04 => self.ipc.ppcctrl,
            0x08 => self.ipc.armmsg,
            0x30 => self.ipc.irq_flag,
            0x34 => self.ipc.irq_mask,
            0x214 => 0x0000_0011, // Hollywood revision
            o => self.regs.get((o / 4) as usize).copied().unwrap_or(0),
        }
    }

    fn write32(&mut self, off: u32, v: u32, mem1: &mut [u8]) {
        match off {
            0x00 => self.ipc.ppcmsg = v,
            0x04 => {
                if self.ipc.write_ctrl(v) {
                    let req = self.ipc.ppcmsg;
                    let mut mem = WiiMem { mem1, mem2: self.mem2.as_mut_slice() };
                    self.ios.handle(&mut mem, req);
                    self.ipc.complete(req);
                }
            }
            0x30 => self.ipc.irq_flag &= !v,
            0x34 => self.ipc.irq_mask = v,
            o => {
                if let Some(r) = self.regs.get_mut((o / 4) as usize) {
                    *r = v;
                }
            }
        }
    }

    fn mem2(&mut self) -> &mut [u8] {
        self.mem2.as_mut_slice()
    }

    fn interrupt(&self) -> bool {
        self.ipc.interrupt()
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

pub struct Wii {
    pub gc: GameCube,
    pub ac_setup: AcSetup,
    pub extension: hid::Extension,
    pub input: InputState,
}

impl Wii {
    pub fn new_machine() -> Result<Self, BootError> {
        let mut gc = GameCube::new_machine(PpcModel::Broadway)?;
        gc.bus.hollywood = Some(Box::new(Hollywood::new()?));
        Ok(Self {
            gc,
            ac_setup: animal_crossing::setup_for(System::Wii),
            extension: hid::Extension::Nunchuk,
            input: InputState::default(),
        })
    }

    /// Boot a Wii DOL (homebrew / channel executables).
    pub fn from_dol(data: &[u8]) -> Result<Self, BootError> {
        let mut w = Self::new_machine()?;
        w.gc.boot_dol(data)?;
        Ok(w)
    }

    pub fn hollywood(&mut self) -> &mut Hollywood {
        self.gc
            .bus
            .hollywood
            .as_mut()
            .and_then(|h| h.as_any_mut().downcast_mut::<Hollywood>())
            .expect("Wii machine always has Hollywood")
    }

    pub fn ios(&mut self) -> &mut Ios {
        &mut self.hollywood().ios
    }

    /// Insert a disc (plain image or [`containers::WbfsDisc`]); IOS
    /// `/dev/di` serves it. Retail partitions are encrypted with the
    /// console common key, which is never bundled — decrypted images work.
    pub fn insert_disc(&mut self, mut disc: Box<dyn Disc>) -> Result<DiscHeader, BootError> {
        let header = DiscHeader::read(disc.as_mut())?;
        self.gc.game_id = header.game_id;
        let id4 = u32::from_be_bytes([header.game_id[0], header.game_id[1], header.game_id[2], header.game_id[3]]);
        let ios = self.ios();
        ios.title_id = 0x0001_0000_0000_0000 | id4 as u64;
        ios.disc = Some(disc);
        let ac = animal_crossing::lookup(System::Wii, &header.game_id).is_some();
        self.ios().sd_inserted = ac || self.ac_setup.sd_card;
        Ok(header)
    }

    /// The input report the emulated Wii Remote sends this frame.
    pub fn wiimote_report(&self, mode: u8) -> Vec<u8> {
        hid::input_report(mode, self.extension, &self.input)
    }
}

impl EmulatorCore for Wii {
    fn system(&self) -> System {
        System::Wii
    }
    fn peek(&mut self, addr: u32, size: u8) -> u32 {
        self.gc.peek_virtual(addr, size)
    }
    fn poke(&mut self, addr: u32, value: u32, size: u8) {
        self.gc.poke_virtual(addr, value, size)
    }
    fn game_code(&self) -> [u8; 6] {
        self.gc.game_id
    }
    fn run_frame(&mut self, input: &InputState) {
        self.input = *input;
        self.gc.run_frame(input);
    }
    fn frame(&self) -> FrameView<'_> {
        self.gc.frame()
    }
    fn audio_sample_rate(&self) -> u32 {
        self.gc.audio_sample_rate()
    }
    fn drain_audio(&mut self, out: &mut Vec<StereoFrame>) {
        self.gc.drain_audio(out)
    }
    fn reset(&mut self) {
        self.gc.reset()
    }
    fn save_state(&self) -> Vec<u8> {
        self.gc.save_state()
    }
    fn load_state(&mut self, data: &[u8]) -> StateResult<()> {
        self.gc.load_state(data)
    }
    fn backup(&self) -> Option<&[u8]> {
        None
    }
    fn load_backup(&mut self, _data: &[u8]) {}
    fn clock(&mut self) -> &mut GuestClock {
        self.gc.clock()
    }
}
