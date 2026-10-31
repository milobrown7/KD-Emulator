//! Starlet IOS high-level emulation: the Hollywood IPC mailbox and a
//! request router for the `/dev/*` resource managers the PowerPC talks to.
//!
//! Implemented devices:
//! * `/dev/fs` — ISFS on an in-memory NAND (files, directories, stats,
//!   usage) plus file handles opened by path.
//! * `/dev/es` — title identity queries.
//! * `/dev/di` — disc ID, inquiry, cover status and unencrypted reads.
//! * `/dev/sdio/slot0` — SD host controller commands on a virtual SD
//!   card (City Folk saves photos and patterns there).
//! * `/dev/stm/*` — power/reset event hooks.
//! * `/dev/usb/oh1/57e/305` — the Bluetooth HCI endpoint for Wii Remotes
//!   (opens and accepts traffic; input is injected via [`crate::hid`]).

use core_gcn::disc::Disc;
use std::collections::BTreeMap;

pub const IPC_OPEN: u32 = 1;
pub const IPC_CLOSE: u32 = 2;
pub const IPC_READ: u32 = 3;
pub const IPC_WRITE: u32 = 4;
pub const IPC_SEEK: u32 = 5;
pub const IPC_IOCTL: u32 = 6;
pub const IPC_IOCTLV: u32 = 7;
pub const IPC_REPLY: u32 = 8;

pub const IPC_OK: i32 = 0;
pub const IPC_EINVAL: i32 = -4;
pub const IPC_ENOENT: i32 = -6;
pub const FS_EEXIST: i32 = -105;
pub const FS_ENOENT: i32 = -106;

/// Guest memory view used by the router (MEM1 + MEM2, physical).
pub trait IosMemory {
    fn read(&self, addr: u32, len: usize) -> Vec<u8>;
    fn write(&mut self, addr: u32, data: &[u8]);
    fn read32(&self, addr: u32) -> u32 {
        let b = self.read(addr, 4);
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }
    fn write32(&mut self, addr: u32, v: u32) {
        self.write(addr, &v.to_be_bytes());
    }
    fn read_cstr(&self, addr: u32, max: usize) -> String {
        let b = self.read(addr, max);
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).into_owned()
    }
}

#[derive(Debug, Clone)]
enum Handle {
    Fs,
    Es,
    Di,
    Sdio,
    Stm,
    Bluetooth,
    File { path: String, pos: usize, write: bool },
}

#[derive(Debug, Default, Clone)]
pub struct Nand {
    pub files: BTreeMap<String, Vec<u8>>,
    pub dirs: std::collections::BTreeSet<String>,
}

impl Nand {
    fn parent_exists(&self, path: &str) -> bool {
        match path.rfind('/') {
            Some(0) | None => true,
            Some(i) => self.dirs.contains(&path[..i]),
        }
    }
}

pub struct Ios {
    handles: BTreeMap<u32, Handle>,
    next_fd: u32,
    pub nand: Nand,
    pub title_id: u64,
    pub disc: Option<Box<dyn Disc>>,
    pub sd_card: Vec<u8>,
    pub sd_inserted: bool,
    /// Requests served (debugger).
    pub requests: u64,
}

impl Default for Ios {
    fn default() -> Self {
        Self::new()
    }
}

impl Ios {
    pub fn new() -> Self {
        let mut nand = Nand::default();
        for d in ["/title", "/shared2", "/tmp", "/sys"] {
            nand.dirs.insert(d.to_string());
        }
        Self {
            handles: BTreeMap::new(),
            next_fd: 0,
            nand,
            title_id: 0,
            disc: None,
            sd_card: vec![0; 32 * 1024 * 1024],
            sd_inserted: true,
            requests: 0,
        }
    }

