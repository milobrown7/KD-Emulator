//! ARM946E-S CP15 system control coprocessor: ID registers, control
//! register (high vectors, TCM enables), protection-unit regions, cache
//! maintenance (no-ops), wait-for-interrupt and the ITCM/DTCM windows.

use kd_core::{SaveState, StateReader, StateResult, StateWriter};

pub const CTRL_MPU: u32 = 1 << 0;
pub const CTRL_HIGH_VECTORS: u32 = 1 << 13;
pub const CTRL_DTCM_ENABLE: u32 = 1 << 16;
pub const CTRL_DTCM_LOAD: u32 = 1 << 17;
pub const CTRL_ITCM_ENABLE: u32 = 1 << 18;
pub const CTRL_ITCM_LOAD: u32 = 1 << 19;

const CTRL_WRITABLE: u32 = 0x000F_F085;
const CTRL_RESET: u32 = 0x0000_2078;

#[derive(Debug, Clone)]
pub struct Cp15 {
    pub control: u32,
    pub dtcm_setting: u32,
    pub itcm_setting: u32,
    pub dtcm_base: u32,
    pub dtcm_size: u32,
    pub itcm_size: u32,
    pub regions: [u32; 8],
    pub data_cacheable: u32,
    pub inst_cacheable: u32,
    pub write_buffer: u32,
    pub data_perm: u32,
    pub inst_perm: u32,
    /// Set by the "wait for interrupt" operation; the system halts the CPU
    /// and clears it.
    pub halt_requested: bool,
}

impl Default for Cp15 {
    fn default() -> Self {
        let mut c = Self {
            control: CTRL_RESET,
            dtcm_setting: 0,
            itcm_setting: 0,
            dtcm_base: 0,
            dtcm_size: 0,
            itcm_size: 0,
            regions: [0; 8],
            data_cacheable: 0,
            inst_cacheable: 0,
            write_buffer: 0,
            data_perm: 0,
            inst_perm: 0,
            halt_requested: false,
        };
        c.update_tcm();
        c
    }
}

impl Cp15 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn exception_base(&self) -> u32 {
        if self.control & CTRL_HIGH_VECTORS != 0 {
            0xFFFF_0000
        } else {
            0
        }
    }

    pub fn dtcm_readable(&self) -> bool {
        self.control & CTRL_DTCM_ENABLE != 0 && self.control & CTRL_DTCM_LOAD == 0
    }
    pub fn dtcm_writable(&self) -> bool {
        self.control & CTRL_DTCM_ENABLE != 0
    }
    pub fn itcm_readable(&self) -> bool {
        self.control & CTRL_ITCM_ENABLE != 0 && self.control & CTRL_ITCM_LOAD == 0
    }
    pub fn itcm_writable(&self) -> bool {
        self.control & CTRL_ITCM_ENABLE != 0
    }

    /// Is `addr` inside the DTCM window?
    #[inline]
    pub fn in_dtcm(&self, addr: u32) -> bool {
        self.dtcm_size != 0 && addr.wrapping_sub(self.dtcm_base) < self.dtcm_size
    }

    /// Is `addr` inside the ITCM window (always based at 0 on the DS)?
    #[inline]
    pub fn in_itcm(&self, addr: u32) -> bool {
        addr < self.itcm_size
    }

    fn update_tcm(&mut self) {
        let size = |setting: u32| {
            let shift = (setting >> 1) & 0x1F;
            if shift < 3 {
                4096 // minimum 4 KiB
            } else {
                512u32.checked_shl(shift).unwrap_or(0)
            }
        };
        self.dtcm_base = self.dtcm_setting & 0xFFFF_F000;
        self.dtcm_size = size(self.dtcm_setting);
        self.itcm_size = size(self.itcm_setting);
    }

    pub fn read(&self, op1: u8, crn: u8, crm: u8, op2: u8) -> Option<u32> {
        if op1 != 0 {
            return None;
        }
        Some(match (crn, crm, op2) {
            (0, 0, 0) => 0x4105_9461,
            (0, 0, 1) => 0x0F0D_2112,
            (0, 0, 2) => 0x0014_0180,
            (1, 0, 0) => self.control,
            (2, 0, 0) => self.data_cacheable,
            (2, 0, 1) => self.inst_cacheable,
            (3, 0, 0) => self.write_buffer,
            (5, 0, 2) => self.data_perm,
            (5, 0, 3) => self.inst_perm,
            (5, 0, 0) => compress_perm(self.data_perm),
            (5, 0, 1) => compress_perm(self.inst_perm),
            (6, r, _) if r < 8 => self.regions[r as usize],
            (9, 1, 0) => self.dtcm_setting,
            (9, 1, 1) => self.itcm_setting,
            _ => {
                tracing::warn!("CP15 read of unimplemented c{crn},c{crm},{op2}");
                0
            }
        })
    }

    pub fn write(&mut self, op1: u8, crn: u8, crm: u8, op2: u8, value: u32) -> bool {
        if op1 != 0 {
            return false;
        }
        match (crn, crm, op2) {
            (1, 0, 0) => {
                self.control = (self.control & !CTRL_WRITABLE) | (value & CTRL_WRITABLE);
            }
            (2, 0, 0) => self.data_cacheable = value,
            (2, 0, 1) => self.inst_cacheable = value,
            (3, 0, 0) => self.write_buffer = value,
            (5, 0, 0) => self.data_perm = expand_perm(value),
            (5, 0, 1) => self.inst_perm = expand_perm(value),
            (5, 0, 2) => self.data_perm = value,
            (5, 0, 3) => self.inst_perm = value,
            (6, r, _) if r < 8 => self.regions[r as usize] = value,
            (7, 0, 4) | (7, 8, 2) => self.halt_requested = true,
            (7, _, _) => {} // cache / write-buffer maintenance
            (9, 1, 0) => {
                self.dtcm_setting = value;
                self.update_tcm();
            }
            (9, 1, 1) => {
                self.itcm_setting = value;
                self.update_tcm();
            }
            (9, 0, _) => {} // cache lockdown
            _ => tracing::warn!("CP15 write of unimplemented c{crn},c{crm},{op2} = {value:#x}"),
        }
        true
    }
}

