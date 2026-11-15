// A real static RISC-V ELF that speaks the Linux syscall ABI by number:
// write(1, ...) = 64, exit(code) = 93. No libc, no crates, no runtime.
//
// The point is not that this program is impressive - it is that eikon can load
// it, drop to U-mode, serve its syscalls and take the exit code back, with the
// guest's run recorded as a value in the kernel's generation chain.

#![no_std]
#![no_main]

use core::arch::asm;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { asm!("li a7, 93", "li a0, 134", "ecall", options(noreturn)) }
}

fn write(fd: usize, buf: &[u8]) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "ecall",
            in("a7") 64usize,
            in("a0") fd,
            in("a1") buf.as_ptr() as usize,
            in("a2") buf.len(),
            lateout("a0") ret,
            options(nostack)
        )
    };
    ret
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let msg = b"hello from a Linux ELF, served by eikon\n";
    let n = write(1, msg);
    // a second write proves the trap returns to the same user context
    let msg2 = b"second syscall, same context\n";
    write(1, msg2);
    // the exit code is ours to choose; 7 is the classic "something happened"
    let _ = n;
    unsafe { asm!("li a7, 93", "li a0, 7", "ecall", options(noreturn)) }
}