    /// Execute the request at `req`; writes the result and returns it.
    pub fn handle(&mut self, mem: &mut dyn IosMemory, req: u32) -> i32 {
        self.requests += 1;
        let cmd = mem.read32(req);
        let fd = mem.read32(req + 8);
        let arg = |i: u32| mem.read32(req + 0x0C + i * 4);
        let a = [arg(0), arg(1), arg(2), arg(3), arg(4)];
        let result = match cmd {
            IPC_OPEN => self.open(mem, a[0], a[1]),
            IPC_CLOSE => {
                if self.handles.remove(&fd).is_some() {
                    IPC_OK
                } else {
                    IPC_EINVAL
                }
            }
            IPC_READ | IPC_WRITE => self.read_write(mem, fd, a[0], a[1] as usize, cmd == IPC_WRITE),
            IPC_SEEK => self.seek(fd, a[0] as i32, a[1]),
            IPC_IOCTL => self.ioctl(mem, fd, a[0], (a[1], a[2] as usize), (a[3], a[4] as usize)),
            IPC_IOCTLV => {
                let (n_in, n_io) = (a[1] as usize, a[2] as usize);
                let vecs: Vec<(u32, usize)> = (0..n_in + n_io)
                    .map(|i| (mem.read32(a[3] + i as u32 * 8), mem.read32(a[3] + i as u32 * 8 + 4) as usize))
                    .collect();
                self.ioctlv(mem, fd, a[0], &vecs[..n_in], &vecs[n_in..])
            }
            _ => IPC_EINVAL,
        };
        mem.write32(req + 4, result as u32);
        mem.write32(req, IPC_REPLY);
        mem.write32(req + 8, fd);
        result
    }

    fn open(&mut self, mem: &dyn IosMemory, path_ptr: u32, mode: u32) -> i32 {
        let path = mem.read_cstr(path_ptr, 64);
        let handle = match path.as_str() {
            "/dev/fs" => Handle::Fs,
            "/dev/es" => Handle::Es,
            "/dev/di" => Handle::Di,
            "/dev/sdio/slot0" => Handle::Sdio,
            "/dev/stm/immediate" | "/dev/stm/eventhook" => Handle::Stm,
            "/dev/usb/oh1/57e/305" => Handle::Bluetooth,
            p if p.starts_with('/') && !p.starts_with("/dev/") => {
                if !self.nand.files.contains_key(p) {
                    return FS_ENOENT;
                }
                Handle::File { path: p.to_string(), pos: 0, write: mode & 2 != 0 }
            }
            other => {
                tracing::warn!("IOS: open of unknown device {other}");
                return IPC_ENOENT;
            }
        };
        let fd = self.next_fd;
        self.next_fd += 1;
        self.handles.insert(fd, handle);
        fd as i32
    }

    fn read_write(&mut self, mem: &mut dyn IosMemory, fd: u32, buf: u32, len: usize, write: bool) -> i32 {
        let Some(Handle::File { path, pos, write: can_write }) = self.handles.get_mut(&fd) else {
            return IPC_EINVAL;
        };
        let file = self.nand.files.entry(path.clone()).or_default();
        if write {
            if !*can_write {
                return -102; // FS_EACCESS
            }
            let data = mem.read(buf, len);
            if file.len() < *pos + len {
                file.resize(*pos + len, 0);
            }
            file[*pos..*pos + len].copy_from_slice(&data);
            *pos += len;
            len as i32
        } else {
            let n = len.min(file.len().saturating_sub(*pos));
            let data = file[*pos..*pos + n].to_vec();
            mem.write(buf, &data);
            *pos += n;
            n as i32
        }
    }

    fn seek(&mut self, fd: u32, off: i32, whence: u32) -> i32 {
        let Some(Handle::File { path, pos, .. }) = self.handles.get_mut(&fd) else {
            return IPC_EINVAL;
        };
        let len = self.nand.files.get(path.as_str()).map_or(0, |f| f.len()) as i64;
        let base = match whence {
            0 => 0,
            1 => *pos as i64,
            _ => len,
        };
        let new = base + off as i64;
        if new < 0 || new > len {
            return -101;
        }
        *pos = new as usize;
        new as i32
    }

