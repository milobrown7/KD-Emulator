//! Serial Interface: four controller ports with polling and the JoyBus
//! command buffer. Port 2 can host a linked Game Boy Advance — the
//! Animal Crossing island and NES-on-GBA connection.

use kd_core::input::button;
use kd_core::InputState;

pub const DEVICE_GC_CONTROLLER: u32 = 0x0900_0000;
pub const DEVICE_GBA: u32 = 0x0004_0000;

/// Something plugged into a SI port that answers JoyBus commands.
pub trait JoyBusDevice: Send {
    /// Execute `cmd` with its payload; return the response bytes.
    fn command(&mut self, cmd: &[u8]) -> Vec<u8>;
}

/// GBA link adapter: forwards JoyBus commands to an emulated GBA.
pub struct GbaLink {
    pub gba: core_gba::Gba,
}

impl JoyBusDevice for GbaLink {
    fn command(&mut self, cmd: &[u8]) -> Vec<u8> {
        let op = cmd.first().copied().unwrap_or(0);
        let data = cmd.get(1..5).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
        self.gba.bus.sio.host_command(op, data)
    }
}

#[derive(Default)]
pub struct SiPort {
    pub input: InputState,
    pub connected: bool,
    pub joybus: Option<Box<dyn JoyBusDevice>>,
    pub out: u32,
    /// Analog origin reported by command 0x41.
    pub rumble: bool,
}

/// Encode a GameCube controller poll response (INBUFH, INBUFL).
pub fn controller_report(i: &InputState) -> (u32, u32) {
    let mut b = 0u32;
    let map = [
        (button::LEFT, 0x0001),
        (button::RIGHT, 0x0002),
        (button::DOWN, 0x0004),
        (button::UP, 0x0008),
        (button::Z, 0x0010),
        (button::R, 0x0020),
        (button::L, 0x0040),
        (button::A, 0x0100),
        (button::B, 0x0200),
        (button::X, 0x0400),
        (button::Y, 0x0800),
        (button::START, 0x1000),
    ];
    for (k, bit) in map {
        if i.pressed(k) {
            b |= bit;
        }
    }
    b |= 0x0080; // "use origin"
    let axis = |v: f32| ((v.clamp(-1.0, 1.0) * 127.0) + 128.0).round().clamp(0.0, 255.0) as u32;
    let trig = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    let hi = b << 16 | axis(i.stick.0) << 8 | axis(i.stick.1);
    let lo = axis(i.c_stick.0) << 24 | axis(i.c_stick.1) << 16 | trig(i.triggers.0) << 8 | trig(i.triggers.1);
    (hi, lo)
}

pub struct SerialInterface {
    pub ports: [SiPort; 4],
    pub poll: u32,
    pub comcsr: u32,
    pub sr: u32,
    pub exilk: u32,
    pub buffer: [u8; 128],
}

impl Default for SerialInterface {
    fn default() -> Self {
        let mut s = Self { ports: Default::default(), poll: 0, comcsr: 0, sr: 0, exilk: 0, buffer: [0; 128] };
        s.ports[0].connected = true;
        s
    }
}

impl SerialInterface {
    pub fn interrupt(&self) -> bool {
        let tc = self.comcsr & (1 << 31) != 0 && self.comcsr & (1 << 30) != 0;
        let rdst = self.comcsr & (1 << 29) != 0 && self.comcsr & (1 << 28) != 0;
        tc || rdst
    }

    pub fn read32(&mut self, off: u32) -> u32 {
        match off {
            0x00..=0x2F => {
                let ch = (off / 0xC) as usize;
                let p = &self.ports[ch];
                match off % 0xC {
                    0 => p.out,
                    4 | 8 => {
                        if !p.connected || p.joybus.is_some() {
                            return if off % 0xC == 4 { 0x8000_0000 } else { 0 }; // no response
                        }
                        let (hi, lo) = controller_report(&p.input);
                        self.sr &= !(0x2000_0000 >> (ch * 8)); // clear RDST
                        if off % 0xC == 4 {
                            hi
                        } else {
                            lo
                        }
                    }
                    _ => 0,
                }
            }
            0x30 => self.poll,
            0x34 => self.comcsr,
            0x38 => self.sr,
            0x3C => self.exilk,
            0x80..=0xFF => {
                let i = (off - 0x80) as usize;
                u32::from_be_bytes([self.buffer[i], self.buffer[i + 1], self.buffer[i + 2], self.buffer[i + 3]])
            }
            _ => 0,
        }
    }