fn expand_perm(v: u32) -> u32 {
    (0..8).fold(0, |acc, i| acc | (((v >> (i * 2)) & 3) << (i * 4)))
}

fn compress_perm(v: u32) -> u32 {
    (0..8).fold(0, |acc, i| acc | (((v >> (i * 4)) & 3) << (i * 2)))
}

impl SaveState for Cp15 {
    fn save_state(&self, w: &mut StateWriter) {
        w.u32(self.control);
        w.u32(self.dtcm_setting);
        w.u32(self.itcm_setting);
        w.u32s(&self.regions);
        w.u32(self.data_cacheable);
        w.u32(self.inst_cacheable);
        w.u32(self.write_buffer);
        w.u32(self.data_perm);
        w.u32(self.inst_perm);
        w.bool(self.halt_requested);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        self.control = r.u32()?;
        self.dtcm_setting = r.u32()?;
        self.itcm_setting = r.u32()?;
        r.u32s_into(&mut self.regions)?;
        self.data_cacheable = r.u32()?;
        self.inst_cacheable = r.u32()?;
        self.write_buffer = r.u32()?;
        self.data_perm = r.u32()?;
        self.inst_perm = r.u32()?;
        self.halt_requested = r.bool()?;
        self.update_tcm();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcm_regions() {
        let mut c = Cp15::new();
        // DTCM at 0x027C0000, 16 KiB (size field 5 → 512 << 5)
        c.write(0, 9, 1, 0, 0x027C_000A);
        c.write(0, 9, 1, 1, 0x0000_0020); // ITCM 32 MiB window (16 → 512<<16)
        c.write(0, 1, 0, 0, c.control | CTRL_DTCM_ENABLE | CTRL_ITCM_ENABLE);
        assert_eq!(c.dtcm_base, 0x027C_0000);
        assert_eq!(c.dtcm_size, 16 * 1024);
        assert!(c.in_dtcm(0x027C_3FFF));
        assert!(!c.in_dtcm(0x027C_4000));
        assert_eq!(c.itcm_size, 32 * 1024 * 1024);
        assert!(c.dtcm_readable() && c.itcm_writable());
    }

    #[test]
    fn high_vectors_and_wfi() {
        let mut c = Cp15::new();
        // The DS boots from the BIOS at 0xFFFF0000, so V is set at reset.
        assert_eq!(c.exception_base(), 0xFFFF_0000);
        c.write(0, 1, 0, 0, 0);
        assert_eq!(c.exception_base(), 0);
        c.write(0, 1, 0, 0, CTRL_HIGH_VECTORS);
        assert_eq!(c.exception_base(), 0xFFFF_0000);
        c.write(0, 7, 0, 4, 0);
        assert!(c.halt_requested);
        assert_eq!(c.read(0, 0, 0, 0), Some(0x4105_9461));
    }

    #[test]
    fn permission_encodings_round_trip() {
        let mut c = Cp15::new();
        c.write(0, 5, 0, 0, 0b11_10_01_00);
        assert_eq!(c.read(0, 5, 0, 2), Some(0x3210));
        assert_eq!(c.read(0, 5, 0, 0), Some(0b11_10_01_00));
    }
}
