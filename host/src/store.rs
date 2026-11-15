// The disk image is a kernel feature (virtio). On the host, `(save)` says so
// instead of pretending to have written anything.
use crate::image::Kernel;

pub fn save(_k: &Kernel) -> Result<u64, &'static str> {
    Err("ENODEV: no disk on the host")
}

pub fn load(_k: &Kernel) -> Result<Option<u64>, &'static str> {
    Ok(None)
}