    fn ioctl(&mut self, mem: &mut dyn IosMemory, fd: u32, req: u32, input: (u32, usize), out: (u32, usize)) -> i32 {
        let Some(h) = self.handles.get(&fd).cloned() else { return IPC_EINVAL };
        match h {
            Handle::Fs => self.fs_ioctl(mem, req, input, out),
            Handle::Di => self.di_ioctl(mem, req, input, out),
            Handle::Sdio => self.sdio_ioctl(mem, req, input, out),
            Handle::Stm | Handle::Bluetooth => IPC_OK,
            Handle::Es | Handle::File { .. } => IPC_EINVAL,
        }
    }

    fn ioctlv(
        &mut self,
        mem: &mut dyn IosMemory,
        fd: u32,
        req: u32,
        ins: &[(u32, usize)],
        ios: &[(u32, usize)],
    ) -> i32 {
        let Some(h) = self.handles.get(&fd).cloned() else { return IPC_EINVAL };
        match (h, req) {
            // ES_GetTitleID
            (Handle::Es, 0x20) => {
                if let Some(&(p, _)) = ios.first() {
                    mem.write(p, &self.title_id.to_be_bytes());
                }
                IPC_OK
            }
            // ES_GetDataDir
            (Handle::Es, 0x1D) => {
                if let Some(&(p, _)) = ios.first() {
                    let dir = format!("/title/{:08x}/{:08x}/data\0", self.title_id >> 32, self.title_id as u32);
                    mem.write(p, dir.as_bytes());
                }
                IPC_OK
            }
            // ISFS_ReadDir: (path) → (names, count)
            (Handle::Fs, 0x04) => {
                let path = ins.first().map(|&(p, _)| mem.read_cstr(p, 64)).unwrap_or_default();
                let prefix = format!("{}/", path.trim_end_matches('/'));
                let mut names: Vec<String> = self
                    .nand
                    .files
                    .keys()
                    .chain(self.nand.dirs.iter())
                    .filter_map(|k| k.strip_prefix(&prefix).filter(|r| !r.contains('/')).map(|r| r.to_string()))
                    .collect();
                names.sort();
                names.dedup();
                if ios.len() >= 2 {
                    let mut buf = Vec::new();
                    for n in &names {
                        buf.extend_from_slice(n.as_bytes());
                        buf.push(0);
                    }
                    mem.write(ios[0].0, &buf);
                    mem.write32(ios[1].0, names.len() as u32);
                } else if let Some(&(p, _)) = ios.first() {
                    mem.write32(p, names.len() as u32);
                }
                IPC_OK
            }
            // ISFS_GetUsage: (path) → (clusters, inodes)
            (Handle::Fs, 0x0C) => {
                let path = ins.first().map(|&(p, _)| mem.read_cstr(p, 64)).unwrap_or_default();
                let (mut bytes, mut inodes) = (0usize, 0u32);
                for (k, v) in &self.nand.files {
                    if k.starts_with(&path) {
                        bytes += v.len();
                        inodes += 1;
                    }
                }
                if ios.len() >= 2 {
                    mem.write32(ios[0].0, bytes.div_ceil(0x4000) as u32);
                    mem.write32(ios[1].0, inodes);
                }
                IPC_OK
            }
            (Handle::Sdio, 0x07) => {
                // SendCMD via ioctlv: in[0] = command block, out[0] = reply
                let input = ins.first().copied().unwrap_or((0, 0));
                let out = ios.first().copied().unwrap_or((0, 0));
                self.sdio_ioctl(mem, 0x07, input, out)
            }
            (Handle::Bluetooth, _) | (Handle::Stm, _) => IPC_OK,
            (h, r) => {
                tracing::warn!("IOS: unhandled ioctlv {r:#x} on {h:?}");
                IPC_EINVAL
            }
        }
    }

