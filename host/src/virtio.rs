// No block device on the host: the guest window and the disk live in the
// kernel's own demo, not in the portable core.
pub struct Blk {
    pub ok: bool,
    pub capacity: u64,
}

impl Blk {
    pub const fn none() -> Self {
        Blk { ok: false, capacity: 0 }
    }

    pub fn read_sector(&self, _sector: u64, _out: &mut [u8; 512]) -> Result<(), &'static str> {
        Err("ENODEV: no block device on the host")
    }

    pub fn write_sector(&self, _sector: u64, _data: &[u8; 512]) -> Result<(), &'static str> {
        Err("ENODEV: no block device on the host")
    }
}
