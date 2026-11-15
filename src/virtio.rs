// eikon: a virtio-mmio block device driver. The smallest one that is honest.
//
// qemu -machine virt puts eight virtio-mmio slots at 0x1000_1000 + n*0x1000.
// A block device there needs: feature handshake, one split virtqueue, and a
// three-descriptor request (header, data, status). No interrupts: we poll the
// used ring, because a kernel this size has nothing else to do.

const MMIO_BASE: usize = 0x1000_1000;
const MMIO_STRIDE: usize = 0x1000;
const MMIO_SLOTS: usize = 8;

// register offsets
const R_MAGIC: usize = 0x000;
const R_VERSION: usize = 0x004;
const R_DEVICE_ID: usize = 0x008;
const R_DEVICE_FEATURES: usize = 0x010;
const R_DRIVER_FEATURES: usize = 0x020;
const R_QUEUE_SEL: usize = 0x030;
const R_QUEUE_NUM_MAX: usize = 0x034;
const R_QUEUE_NUM: usize = 0x038;
const R_QUEUE_READY: usize = 0x044;
// legacy layout (virtio-mmio 0.95): page size, alignment, queue page frame
const R_GUEST_PAGE_SIZE: usize = 0x028;
const R_QUEUE_ALIGN: usize = 0x03c;
const R_QUEUE_PFN: usize = 0x040;
const R_QUEUE_NOTIFY: usize = 0x050;
const R_INTERRUPT_STATUS: usize = 0x060;
const R_INTERRUPT_ACK: usize = 0x064;
const R_STATUS: usize = 0x070;
const R_DESC_LOW: usize = 0x080;
const R_DESC_HIGH: usize = 0x084;
const R_DRIVER_LOW: usize = 0x090;
const R_DRIVER_HIGH: usize = 0x094;
const R_DEVICE_LOW: usize = 0x0a0;
const R_DEVICE_HIGH: usize = 0x0a4;

const S_ACK: u32 = 1;
const S_DRIVER: u32 = 2;
const S_DRIVER_OK: u32 = 4;
const S_FEATURES_OK: u32 = 8;

const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;

pub const SECTOR: usize = 512;
const QSIZE: usize = 8;

// One page holds descriptors, the rings and the request buffers. Layout is hand
// made because the device reads these structures by address, not by name.
#[repr(C, align(4096))]
struct QMem {
    // Two pages: descriptors + available ring in the first, the used ring in the
    // second. Legacy virtio requires that the used ring starts on a page.
    bytes: [u8; 8192],
}

static mut QMEM: QMem = QMem { bytes: [0; 8192] };

const DESC_OFF: usize = 0; // 8 * 16 = 128
const AVAIL_OFF: usize = 128; // flags,idx,ring[8],used_event
const USED_OFF: usize = 4096; // flags,idx,ring[8],avail_event (page-aligned for legacy)
const HDR_OFF: usize = 256; // virtio_blk_req header, 16 bytes
const DATA_OFF: usize = 512; // one sector
const STATUS_OFF: usize = 1024; // one byte

pub struct Blk {
    base: usize,
    pub ok: bool,
    pub capacity: u64,
    last_used: u16,
}

unsafe fn rd(base: usize, off: usize) -> u32 {
    core::ptr::read_volatile((base + off) as *const u32)
}

unsafe fn wr(base: usize, off: usize, v: u32) {
    core::ptr::write_volatile((base + off) as *mut u32, v)
}

fn qmem() -> &'static mut QMem {
    unsafe { &mut *core::ptr::addr_of_mut!(QMEM) }
}

impl Blk {
    pub const fn none() -> Self {
        Blk { base: 0, ok: false, capacity: 0, last_used: 0 }
    }

    /// What the kernel sees in the virtio-mmio slots. Printed only when the
    /// device is missing: "not present" without numbers is a guess, not a fact.
    pub fn probe() {
        let mut i = 0;
        while i < MMIO_SLOTS {
            let base = MMIO_BASE + i * MMIO_STRIDE;
            let (m, v, d) = unsafe { (rd(base, R_MAGIC), rd(base, R_VERSION), rd(base, R_DEVICE_ID)) };
            if m != 0 || v != 0 || d != 0 {
                crate::serial::write_str("    slot ");
                crate::serial::write_num(i as i64);
                crate::serial::write_str(": magic ");
                crate::serial::write_num((m >> 16) as i64);
                crate::serial::write_str(" version ");
                crate::serial::write_num(v as i64);
                crate::serial::write_str(" devid ");
                crate::serial::write_num(d as i64);
                crate::serial::write_str("\n");
            }
            i += 1;
        }
    }