    fn fs_ioctl(&mut self, mem: &mut dyn IosMemory, req: u32, input: (u32, usize), out: (u32, usize)) -> i32 {
        // Attribute blocks start with owner id (4), group id (2), then the
        // 64-byte path.
        let path_at = |base: u32| mem.read_cstr(base + 6, 64);
        match req {
            // GetStats: cluster size, free/used clusters, inodes...
            0x02 => {
                let used: usize = self.nand.files.values().map(|f| f.len().div_ceil(0x4000)).sum();
                let stats =
                    [0x4000u32, 0x7800 - used as u32, used as u32, 0, 0, 0x4000 - self.nand.files.len() as u32, 0];
                for (i, v) in stats.iter().enumerate() {
                    mem.write32(out.0 + i as u32 * 4, *v);
                }
                IPC_OK
            }
            0x03 => {
                let p = path_at(input.0);
                if self.nand.dirs.contains(&p) || self.nand.files.contains_key(&p) {
                    return FS_EEXIST;
                }
                if !self.nand.parent_exists(&p) {
                    return FS_ENOENT;
                }
                self.nand.dirs.insert(p);
                IPC_OK
            }
            0x05 => IPC_OK, // SetAttr
            0x06 => {
                let p = mem.read_cstr(input.0, 64);
                if self.nand.files.contains_key(&p) || self.nand.dirs.contains(&p) {
                    let mut attr = vec![0u8; 0x4C];
                    attr[6..6 + p.len().min(64)].copy_from_slice(&p.as_bytes()[..p.len().min(64)]);
                    attr[0x46] = 3; // owner rw
                    mem.write(out.0, &attr);
                    IPC_OK
                } else {
                    FS_ENOENT
                }
            }
            0x07 => {
                let p = mem.read_cstr(input.0, 64);
                let removed = self.nand.files.remove(&p).is_some() | self.nand.dirs.remove(&p);
                self.nand.files.retain(|k, _| !k.starts_with(&format!("{p}/")));
                if removed {
                    IPC_OK
                } else {
                    FS_ENOENT
                }
            }
            0x08 => {
                let from = mem.read_cstr(input.0, 64);
                let to = mem.read_cstr(input.0 + 64, 64);
                match self.nand.files.remove(&from) {
                    Some(f) => {
                        self.nand.files.insert(to, f);
                        IPC_OK
                    }
                    None => FS_ENOENT,
                }
            }
            0x09 => {
                let p = path_at(input.0);
                if self.nand.files.contains_key(&p) {
                    return FS_EEXIST;
                }
                if !self.nand.parent_exists(&p) {
                    return FS_ENOENT;
                }
                self.nand.files.insert(p, Vec::new());
                IPC_OK
            }
            _ => {
                tracing::warn!("IOS: unhandled /dev/fs ioctl {req:#x}");
                IPC_EINVAL
            }
        }
    }

    fn di_ioctl(&mut self, mem: &mut dyn IosMemory, req: u32, input: (u32, usize), out: (u32, usize)) -> i32 {
        let cmd_word = |i: u32| mem.read32(input.0 + i * 4);
        match req {
            0x12 => {
                let mut info = [0u8; 0x20];
                info[0..4].copy_from_slice(&0x0000_0002u32.to_be_bytes());
                mem.write(out.0, &info[..out.1.min(0x20)]);
                1
            }
            0x70 => {
                let mut id = vec![0u8; 0x20];
                if let Some(d) = self.disc.as_mut() {
                    let _ = d.read(0, &mut id);
                }
                mem.write(out.0, &id);
                1
            }
            0x71 | 0x8D => {
                let (len, offset) = (cmd_word(1) as usize, (cmd_word(2) as u64) << 2);
                let mut buf = vec![0u8; len.min(out.1.max(len))];
                if let Some(d) = self.disc.as_mut() {
                    let _ = d.read(offset, &mut buf);
                }
                mem.write(out.0, &buf);
                1
            }
            0x7A => {
                mem.write32(out.0, if self.disc.is_some() { 2 } else { 1 });
                1
            }
            0x86 | 0x8A | 0xE3 => 1,
            _ => {
                tracing::warn!("IOS: unhandled /dev/di ioctl {req:#x}");
                1
            }
        }
    }

