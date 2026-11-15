// eikon: traps, the timer, and preemption.
//
// Until now the kernel was a program: it ran to completion because nothing could
// interrupt it. This file is what changes that - the first real power a kernel
// has, the power to take the processor away from running code.
//
// Design, deliberately minimal:
//   * stvec -> trap_entry, an assembly trampoline that saves 30 registers plus
//     sepc/sstatus on the *current* stack (256 bytes, 16-aligned);
//   * the Rust handler returns the stack pointer to resume with, which is how a
//     context switch happens without a second copy of the register file;
//   * a fresh task is a prepared frame on its own stack, whose sepc points at
//     task_entry and whose sstatus says "return to S-mode with interrupts on";
//   * the timer is Sstc's stimecmp (no SBI call per tick, no firmware in the
//     hot path).

use crate::val::Val;

pub const FRAME: usize = 256;
const FRAME_SEPC: usize = 240;
const FRAME_SSTATUS: usize = 248;

// bit helpers for sstatus
const SSTATUS_SPP: u64 = 1 << 8; // previous privilege: supervisor
const SSTATUS_SPIE: u64 = 1 << 5; // interrupts on after sret
const SSTATUS_SIE: u64 = 1 << 1;

const SIE_STIE: u64 = 1 << 5; // supervisor timer interrupt enable


core::arch::global_asm!(
    r#"
.section .text
.global trap_entry
.align 4
trap_entry:
    addi sp, sp, -256
    sd ra,   0(sp)
    sd gp,   8(sp)
    sd tp,  16(sp)
    sd t0,  24(sp)
    sd t1,  32(sp)
    sd t2,  40(sp)
    sd s0,  48(sp)
    sd s1,  56(sp)
    sd a0,  64(sp)
    sd a1,  72(sp)
    sd a2,  80(sp)
    sd a3,  88(sp)
    sd a4,  96(sp)
    sd a5, 104(sp)
    sd a6, 112(sp)
    sd a7, 120(sp)
    sd s2, 128(sp)
    sd s3, 136(sp)
    sd s4, 144(sp)
    sd s5, 152(sp)
    sd s6, 160(sp)
    sd s7, 168(sp)
    sd s8, 176(sp)
    sd s9, 184(sp)
    sd s10, 192(sp)
    sd s11, 200(sp)
    sd t3, 208(sp)
    sd t4, 216(sp)
    sd t5, 224(sp)
    sd t6, 232(sp)
    csrr t0, sepc
    sd   t0, 240(sp)
    csrr t0, sstatus
    sd   t0, 248(sp)

    mv a0, sp
    call trap_handler
    mv sp, a0

    // Restore everything EXCEPT t0/t1, which are needed to write sepc/sstatus
    // at the very end. Writing sstatus with SIE=1 while still inside the
    // handler is a bug with teeth: a tick arriving in that window re-enters the
    // handler, pushes a second frame under the first, and the scheduler saves
    // that nested sp as the task's own - the task then runs on someone else's
    // stack. So: SIE stays 0 here, and `sret` restores it from SPIE.
    ld ra,   0(sp)
    ld gp,   8(sp)
    ld tp,  16(sp)
    ld t2,  40(sp)
    ld s0,  48(sp)
    ld s1,  56(sp)
    ld a0,  64(sp)
    ld a1,  72(sp)
    ld a2,  80(sp)
    ld a3,  88(sp)
    ld a4,  96(sp)
    ld a5, 104(sp)
    ld a6, 112(sp)
    ld a7, 120(sp)
    ld s2, 128(sp)
    ld s3, 136(sp)
    ld s4, 144(sp)
    ld s5, 152(sp)
    ld s6, 160(sp)
    ld s7, 168(sp)
    ld s8, 176(sp)
    ld s9, 184(sp)
    ld s10, 192(sp)
    ld s11, 200(sp)
    ld t3, 208(sp)
    ld t4, 216(sp)
    ld t5, 224(sp)
    ld t6, 232(sp)

    ld t1, 248(sp)          // saved sstatus
    li t2, -3
    and t1, t1, t2          // clear SIE: interrupts come back with sret
    csrw sstatus, t1
    ld t1, 240(sp)          // saved sepc
    csrw sepc, t1
    ld t0,  24(sp)          // t0/t1 restored last, with interrupts still off
    ld t1,  32(sp)
    addi sp, sp, 256
    sret
"#
);

// CSR instructions take a 12-bit number, not a register: each accessor is
// written out. A generic helper with `in(reg)` does not assemble.
fn read_scause() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("csrr {}, scause", out(reg) v, options(nomem, nostack)) };
    v
}