    pub fn init() -> Self {
        let mut i = 0;
        while i < MMIO_SLOTS {
            let base = MMIO_BASE + i * MMIO_STRIDE;
            let magic = unsafe { rd(base, R_MAGIC) };
            let version = unsafe { rd(base, R_VERSION) };
            let devid = unsafe { rd(base, R_DEVICE_ID) };
            // version 2 is virtio 1.0 (modern), version 1 is the legacy layout
            // QEMU still defaults to. Both are supported; the kernel should not
            // need a command line flag to see its own disk.
            if magic == 0x7472_6976 && (version == 2 || version == 1) && devid == 2 {
                if let Some(b) = Self::bring_up(base, version) {
                    return b;
                }
            }
            i += 1;
        }
        Self::none()
    }

    fn bring_up(base: usize, version: u32) -> Option<Self> {
        unsafe {
            wr(base, R_STATUS, 0);
            wr(base, R_STATUS, S_ACK);
            wr(base, R_STATUS, S_ACK | S_DRIVER);
            let _features = rd(base, R_DEVICE_FEATURES);
            // No features: no indirect descriptors, no event index, no flush.
            wr(base, R_DRIVER_FEATURES, 0);

            wr(base, R_QUEUE_SEL, 0);
            let max = rd(base, R_QUEUE_NUM_MAX);
            if max == 0 || max < QSIZE as u32 {
                return None;
            }
            wr(base, R_QUEUE_NUM, QSIZE as u32);

            let q = qmem();
            let base_ptr = q.bytes.as_mut_ptr() as usize;

            if version == 2 {
                wr(base, R_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK);
                if rd(base, R_STATUS) & S_FEATURES_OK == 0 {
                    return None;
                }
                wr(base, R_DESC_LOW, (base_ptr + DESC_OFF) as u32);
                wr(base, R_DESC_HIGH, ((base_ptr + DESC_OFF) >> 32) as u32);
                wr(base, R_DRIVER_LOW, (base_ptr + AVAIL_OFF) as u32);
                wr(base, R_DRIVER_HIGH, ((base_ptr + AVAIL_OFF) >> 32) as u32);
                wr(base, R_DEVICE_LOW, (base_ptr + USED_OFF) as u32);
                wr(base, R_DEVICE_HIGH, ((base_ptr + USED_OFF) >> 32) as u32);
                wr(base, R_QUEUE_READY, 1);
                wr(base, R_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK);
            } else {
                // legacy: the queue is addressed by page frame number, and the
                // used ring follows the available ring on the next page. The
                // page size and alignment registers are part of the legacy
                // layout (0x028 and 0x03c) - the modern layout has neither.
                wr(base, R_GUEST_PAGE_SIZE, 4096);
                wr(base, R_QUEUE_ALIGN, 4096);
                wr(base, R_QUEUE_PFN, (base_ptr >> 12) as u32);
                wr(base, R_STATUS, S_ACK | S_DRIVER | S_DRIVER_OK);
            }

            // capacity in sectors lives in the device-specific config space
            let cap_lo = rd(base, 0x100) as u64;
            let cap_hi = rd(base, 0x104) as u64;
            Some(Blk { base, ok: true, capacity: cap_lo | (cap_hi << 32), last_used: 0 })
        }
    }

    fn set_desc(&self, i: usize, addr: usize, len: u32, flags: u16, next: u16) {
        let q = qmem();
        let d = &mut q.bytes[DESC_OFF + i * 16..DESC_OFF + i * 16 + 16];
        d[0..8].copy_from_slice(&(addr as u64).to_le_bytes());
        d[8..12].copy_from_slice(&len.to_le_bytes());
        d[12..14].copy_from_slice(&flags.to_le_bytes());
        d[14..16].copy_from_slice(&next.to_le_bytes());
    }

