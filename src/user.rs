// eikon: a guest program. A static RISC-V ELF that speaks the Linux syscall ABI
// by number, loaded at a physical address and run in U-mode.
//
// The kernel stays the centre of the machine: the guest gets exactly the calls
// this file agrees to serve, its run is recorded as a value in the generation
// chain, and it cannot touch anything the kernel has not laid out for it. There
// is no MMU yet, so "cannot" means "the loader did not put anything there",
// not "the hardware forbids it" - that distinction is stated in the README, not
// glossed over.
//
// What makes the syscall path work at all: an `ecall` from U-mode traps to
// S-mode as scause 8 (verified against OpenSBI in QEMU). An `ecall` from S-mode
// would go to machine mode instead, which is why the guest has to be in U.

use crate::image::Kernel;

pub const USER_BASE: usize = 0x8400_0000;
pub const USER_STACK_TOP: usize = 0x8500_0000;

/// Where the trampoline must return after the guest ends, and on which stack.
/// Filled in by the asm block in `run`, read by the trap handler.
/// The kernel's own trap frame, saved by the trampoline when we deliberately
/// trap to enter the guest. Returning it is how the guest comes back: sepc and
/// every register are already exactly where they were.
static mut KSP: u64 = 0;
static mut ENTERING: bool = false;
static mut USR_ENTRY: u64 = 0;
static mut USR_SP: u64 = 0;

static mut RUNNING: bool = false;
static mut EXIT_CODE: i64 = -1;
static mut WROTE: usize = 0;
static mut SYSCALLS: u64 = 0;

pub struct Loaded {
    pub entry: u64,
    pub segments: usize,
    pub bytes: usize,
}

fn rd16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn rd32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn rd64(b: &[u8], off: usize) -> u64 {
    let mut v = 0u64;
    let mut i = 0;
    while i < 8 {
        v |= (b[off + i] as u64) << (8 * i);
        i += 1;
    }
    v
}

/// Copy PT_LOAD segments to their physical addresses. No relocations, no
/// interpreter, no dynamic linker: a static ELF pinned where it will run.
pub fn load(elf: &[u8]) -> Result<Loaded, &'static str> {
    if elf.len() < 64 {
        return Err("ENOEXEC: shorter than an ELF header");
    }
    if &elf[0..4] != b"\x7fELF" {
        return Err("ENOEXEC: bad magic");
    }
    if elf[4] != 2 {
        return Err("ENOEXEC: not ELF64");
    }
    if elf[5] != 1 {
        return Err("ENOEXEC: not little-endian");
    }
    if rd16(elf, 18) != 243 {
        return Err("ENOEXEC: not a RISC-V binary");
    }
    if rd16(elf, 16) != 2 {
        return Err("ENOEXEC: not ET_EXEC");
    }
    let entry = rd64(elf, 24);
    let phoff = rd64(elf, 32) as usize;
    let phentsize = rd16(elf, 54) as usize;
    let phnum = rd16(elf, 56) as usize;
    if phnum == 0 || phentsize < 56 || phoff + phnum * phentsize > elf.len() {
        return Err("ENOEXEC: bad program headers");
    }

    let mut segments = 0;
    let mut bytes = 0;
    let mut i = 0;
    while i < phnum {
        let ph = phoff + i * phentsize;
        let p_type = rd32(elf, ph);
        if p_type == 1 {
            // PT_LOAD
            let off = rd64(elf, ph + 8) as usize;
            let vaddr = rd64(elf, ph + 16) as usize;
            let filesz = rd64(elf, ph + 32) as usize;
            let memsz = rd64(elf, ph + 40) as usize;
            if off + filesz > elf.len() || memsz < filesz {
                return Err("ENOEXEC: segment outside the file");
            }
            if vaddr < USER_BASE || vaddr + memsz > USER_STACK_TOP {
                return Err("ENOEXEC: segment outside the guest window");
            }
            unsafe {
                let dst = vaddr as *mut u8;
                core::ptr::copy_nonoverlapping(elf.as_ptr().add(off), dst, filesz);
                core::ptr::write_bytes(dst.add(filesz), 0, memsz - filesz);
            }
            segments += 1;
            bytes += memsz;
        }
        i += 1;
    }
    if segments == 0 {
        return Err("ENOEXEC: nothing to load");
    }
    Ok(Loaded { entry, segments, bytes })
}

fn frame_get(sp: u64, off: usize) -> u64 {
    unsafe { core::ptr::read_volatile((sp as usize + off) as *const u64) }
}

fn frame_set(sp: u64, off: usize, v: u64) {
    unsafe { core::ptr::write_volatile((sp as usize + off) as *mut u64, v) };
}

/// Build the frame that takes us back to S-mode: same trick as a fresh task,
/// with sepc pointing at the label right after the `sret` in `run`.
fn back_to_kernel(code: i64) -> u64 {
    {
        let ksp = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(KSP)) };
        let _ = ksp;
    }
    unsafe {
        EXIT_CODE = code;
        RUNNING = false;
        core::ptr::read_volatile(core::ptr::addr_of!(KSP))
    }
}

