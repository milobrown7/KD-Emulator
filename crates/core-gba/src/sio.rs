//! Serial port in JoyBus mode — the GBA side of the GameCube link cable.
//!
//! This is the Animal Crossing connection: the GameCube game talks to a GBA
//! on controller port 2 to send the island and NES programs (multiboot) and
//! to exchange pattern data. The GameCube core drives [`JoyBus::host_command`];
//! the GBA program sees `JOYCNT`/`JOY_RECV`/`JOY_TRANS`/`JOYSTAT`.

use kd_core::{SaveState, StateReader, StateResult, StateWriter};

pub const CMD_RESET: u8 = 0xFF;
pub const CMD_STATUS: u8 = 0x00;
pub const CMD_READ: u8 = 0x14;
pub const CMD_WRITE: u8 = 0x15;

const JOYCNT_RESET: u16 = 1;
const JOYCNT_RECV: u16 = 2;
const JOYCNT_SEND: u16 = 4;
const JOYCNT_IRQ: u16 = 0x40;

const JOYSTAT_RECV: u8 = 2;
const JOYSTAT_SEND: u8 = 8;

#[derive(Debug, Clone, Default)]
pub struct JoyBus {
    pub joycnt: u16,
    pub recv: u32,
    pub trans: u32,
    pub joystat: u8,
    /// RCNT (mode select; JoyBus when bits 14-15 = 11).
    pub rcnt: u16,
    /// SIOCNT / data registers for normal/multiplayer modes (kept for reads).
    pub siocnt: u16,
    /// Set when the host completes a command with JOYCNT IRQ enabled.
    pub irq: bool,
}

impl JoyBus {
    pub fn joybus_mode(&self) -> bool {
        self.rcnt & 0xC000 == 0xC000
    }

    /// Execute a command from the GameCube (host). Returns the response
    /// bytes the GBA puts on the wire.
    pub fn host_command(&mut self, cmd: u8, data: u32) -> Vec<u8> {
        let resp = match cmd {
            CMD_RESET | CMD_STATUS => {
                if cmd == CMD_RESET {
                    self.joycnt |= JOYCNT_RESET;
                }
                vec![0x00, 0x04, self.joystat]
            }
            CMD_WRITE => {
                self.recv = data;
                self.joystat |= JOYSTAT_RECV;
                self.joycnt |= JOYCNT_RECV;
                vec![self.joystat]
            }
            CMD_READ => {
                let mut v = self.trans.to_le_bytes().to_vec();
                self.joystat &= !JOYSTAT_SEND;
                self.joycnt |= JOYCNT_SEND;
                v.push(self.joystat);
                v
            }
            _ => {
                tracing::warn!("unknown JoyBus command {cmd:#04x}");
                vec![]
            }
        };
        if self.joycnt & JOYCNT_IRQ != 0 {
            self.irq = true;
        }
        resp
    }

    pub fn read16(&mut self, off: u32) -> u16 {
        match off {
            0x128 => self.siocnt,
            0x134 => self.rcnt,
            0x140 => self.joycnt,
            0x150 => {
                self.joystat &= !JOYSTAT_RECV;
                self.recv as u16
            }
            0x152 => {
                self.joystat &= !JOYSTAT_RECV;
                (self.recv >> 16) as u16
            }
            0x154 => self.trans as u16,
            0x156 => (self.trans >> 16) as u16,
            0x158 => self.joystat as u16,
            _ => 0,
        }
    }

    pub fn write16(&mut self, off: u32, v: u16) {
        match off {
            0x128 => self.siocnt = v & !0x80, // transfers complete instantly (no link partner)
            0x134 => self.rcnt = v,
            // Bits 0-2 are acknowledge-by-writing-1.
            0x140 => self.joycnt = (self.joycnt & !(v & 7)) & 7 | (v & JOYCNT_IRQ),
            0x150 => self.recv = (self.recv & 0xFFFF_0000) | v as u32,
            0x152 => self.recv = (self.recv & 0xFFFF) | (v as u32) << 16,
            0x154 => self.trans = (self.trans & 0xFFFF_0000) | v as u32,
            0x156 => {
                self.trans = (self.trans & 0xFFFF) | (v as u32) << 16;
                self.joystat |= JOYSTAT_SEND;
            }
            0x158 => self.joystat = (self.joystat & !0x30) | (v as u8 & 0x30),
            _ => {}
        }
    }
}

impl SaveState for JoyBus {
    fn save_state(&self, w: &mut StateWriter) {
        w.u16(self.joycnt);
        w.u32(self.recv);
        w.u32(self.trans);
        w.u8(self.joystat);
        w.u16(self.rcnt);
        w.u16(self.siocnt);
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        self.joycnt = r.u16()?;
        self.recv = r.u32()?;
        self.trans = r.u32()?;
        self.joystat = r.u8()?;
        self.rcnt = r.u16()?;
        self.siocnt = r.u16()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamecube_handshake() {
        let mut j = JoyBus { rcnt: 0xC000, ..Default::default() };
        assert!(j.joybus_mode());
        assert_eq!(j.host_command(CMD_RESET, 0), vec![0x00, 0x04, 0x00]);
        assert_eq!(j.joycnt & JOYCNT_RESET, 1);
        j.write16(0x140, 1); // acknowledge reset
        assert_eq!(j.joycnt & JOYCNT_RESET, 0);

        // GameCube → GBA
        j.host_command(CMD_WRITE, 0xA1B2_C3D4);
        assert_eq!(j.read16(0x158) as u8 & JOYSTAT_RECV, JOYSTAT_RECV);
        assert_eq!(j.read16(0x150), 0xC3D4);
        assert_eq!(j.read16(0x152), 0xA1B2);
        assert_eq!(j.joystat & JOYSTAT_RECV, 0);

        // GBA → GameCube
        j.write16(0x154, 0x5678);
        j.write16(0x156, 0x1234);
        assert_eq!(j.joystat & JOYSTAT_SEND, JOYSTAT_SEND);
        assert_eq!(j.host_command(CMD_READ, 0), vec![0x78, 0x56, 0x34, 0x12, 0x00]);
    }

    #[test]
    fn irq_on_command() {
        let mut j = JoyBus::default();
        j.write16(0x140, JOYCNT_IRQ);
        j.host_command(CMD_STATUS, 0);
        assert!(j.irq);
    }
}