    /// SD host controller: GetStatus (0x0B), GetOCR (0x0C), SendCMD (0x07)
    /// with CMD17/CMD24 single-block read/write.
    fn sdio_ioctl(&mut self, mem: &mut dyn IosMemory, req: u32, input: (u32, usize), out: (u32, usize)) -> i32 {
        match req {
            0x0B => {
                mem.write32(out.0, if self.sd_inserted { 0x0001_0001 } else { 0x0000_0002 });
                IPC_OK
            }
            0x0C => {
                mem.write32(out.0, 0x80FF_8000);
                IPC_OK
            }
            0x01 | 0x02 | 0x04 => IPC_OK,
            0x07 => {
                // Command block: cmd, cmd type, resp type, arg, block count,
                // block size, buffer address.
                let w = |i: u32| mem.read32(input.0 + i * 4);
                let (cmd, arg, blocks, bsize, buf) = (w(0), w(3), w(4) as usize, w(5) as usize, w(6));
                let bsize = if bsize == 0 { 512 } else { bsize };
                match cmd {
                    17 | 18 => {
                        let start = arg as usize;
                        let end = (start + blocks.max(1) * bsize).min(self.sd_card.len());
                        let data = self.sd_card[start.min(end)..end].to_vec();
                        mem.write(buf, &data);
                    }
                    24 | 25 => {
                        let len = blocks.max(1) * bsize;
                        let data = mem.read(buf, len);
                        let start = (arg as usize).min(self.sd_card.len());
                        let end = (start + len).min(self.sd_card.len());
                        self.sd_card[start..end].copy_from_slice(&data[..end - start]);
                    }
                    _ => {}
                }
                if out.0 != 0 {
                    mem.write32(out.0, 0x900); // R1: ready for data, transfer state
                }
                IPC_OK
            }
            _ => IPC_OK,
        }
    }
}

/// Hollywood IPC mailbox registers.
#[derive(Debug, Default, Clone)]
pub struct IpcRegs {
    pub ppcmsg: u32,
    pub ppcctrl: u32,
    pub armmsg: u32,
    pub irq_flag: u32,
    pub irq_mask: u32,
}

pub const CTRL_X1: u32 = 0x01;
pub const CTRL_Y2: u32 = 0x02;
pub const CTRL_Y1: u32 = 0x04;
pub const CTRL_IY1: u32 = 0x10;
pub const CTRL_IY2: u32 = 0x20;
const IRQ_IPC: u32 = 1 << 30;

impl IpcRegs {
    /// PPCCTRL write. Returns true when a new request must be executed.
    pub fn write_ctrl(&mut self, v: u32) -> bool {
        // Y1/Y2 acknowledge on write-1; IY bits are plain.
        let acks = v & (CTRL_Y1 | CTRL_Y2);
        self.ppcctrl &= !acks;
        self.ppcctrl = (self.ppcctrl & !(CTRL_IY1 | CTRL_IY2)) | (v & (CTRL_IY1 | CTRL_IY2));
        v & CTRL_X1 != 0
    }

    /// IOS acknowledged and replied to `req`.
    pub fn complete(&mut self, req: u32) {
        self.armmsg = req;
        self.ppcctrl |= CTRL_Y1 | CTRL_Y2;
        self.irq_flag |= IRQ_IPC;
    }

    pub fn interrupt(&self) -> bool {
        let pending = (self.ppcctrl & CTRL_Y1 != 0 && self.ppcctrl & CTRL_IY1 != 0)
            || (self.ppcctrl & CTRL_Y2 != 0 && self.ppcctrl & CTRL_IY2 != 0);
        pending && self.irq_flag & self.irq_mask & IRQ_IPC != 0
    }
}