/// Called from the trap handler for the trap we raise on purpose to enter the
/// guest. The kernel's context is already saved on its own stack by the
/// trampoline; all that is left is a frame for the guest, placed so that after
/// the trampoline's `addi sp, sp, 256` the guest runs on its own stack.
pub fn enter_guest() -> u64 {
    let entry = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(USR_ENTRY)) };
    let usp = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(USR_SP)) };
    unsafe {
        ENTERING = false;
    }
    let base = usp - 256;
    unsafe { core::ptr::write_bytes(base as *mut u8, 0, 256) };
    frame_set(base, 240, entry); // sepc: the guest's first instruction
    frame_set(base, 248, 0); // SPP=0 -> U-mode, SPIE=0: no interrupts in the guest
    base
}

pub fn entering() -> bool {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(ENTERING)) }
}

pub fn save_kernel_frame(sp: u64) {
    unsafe { KSP = sp };
    // sepc points *at* the trap we raised; step over it, or the kernel resumes
    // by executing the same instruction again. The step is 2 or 4 bytes: with
    // the C extension `unimp` assembles to the compressed `c.unimp`, and a
    // blind `+4` lands in the middle of the next instruction.
    let sepc = frame_get(sp, 240) as usize;
    let insn = unsafe { core::ptr::read_volatile(sepc as *const u16) };
    let step = if insn & 3 == 3 { 4 } else { 2 };
    frame_set(sp, 240, (sepc + step) as u64);
}

fn write_out(buf: usize, len: usize) -> isize {
    if buf == 0 || len == 0 || len > 1 << 20 {
        return -14; // EFAULT, honestly: we do not trust the pointer
    }
    let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, len) };
    match core::str::from_utf8(bytes) {
        Ok(s) => crate::serial::write_str(s),
        Err(_) => {
            // not text: print as hex so the guest's bytes are still visible
            for b in bytes {
                crate::serial::write_str("\\x");
                serial_hex(*b);
            }
        }
    }
    unsafe {
        WROTE += len;
    }
    len as isize
}

fn serial_hex(b: u8) {
    const D: &[u8; 16] = b"0123456789abcdef";
    let hi = D[(b >> 4) as usize] as char;
    let lo = D[(b & 0xf) as usize] as char;
    let mut buf = [0u8; 2];
    buf[0] = hi as u8;
    buf[1] = lo as u8;
    crate::serial::write_str(unsafe { core::str::from_utf8_unchecked(&buf) });
}

/// Served syscalls: write, exit, exit_group, brk, set_tid_address.
pub fn syscall(sp: u64) -> u64 {
    let nr = frame_get(sp, 120);
    let a0 = frame_get(sp, 64);
    let a1 = frame_get(sp, 72);
    let a2 = frame_get(sp, 80);
    unsafe { SYSCALLS += 1 };

    let ret: i64 = match nr {
        64 => write_out(a1 as usize, a2 as usize) as i64,
        93 | 94 => return back_to_kernel(a0 as i64),
        214 => a0 as i64, // brk: one flat window, the break is where they asked
        96 => 0,          // set_tid_address
        174 => 1,         // getuid: the guest is the kernel's guest
        _ => -38,         // ENOSYS
    };
    frame_set(sp, 64, ret as u64);
    // sepc points *at* the ecall; without stepping over it the guest returns to
    // the same instruction and calls write forever
    let sepc = frame_get(sp, 240);
    frame_set(sp, 240, sepc + 4);
    sp
}

/// Anything the guest does that we did not agree to serve ends its run.
pub fn fault(sp: u64, cause: u64) -> u64 {
    crate::serial::write_str("\\nguest faulted: scause=");
    crate::serial::write_num(cause as i64);
    crate::serial::write_str(", sepc=");
    crate::serial::write_num(frame_get(sp, 240) as i64);
    crate::serial::write_str(", stval=");
    let stval: u64;
    unsafe { core::arch::asm!("csrr {}, stval", out(reg) stval, options(nomem, nostack)) };
    crate::serial::write_num(stval as i64);
    crate::serial::write_str("\\n");
    back_to_kernel(-2)
}

pub fn running() -> bool {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(RUNNING)) }
}

pub fn wrote() -> usize {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WROTE)) }
}

pub fn last_exit() -> i64 {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(EXIT_CODE)) }
}

pub fn syscalls() -> u64 {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SYSCALLS)) }
}

/// Load the ELF and drop to U-mode. Returns (exit code, bytes written).
pub fn run(k: &Kernel, elf: &[u8]) -> Result<(i64, usize), &'static str> {
    let l = load(elf)?;
    unsafe {
        RUNNING = true;
        WROTE = 0;
        SYSCALLS = 0;
    }
    unsafe {
        USR_ENTRY = l.entry;
        USR_SP = (USER_STACK_TOP - 16) as u64;
        ENTERING = true;
        // The guest runs without preemption for now: the timer is silenced,
        // because saving a *user* context into a task frame is a different job
        // from saving a kernel one, and half of it would be a lie.
        crate::trap::disable_timer_irq();
        crate::trap::arm_timer(u64::MAX / 2);
        // a deliberate trap: the trampoline saves the kernel context exactly as
        // it would on any other trap, so coming back is symmetric and no
        // register has to be hand-assembled
        core::arch::asm!("unimp", options(nostack));
    }
    let _ = k;
    let code = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(EXIT_CODE)) };
    Ok((code, wrote()))
}

#[allow(dead_code)]
pub fn loaded_summary(elf: &[u8]) -> Result<(usize, usize), &'static str> {
    let l = load(elf)?;
    Ok((l.segments, l.bytes))
}