    /// One request: 1 = device->memory, 0 = memory->device.
    fn request(&mut self, write_to_host: bool, sector: u64, nbytes: u32) -> Result<(), &'static str> {
        if !self.ok {
            return Err("ENODEV: no block device");
        }
        let q = qmem();
        let base_ptr = q.bytes.as_mut_ptr() as usize;

        // header: type, reserved, sector
        let h = &mut q.bytes[HDR_OFF..HDR_OFF + 16];
        let ty: u32 = if write_to_host { 0 } else { 1 }; // VIRTIO_BLK_T_IN / OUT
        h[0..4].copy_from_slice(&ty.to_le_bytes());
        h[4..8].copy_from_slice(&0u32.to_le_bytes());
        h[8..16].copy_from_slice(&sector.to_le_bytes());
        q.bytes[STATUS_OFF] = 0xff;

        let data_flags = DESC_F_NEXT | if write_to_host { DESC_F_WRITE } else { 0 };
        self.set_desc(0, base_ptr + HDR_OFF, 16, DESC_F_NEXT, 1);
        self.set_desc(1, base_ptr + DATA_OFF, nbytes, data_flags, 2);
        self.set_desc(2, base_ptr + STATUS_OFF, 1, DESC_F_WRITE, 0);

        // publish the head descriptor in the available ring
        let idx_off = AVAIL_OFF + 2;
        let mut idx = u16::from_le_bytes([q.bytes[idx_off], q.bytes[idx_off + 1]]);
        let slot = (idx as usize) % QSIZE;
        let ring = AVAIL_OFF + 4 + slot * 2;
        q.bytes[ring] = 0;
        q.bytes[ring + 1] = 0;
        idx = idx.wrapping_add(1);
        q.bytes[idx_off] = idx as u8;
        q.bytes[idx_off + 1] = (idx >> 8) as u8;

        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        unsafe { wr(self.base, R_QUEUE_NOTIFY, 0) };

        // poll the used ring: a kernel without interrupts must not pretend
        let used_idx_off = USED_OFF + 2;
        let up = unsafe { q.bytes.as_mut_ptr().add(used_idx_off) };
        let status_ptr = unsafe { q.bytes.as_mut_ptr().add(STATUS_OFF) };
        let mut spins = 0u32;
        loop {
            let lo = unsafe { core::ptr::read_volatile(up) } as u16;
            let hi = unsafe { core::ptr::read_volatile(up.add(1)) } as u16;
            let used = lo | (hi << 8);
            if used != self.last_used {
                self.last_used = used;
                break;
            }
            spins += 1;
            if spins > 20_000_000 {
                crate::serial::write_str("  request stuck: avail.idx=");
                crate::serial::write_num(idx as i64);
                crate::serial::write_str(" used.idx=");
                crate::serial::write_num(used as i64);
                crate::serial::write_str(" status=");
                crate::serial::write_num(unsafe { core::ptr::read_volatile(status_ptr) } as i64);
                crate::serial::write_str(" irq=");
                crate::serial::write_num(unsafe { rd(self.base, R_INTERRUPT_STATUS) } as i64);
                crate::serial::write_str("\n");
                return Err("ETIMEDOUT: block request never completed");
            }
            core::hint::spin_loop();
        }
        // acknowledge (we did not enable interrupts, so this is just hygiene)
        unsafe {
            let st = rd(self.base, R_INTERRUPT_STATUS);
            if st != 0 {
                wr(self.base, R_INTERRUPT_ACK, st);
            }
        }
        match unsafe { core::ptr::read_volatile(status_ptr) } {
            0 => Ok(()),
            1 => Err("EIO: device reported an I/O error"),
            _ => Err("EOPNOTSUPP: device does not support the request"),
        }
    }

    pub fn read_sector(&mut self, sector: u64, out: &mut [u8; SECTOR]) -> Result<(), &'static str> {
        self.request(true, sector, SECTOR as u32)?;
        let q = qmem();
        out.copy_from_slice(&q.bytes[DATA_OFF..DATA_OFF + SECTOR]);
        Ok(())
    }

    pub fn write_sector(&mut self, sector: u64, data: &[u8; SECTOR]) -> Result<(), &'static str> {
        let q = qmem();
        q.bytes[DATA_OFF..DATA_OFF + SECTOR].copy_from_slice(data);
        self.request(false, sector, SECTOR as u32)
    }
}