/// Arm the timer through SBI (ext TIME, fid 0). Writing the S-mode `stimecmp`
/// CSR directly is cheaper, but it only exists when M-mode enabled Sstc via
/// menvcfg - and from S-mode there is no way to check that. SBI always works.
fn sbi_set_timer(when: u64) {
    let err: i64;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0x5449_4D45usize, // "TIME"
            in("a6") 0usize,           // set_timer
            in("a0") when as usize,
            lateout("a0") err,
            options(nostack)
        )
    };
    if err != 0 {
        crate::serial::write_str("  sbi set_timer failed: ");
        crate::serial::write_num(err);
        crate::serial::write_str("\n");
    }
}

fn set_sie(bits: u64) {
    unsafe { core::arch::asm!("csrs sie, {}", in(reg) bits, options(nomem, nostack)) };
}

fn clear_sie(bits: u64) {
    unsafe { core::arch::asm!("csrc sie, {}", in(reg) bits, options(nomem, nostack)) };
}

fn set_sstatus(bits: u64) {
    unsafe { core::arch::asm!("csrs sstatus, {}", in(reg) bits, options(nomem, nostack)) };
}

fn clear_sstatus(bits: u64) {
    unsafe { core::arch::asm!("csrc sstatus, {}", in(reg) bits, options(nomem, nostack)) };
}

/// Interrupts off, and the flag we need to restore the previous state.
pub fn irq_save() -> u64 {
    let prev: u64;
    unsafe {
        core::arch::asm!(
            "csrr {}, sstatus",
            "csrci sstatus, 2", // clear SIE
            out(reg) prev,
            options(nomem, nostack)
        )
    };
    prev & SSTATUS_SIE
}

pub fn irq_restore(prev: u64) {
    if prev != 0 {
        set_sstatus(SSTATUS_SIE);
    }
}

extern "C" {
    fn trap_entry();
}

pub fn init() {
    unsafe {
        #[allow(function_casts_as_integer)]
        let addr: usize = trap_entry as usize;
        core::arch::asm!("csrw stvec, {}", in(reg) addr, options(nomem, nostack));
    }
}





pub fn now() -> u64 {
    let t: u64;
    unsafe { core::arch::asm!("rdtime {}", out(reg) t, options(nomem, nostack)) };
    t
}

/// Arm the next timer interrupt `period` ticks from now (Sstc, S-mode direct).
pub fn arm_timer(period: u64) {
    sbi_set_timer(now() + period);
}

pub fn enable_timer_irq() {
    set_sie(SIE_STIE);
    // wfi wakes on a pending interrupt, but the hart only *takes* it when
    // sstatus.SIE is set - forgetting this looks exactly like a dead timer.
    set_sstatus(SSTATUS_SIE);
}

pub fn disable_timer_irq() {
    clear_sie(SIE_STIE);
    clear_sstatus(SSTATUS_SIE);
}

/// A frame that, when resumed by the trampoline, starts `entry` in S-mode with
/// interrupts enabled. Returns the stack pointer to hand to the trampoline.
pub fn new_task_frame(stack_top: usize, entry: usize) -> usize {
    let sp = stack_top - FRAME;
    // the trampoline restores everything from here; only two slots matter
    unsafe {
        let p = sp as *mut u64;
        for i in 0..(FRAME / 8) {
            core::ptr::write_volatile(p.add(i), 0);
        }
        core::ptr::write_volatile(p.add(0), entry as u64); // ra
        core::ptr::write_volatile(p.add(FRAME_SEPC / 8), entry as u64);
        core::ptr::write_volatile(p.add(FRAME_SSTATUS / 8), SSTATUS_SPP | SSTATUS_SPIE);
    }
    sp
}

// ---- the scheduler hook -------------------------------------------------

/// Set by sched.rs: given the interrupted frame, return the frame to resume.
pub static mut SWITCH: Option<fn(u64) -> u64> = None;

#[no_mangle]
pub extern "C" fn trap_handler(sp: u64) -> u64 {
    let cause = read_scause();
    // interrupt = high bit; supervisor timer = 5; S-mode ecall = 9
    if cause & (1 << 63) != 0 {
        let irq = cause & 0xff;
        if irq == 5 {
            let sch = crate::sched::sched();
            sch.ticks += 1;
            arm_timer(crate::sched::TICK);
        }
    }
    let f = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SWITCH)) };
    match f {
        Some(switch) => switch(sp),
        None => sp,
    }
}

/// Irq-safe bump allocation: the arena is shared between tasks, so the read of
/// the pointer and the write of the next one must not be split by a timer tick.
pub fn with_irq_off<R>(f: impl FnOnce() -> R) -> R {
    let prev = irq_save();
    let r = f();
    irq_restore(prev);
    r
}

#[allow(dead_code)]
pub fn val_is_int(v: Val) -> bool {
    matches!(v, Val::Int(_))
}
