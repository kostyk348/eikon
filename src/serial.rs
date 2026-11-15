// eikon: serial console on the QEMU virt machine (16550 UART at 0x1000_0000).
//
// The console is deliberately the only device driver in this kernel: a kernel
// that talks before it claims is a kernel you can trust.

const UART: usize = 0x1000_0000;

unsafe fn reg(off: usize) -> *mut u8 {
    (UART + off) as *mut u8
}

pub fn init() {
    unsafe {
        core::ptr::write_volatile(reg(1), 0x00); // no interrupts
        core::ptr::write_volatile(reg(3), 0x80); // DLAB on
        core::ptr::write_volatile(reg(0), 0x03); // divisor low: 38.4k-ish
        core::ptr::write_volatile(reg(1), 0x00); // divisor high
        core::ptr::write_volatile(reg(3), 0x03); // 8N1, DLAB off
        core::ptr::write_volatile(reg(2), 0x07); // FIFO on, clear
    }
}

pub fn write_byte(b: u8) {
    unsafe {
        // Wait for the transmit holding register to drain: a bounded spin, so a
        // broken emulator cannot hang the boot forever.
        let mut spins = 0u32;
        while core::ptr::read_volatile(reg(5)) & 0x20 == 0 {
            spins += 1;
            if spins > 20_000_000 {
                break;
            }
        }
        core::ptr::write_volatile(reg(0), b);
    }
}

pub fn write_str(s: &str) {
    for b in s.as_bytes() {
        if *b == b'\n' {
            write_byte(b'\r');
        }
        write_byte(*b);
    }
}

pub fn write_num(n: i64) {
    write_str(crate::print::int_str(n));
}

/// Non-blocking read: is there a byte? The REPL must not spin on a queue.
pub fn try_read() -> Option<u8> {
    unsafe {
        if core::ptr::read_volatile(reg(5)) & 0x01 == 0 {
            return None;
        }
        Some(core::ptr::read_volatile(reg(0)))
    }
}

/// RISC-V `time` CSR: ticks at 10 MHz on qemu virt, i.e. 100 ns each.
pub fn ticks() -> u64 {
    let t: u64;
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) t, options(nomem, nostack));
    }
    t
}