    pub fn write32(&mut self, off: u32, v: u32) {
        match off {
            0x00..=0x2F if off % 0xC == 0 => {
                let ch = (off / 0xC) as usize;
                self.ports[ch].out = v;
                self.ports[ch].rumble = v & 1 != 0;
            }
            0x30 => self.poll = v,
            0x34 => {
                // TCINT / RDSTINT are write-1-to-clear.
                let status = self.comcsr & 0xA000_0000 & !(v & 0xA000_0000);
                self.comcsr = (v & !0xA000_0001) | status;
                if v & 1 != 0 {
                    self.transfer();
                }
            }
            0x38 => self.sr &= !(v & 0x0F0F_0F0F),
            0x3C => self.exilk = v,
            0x80..=0xFF => {
                let i = (off - 0x80) as usize;
                self.buffer[i..i + 4].copy_from_slice(&v.to_be_bytes());
            }
            _ => {}
        }
    }

    /// Run a SICOMCSR transfer through the IO buffer.
    fn transfer(&mut self) {
        let ch = ((self.comcsr >> 1) & 3) as usize;
        let out_len = match (self.comcsr >> 16) & 0x7F {
            0 => 128,
            n => n as usize,
        };
        let cmd = self.buffer[..out_len].to_vec();
        let port = &mut self.ports[ch];
        let resp: Vec<u8> = if let Some(dev) = port.joybus.as_mut() {
            dev.command(&cmd)
        } else if port.connected {
            match cmd.first().copied().unwrap_or(0) {
                0x00 | 0xFF => DEVICE_GC_CONTROLLER.to_be_bytes()[..3].to_vec(),
                0x41 | 0x42 => vec![0x00, 0x80, 128, 128, 128, 128, 0, 0, 0, 0],
                0x40 => {
                    let (hi, lo) = controller_report(&port.input);
                    [hi.to_be_bytes(), lo.to_be_bytes()].concat()
                }
                c => {
                    tracing::warn!("unknown SI command {c:#04x}");
                    vec![]
                }
            }
        } else {
            self.comcsr |= 1 << 29; // no response
            vec![]
        };
        for (i, b) in resp.iter().take(128).enumerate() {
            self.buffer[i] = *b;
        }
        self.comcsr &= !1;
        self.comcsr |= 1 << 31; // TCINT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_report_encodes_buttons_and_sticks() {
        let input = InputState { buttons: button::A | button::START, stick: (1.0, -1.0), triggers: (0.0, 1.0), ..Default::default() };
        let (hi, lo) = controller_report(&input);
        assert_eq!(hi >> 16, 0x1180);
        assert_eq!((hi >> 8) & 0xFF, 255);
        assert_eq!(hi & 0xFF, 1);
        assert_eq!(lo & 0xFF, 255);
    }

    #[test]
    fn status_command_identifies_controller() {
        let mut si = SerialInterface::default();
        si.write32(0x80, 0x0000_0000);
        si.write32(0x34, 1 | 1 << 16 | 3 << 8); // ch0, out 1, in 3
        assert_eq!(si.read32(0x80) >> 8, 0x09_0000);
        assert_ne!(si.comcsr & (1 << 31), 0);
    }

    #[test]
    fn gba_on_port_two_answers_joybus() {
        let mut si = SerialInterface::default();
        let gba = core_gba::Gba::new_multiboot(&[0u8; 0x100]);
        si.ports[1].connected = true;
        si.ports[1].joybus = Some(Box::new(GbaLink { gba }));
        si.write32(0x80, 0xFF00_0000); // reset
        si.write32(0x34, 1 | 1 << 1 | 1 << 16);
        assert_eq!(si.read32(0x80) >> 8, 0x00_0400, "GBA device id");
    }
}
