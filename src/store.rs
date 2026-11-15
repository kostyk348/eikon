// eikon: the image on disk. Persistence with no filesystem at all.
//
// A generation is a form, a form prints canonically, so saving the machine means
// printing the root into sectors and reading it back. There is no inode, no
// directory, no journal and no fsync: either the header sector names a length
// whose body reads back and parses, or the image was never committed.
//
// Layout (512-byte sectors):
//   sector 0 : "EIKONIMG" | version u32 | generation u64 | length u32
//   sector 1+: the printed root form, little-endian by construction (it is text)

use crate::image::Kernel;

const MAGIC: &[u8; 8] = b"EIKONIMG";
const VERSION: u32 = 1;
pub const MAX_IMAGE: usize = 7 * 512; // what fits after the header, in bytes

static mut SOURCE: [u8; MAX_IMAGE] = [0; MAX_IMAGE];

fn source() -> &'static mut [u8; MAX_IMAGE] {
    unsafe { &mut *core::ptr::addr_of_mut!(SOURCE) }
}

/// Print the current root and write it to the disk. Returns the byte count.
pub fn save(k: &mut Kernel) -> Result<u64, &'static str> {
    if !k.blk.ok {
        return Err("ENODEV: no block device (start qemu with a virtio-blk disk)");
    }
    let root = k.img.root();
    let mut buf = crate::print::Buf::new();
    let text = crate::print::to_buf(&k.heap, root, &mut buf)?.as_bytes().len();
    if text > MAX_IMAGE {
        return Err("EFBIG: the image does not fit in the first sectors");
    }

    // header
    let mut head = [0u8; crate::virtio::SECTOR];
    head[0..8].copy_from_slice(MAGIC);
    head[8..12].copy_from_slice(&VERSION.to_le_bytes());
    head[12..20].copy_from_slice(&k.img.generation().to_le_bytes());
    head[20..24].copy_from_slice(&(text as u32).to_le_bytes());
    k.blk.write_sector(0, &head)?;

    // body: one sector at a time, straight out of the printer's buffer
    let mut sector = [0u8; crate::virtio::SECTOR];
    let mut off = 0usize;
    let mut n = 0u64;
    while off < text {
        let take = core::cmp::min(crate::virtio::SECTOR, text - off);
        sector[..take].copy_from_slice(&buf.data[off..off + take]);
        for b in sector.iter_mut().skip(take) {
            *b = 0;
        }
        k.blk.write_sector(1 + n, &sector)?;
        off += take;
        n += 1;
    }
    Ok(text as u64)
}

/// Read the image back and publish it as a new generation. Returns the
/// generation it restored, or None when the disk holds no image.
pub fn load(k: &mut Kernel) -> Result<Option<u64>, &'static str> {
    if !k.blk.ok {
        return Ok(None);
    }
    let mut head = [0u8; crate::virtio::SECTOR];
    k.blk.read_sector(0, &mut head)?;
    if &head[0..8] != MAGIC {
        return Ok(None);
    }
    let version = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
    if version != VERSION {
        return Err("EINVAL: unknown image version on disk");
    }
    let len = u32::from_le_bytes([head[20], head[21], head[22], head[23]]) as usize;
    if len == 0 || len > MAX_IMAGE {
        return Err("EINVAL: bad image length on disk");
    }

    let src = source();
    let mut off = 0usize;
    let mut n = 0u64;
    while off < len {
        let mut sector = [0u8; crate::virtio::SECTOR];
        k.blk.read_sector(1 + n, &mut sector)?;
        let take = core::cmp::min(crate::virtio::SECTOR, len - off);
        src[off..off + take].copy_from_slice(&sector[..take]);
        off += take;
        n += 1;
    }
    let text: &'static str = unsafe { core::str::from_utf8_unchecked(&src[..len]) };

    let mut r = crate::read::Reader::new(text);
    let forms = r.all(&mut k.heap)?;
    let form = k.heap.car(forms)?;
    let gen = k.img.commit(form)?;
    Ok(Some(gen))
}
