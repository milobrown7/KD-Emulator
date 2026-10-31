//! Inter-processor communication: IPCSYNC nibbles and the two 16-word
//! IPCFIFOs between the ARM9 and the ARM7.

use kd_core::{SaveState, StateReader, StateResult, StateWriter};
use std::collections::VecDeque;

pub const IRQ_IPC_SYNC: u32 = 1 << 16;
pub const IRQ_IPC_SEND_EMPTY: u32 = 1 << 17;
pub const IRQ_IPC_RECV_NOT_EMPTY: u32 = 1 << 18;

#[derive(Debug, Clone, Default)]
pub struct IpcSide {
    /// IPCSYNC output nibble (bits 8-11) and IRQ enable (bit 14).
    pub sync: u16,
    /// IPCFIFOCNT control bits (2, 10, 14, 15).
    pub fifocnt: u16,
    /// FIFO this side sends into (the other side receives from it).
    pub send: VecDeque<u32>,
    pub last_received: u32,
}

/// Both directions. Index 0 = ARM9, 1 = ARM7.
#[derive(Debug, Clone, Default)]
pub struct Ipc {
    pub side: [IpcSide; 2],
}

/// IRQ requests produced by an IPC access: (cpu index, IF bits).
pub type IpcIrqs = Vec<(usize, u32)>;

impl Ipc {
    pub fn read_sync(&self, cpu: usize) -> u16 {
        let other = &self.side[1 - cpu];
        (self.side[cpu].sync & 0x4F00) | ((other.sync >> 8) & 0xF)
    }

    pub fn write_sync(&mut self, cpu: usize, v: u16) -> IpcIrqs {
        self.side[cpu].sync = v & 0x4F00;
        let mut irqs = Vec::new();
        if v & 0x2000 != 0 && self.side[1 - cpu].sync & 0x4000 != 0 {
            irqs.push((1 - cpu, IRQ_IPC_SYNC));
        }
        irqs
    }

    pub fn read_fifocnt(&self, cpu: usize) -> u16 {
        let me = &self.side[cpu];
        let recv = &self.side[1 - cpu].send;
        me.fifocnt & 0xC404
            | me.send.is_empty() as u16
            | ((me.send.len() >= 16) as u16) << 1
            | (recv.is_empty() as u16) << 8
            | ((recv.len() >= 16) as u16) << 9
    }

    pub fn write_fifocnt(&mut self, cpu: usize, v: u16) -> IpcIrqs {
        let mut irqs = Vec::new();
        let me = &mut self.side[cpu];
        let old = me.fifocnt;
        if v & 0x0008 != 0 {
            me.send.clear();
        }
        if v & 0x4000 != 0 {
            me.fifocnt &= !0x4000; // acknowledge error
        }
        me.fifocnt = (me.fifocnt & 0x4000) | (v & 0x8404);
        // Enabling an IRQ whose condition already holds fires immediately.
        if old & 4 == 0 && v & 4 != 0 && me.send.is_empty() {
            irqs.push((cpu, IRQ_IPC_SEND_EMPTY));
        }
        let recv_empty = self.side[1 - cpu].send.is_empty();
        if old & 0x400 == 0 && v & 0x400 != 0 && !recv_empty {
            irqs.push((cpu, IRQ_IPC_RECV_NOT_EMPTY));
        }
        irqs
    }

    pub fn send(&mut self, cpu: usize, v: u32) -> IpcIrqs {
        let mut irqs = Vec::new();
        let me = &mut self.side[cpu];
        if me.fifocnt & 0x8000 == 0 {
            return irqs;
        }
        if me.send.len() >= 16 {
            me.fifocnt |= 0x4000;
            return irqs;
        }
        let was_empty = me.send.is_empty();
        me.send.push_back(v);
        let other = &self.side[1 - cpu];
        if was_empty && other.fifocnt & 0x400 != 0 {
            irqs.push((1 - cpu, IRQ_IPC_RECV_NOT_EMPTY));
        }
        irqs
    }

    pub fn receive(&mut self, cpu: usize) -> (u32, IpcIrqs) {
        let mut irqs = Vec::new();
        if self.side[cpu].fifocnt & 0x8000 == 0 {
            return (self.side[cpu].last_received, irqs);
        }
        let other = 1 - cpu;
        match self.side[other].send.pop_front() {
            Some(v) => {
                self.side[cpu].last_received = v;
                if self.side[other].send.is_empty() && self.side[other].fifocnt & 4 != 0 {
                    irqs.push((other, IRQ_IPC_SEND_EMPTY));
                }
                (v, irqs)
            }
            None => {
                self.side[cpu].fifocnt |= 0x4000;
                (self.side[cpu].last_received, irqs)
            }
        }
    }
}

impl SaveState for Ipc {
    fn save_state(&self, w: &mut StateWriter) {
        for s in &self.side {
            w.u16(s.sync);
            w.u16(s.fifocnt);
            let v: Vec<u32> = s.send.iter().copied().collect();
            w.u32s(&v);
            w.u32(s.last_received);
        }
    }
    fn load_state(&mut self, r: &mut StateReader<'_>) -> StateResult<()> {
        for s in &mut self.side {
            s.sync = r.u16()?;
            s.fifocnt = r.u16()?;
            let n = r.u32()? as usize;
            s.send.clear();
            for _ in 0..n {
                s.send.push_back(r.u32()?);
            }
            s.last_received = r.u32()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_nibbles_and_irq() {
        let mut ipc = Ipc::default();
        ipc.write_sync(1, 0x4000); // ARM7 enables sync IRQ
        let irqs = ipc.write_sync(0, 0x2000 | 0x0500); // ARM9 outputs 5 and pokes
        assert_eq!(irqs, vec![(1, IRQ_IPC_SYNC)]);
        assert_eq!(ipc.read_sync(1) & 0xF, 5);
    }

    #[test]
    fn fifo_round_trip_with_irqs() {
        let mut ipc = Ipc::default();
        ipc.write_fifocnt(0, 0x8000);
        let irqs = ipc.write_fifocnt(1, 0x8000 | 0x400);
        assert!(irqs.is_empty());
        let irqs = ipc.send(0, 0xCAFE);
        assert_eq!(irqs, vec![(1, IRQ_IPC_RECV_NOT_EMPTY)]);
        assert_eq!(ipc.read_fifocnt(1) & 0x100, 0, "ARM7 receive FIFO not empty");
        let (v, _) = ipc.receive(1);
        assert_eq!(v, 0xCAFE);
        assert_ne!(ipc.read_fifocnt(0) & 1, 0, "ARM9 send FIFO empty again");
        // reading an empty FIFO sets the error flag
        ipc.receive(1);
        assert_ne!(ipc.read_fifocnt(1) & 0x4000, 0);
    }

    #[test]
    fn fifo_full_sets_error() {
        let mut ipc = Ipc::default();
        ipc.write_fifocnt(0, 0x8000);
        for i in 0..16 {
            ipc.send(0, i);
        }
        assert_ne!(ipc.read_fifocnt(0) & 2, 0);
        ipc.send(0, 99);
        assert_ne!(ipc.read_fifocnt(0) & 0x4000, 0);
    }
}
