// eikon - a homoiconic kernel.
//
// One sentence: the machine is an image of forms, the language is the interface,
// and everything the kernel does is a form going in and a form coming out.
//
// Temple OS as the attitude: one person, one language, one machine, no layers
// between the program and the metal, the compiler and the shell in the same
// system, everything visible. Not Temple OS as the code: this kernel has
// capabilities and immutable generations, because those are the two things our
// own work (document-addressed state, contracts, evidence) proved worth having.
//
// Boots under OpenSBI on qemu-system-riscv64 -machine virt. No bootloader of our
// own, no crates, no allocator, no scheduler yet. It prints, it evaluates, it
// commits, it audits - and every claim below is a check that runs at boot.

#![no_std]
#![no_main]

mod bytecode;
mod eval;
mod sched;
mod store;
mod trap;
mod virtio;
mod image;
mod print;
mod read;
mod serial;
mod val;

use image::Kernel;
use val::{Val, NIL};

// The boot image: a program that is also data, printed back byte for byte.
const BOOT_IMAGE: &str = r#"
; eikon boot image - read, evaluated, then committed as the root form
(def (fact n) (if (< n 2) 1 (* n (fact (- n 1)))))
(def (fib n)  (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
(def sq (lambda (x) (* x x)))
(def base (list 'plant 200 1))
(print (fact 10))
(print (fib 15))
(print (sq 7))
base
"#;

pub static mut KERNEL: Kernel = Kernel::new();
static mut ROUNDTRIP: [u8; 4096] = [0; 4096];

core::arch::global_asm!(
    r#"
.section .text.entry
.global _start
_start:
    # OpenSBI jumps here in S-mode with the MMU off, a0 = hartid, a1 = dtb.
    # Rust assumes .bss is zero: make it true before touching any static.
    la t0, __bss_start
    la t1, __bss_end
1:  bgeu t0, t1, 2f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 1b
2:  la sp, __stack_top
    call kernel_main
3:  j 3b
"#
);

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial::write_str("\nPANIC: ");
    if let Some(m) = info.message().as_str() {
        serial::write_str(m);
        serial::write_str(" at ");
    }
    if let Some(l) = info.location() {
        serial::write_str(l.file());
        serial::write_str(":");
        serial::write_num(l.line() as i64);
    }
    serial::write_str("\n");
    loop {}
}

fn sym(k: &Kernel, s: &'static str) -> Val {
    Val::Sym(k.heap.intern(s).unwrap_or(0))
}

/// Read and evaluate a source string in the global environment.
fn eval_str(k: &Kernel, src: &'static str) -> Result<Val, &'static str> {
    let mut r = read::Reader::new(src);
    let mut forms = r.all(&k.heap)?;
    let mut last = NIL;
    while let Val::Cons(_) = forms {
        last = eval::eval(k, NIL, k.heap.car(forms)?)?;
        forms = k.heap.cdr(forms)?;
    }
    Ok(last)
}

fn is_int(v: Val, n: i64) -> bool {
    matches!(v, Val::Int(x) if x == n)
}

/// Structural equality over the printed image: one spelling per value means the
/// text is the canonical form, so comparing text is comparing values.
fn same(k: &Kernel, a: Val, b: Val) -> bool {
    let mut ba = print::Buf::new();
    let mut bb = print::Buf::new();
    let sa = match print::to_buf(&k.heap, a, &mut ba) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let sb = match print::to_buf(&k.heap, b, &mut bb) {
        Ok(s) => s,
        Err(_) => return false,
    };
    sa == sb
}

struct Checker {
    failed: u32,
}

impl Checker {
    fn check(&mut self, name: &str, ok: bool) {
        serial::write_str(if ok { "  PASS " } else { "  FAIL " });
        serial::write_str(name);
        serial::write_str("\n");
        if !ok {
            self.failed += 1;
        }
    }
}

fn checks(k: &Kernel) -> u32 {
    let mut c = Checker { failed: 0 };

    serial::write_str("\n[1] the image is text, and text is the image\n");
    let mut src = read::Reader::new(BOOT_IMAGE);
    let forms = match src.all(&k.heap) {
        Ok(f) => f,
        Err(e) => {
            serial::write_str("  reader failed: ");
            serial::write_str(e);
            serial::write_str("\n");
            return 1;
        }
    };
    let nforms = k.heap.list_len(forms);
    serial::write_str("  forms in the boot image: ");
    serial::write_num(nforms as i64);
    serial::write_str("\n");
    c.check("boot image reads as eight forms", nforms == 8);

    // Print the whole program back and read it again: equality of printed text
    // is equality of form. That is the whole claim of homoiconicity, measured.
    let mut buf = print::Buf::new();
    let printed = match print::to_buf(&k.heap, forms, &mut buf) {
        Ok(s) => s,
        Err(e) => {
            serial::write_str("  printer failed: ");
            serial::write_str(e);
            serial::write_str("\n");
            return c.failed + 1;
        }
    };
    // The heap stores symbol names as &'static str, so the re-read must see a
    // 'static view. One static scratch region, no allocator: that is the deal.
    let n = {
        let scratch: &'static mut [u8; 4096] = unsafe { &mut *core::ptr::addr_of_mut!(ROUNDTRIP) };
        let mut i = 0;
        while i < printed.len() && i < scratch.len() {
            scratch[i] = printed.as_bytes()[i];
            i += 1;
        }
        i
    };
    let rt: &'static [u8; 4096] = unsafe { &*core::ptr::addr_of!(ROUNDTRIP) };
    let printed_static: &'static str = unsafe { core::str::from_utf8_unchecked(&rt[..n]) };
    let mut again = read::Reader::new(printed_static);
    let reparsed = again.all(&k.heap).unwrap_or(Val::Int(-1));
    let reparsed_one = k.heap.car(reparsed).unwrap_or(NIL);
    c.check("print -> read yields the same form", same(k, forms, reparsed_one));

    serial::write_str("\n[2] the language runs: recursion, closures, let\n");
    let booted = eval_str(k, BOOT_IMAGE);
    c.check("the boot image evaluates without error", booted.is_ok());
    if let Err(e) = booted {
        serial::write_str("  error: ");
        serial::write_str(e);
        serial::write_str("\n");
    }
    c.check("(fact 10) = 3628800", matches!(eval_str(k, "(fact 10)"), Ok(v) if is_int(v, 3628800)));
    c.check("(fib 15) = 610", matches!(eval_str(k, "(fib 15)"), Ok(v) if is_int(v, 610)));
    c.check("(sq 7) = 49 (closure)", matches!(eval_str(k, "(sq 7)"), Ok(v) if is_int(v, 49)));
    c.check(
        "(let ((a 2) (b 3)) (+ a b)) = 5",
        matches!(eval_str(k, "(let ((a 2) (b 3)) (+ a b))"), Ok(v) if is_int(v, 5)),
    );
    c.check("'plant stays a symbol", matches!(eval_str(k, "'plant"), Ok(Val::Sym(_))));

    serial::write_str("\n[3] the same forms, compiled: bytecode runs the boot image\n");
    let mut cur = forms;
    let mut compiled_err: &'static str = "";
    while let Val::Cons(_) = cur {
        let form = match k.heap.car(cur) {
            Ok(f) => f,
            Err(e) => {
                compiled_err = e;
                break;
            }
        };
        match bytecode::compile(k, form) {
            Ok(ci) => match bytecode::run(k, ci, NIL) {
                Ok(_) => {}
                Err(e) => {
                    compiled_err = e;
                    break;
                }
            },
            Err(e) => {
                compiled_err = e;
                break;
            }
        }
        cur = match k.heap.cdr(cur) {
            Ok(v) => v,
            Err(e) => {
                compiled_err = e;
                break;
            }
        };
    }
    c.check("compiled boot image runs without error", compiled_err.is_empty());
    if !compiled_err.is_empty() {
        serial::write_str("  error: ");
        serial::write_str(compiled_err);
        serial::write_str("\n");
    }
    c.check("compiled (fact 10) = 3628800", compiled_int(k, "(fact 10)", 3628800));
    c.check("compiled (fib 15) = 610", compiled_int(k, "(fib 15)", 610));
    c.check("compiled (sq 7) = 49", compiled_int(k, "(sq 7)", 49));

    serial::write_str("\n[4] state is a form: commit, audit, rollback\n");
    let base = eval_str(k, "base").unwrap_or(NIL);
    let g1 = k.img.commit(base).unwrap_or(0);
    let seven = match k.heap.cons(Val::Int(7), NIL) {
        Ok(v) => v,
        Err(_) => NIL,
    };
    let g2 = k.img.commit(seven).unwrap_or(0);
    c.check("two generations published", g1 > 0 && g2 > g1);
    c.check("root is the newest form", same(k, k.img.root(), seven));
    let g3 = k.img.rollback(g1).unwrap_or(0);
    c.check("rollback republishes the old root", g3 > g2 && same(k, k.img.root(), base));

    serial::write_str("\n[5] authority is a capability, not a number\n");
    let cap = k.img.mint().unwrap_or(0);
    c.check("the image mints a handle", cap != 0);
    let open_sym = sym(k, "open");
    let cap_form = list2(k, open_sym, Val::Cap(cap));
    let opened = eval::eval(k, NIL, cap_form);
    c.check("a held handle opens the root", opened.is_ok());
    let forged_form = list2(k, open_sym, Val::Cap(cap ^ 0x5555));
    let forged = eval::eval(k, NIL, forged_form);
    c.check("a forged handle is refused", forged.is_err());

    serial::write_str("\n[6] speed: tree-walk against bytecode, same forms\n");
    let fib_sym = sym(k, "fib");
    let fib_form = list2(k, fib_sym, Val::Int(12));
    let mark = k.heap.mark();

    // control: one evaluation with no rewind at all. If this differs from the
    // loop below by two orders of magnitude, the loop is measuring the arena,
    // not the interpreter - and that is worth knowing before trusting a number.
    let tc0 = serial::ticks();
    let single = eval::eval(k, NIL, fib_form);
    let tc1 = serial::ticks().saturating_sub(tc0);
    serial::write_str("  control: one (fib 12) without rewind: ");
    serial::write_num((tc1 / 10) as i64);
    serial::write_str(" us (");
    serial::write_str(if matches!(single, Ok(v) if is_int(v, 144)) { "correct" } else { "WRONG" });
    serial::write_str(")\n");

    // tree-walk
    let t0 = serial::ticks();
    let mut acc = NIL;
    let mut iters = 0u64;
    while iters < 50 {
        acc = match eval::eval(k, NIL, fib_form) {
            Ok(v) => v,
            Err(_) => Val::Int(-1),
        };
        k.heap.rewind(mark);
        iters += 1;
    }
    let tw_fib = serial::ticks().saturating_sub(t0);
    c.check("tree-walk (fib 12) = 144", is_int(acc, 144));

    // bytecode
    let fib_ci = bytecode::compile(k, fib_form).unwrap_or(0);
    let mark2 = k.heap.mark();
    let t1 = serial::ticks();
    let mut acc2 = NIL;
    let mut iters2 = 0u64;
    while iters2 < 50 {
        acc2 = match bytecode::run(k, fib_ci, NIL) {
            Ok(v) => v,
            Err(_) => Val::Int(-1),
        };
        k.heap.rewind(mark2);
        iters2 += 1;
    }
    let bc_fib = serial::ticks().saturating_sub(t1);
    c.check("bytecode (fib 12) = 144", is_int(acc2, 144));

    // the same for a one-form workload: what a form costs
    let sum_sym = sym(k, "+");
    let sum_form = list2(k, sum_sym, Val::Int(1));
    let one_two = list2(k, sum_form, Val::Int(2));
    let m3 = k.heap.mark();
    let t2 = serial::ticks();
    let mut n3 = 0u64;
    while n3 < 20_000 {
        let _ = eval::eval(k, NIL, one_two);
        k.heap.rewind(m3);
        n3 += 1;
    }
    let tw_sum = serial::ticks().saturating_sub(t2);
    let sum_ci = bytecode::compile(k, one_two).unwrap_or(0);
    let m4 = k.heap.mark();
    let t3 = serial::ticks();
    let mut n4 = 0u64;
    while n4 < 20_000 {
        let _ = bytecode::run(k, sum_ci, NIL);
        k.heap.rewind(m4);
        n4 += 1;
    }
    let bc_sum = serial::ticks().saturating_sub(t3);

    let calls: u64 = 50 * 465;
    serial::write_str("  (fib 12) x50 = 23k calls\n");
    serial::write_str("    tree-walk : ");
    serial::write_num((tw_fib / 10_000) as i64);
    serial::write_str(" ms, ");
    if tw_fib > 0 {
        serial::write_num((calls * 10_000_000 / tw_fib) as i64);
    }
    serial::write_str(" calls/s\n");
    serial::write_str("    bytecode  : ");
    serial::write_num((bc_fib / 10_000) as i64);
    serial::write_str(" ms, ");
    if bc_fib > 0 {
        serial::write_num((calls * 10_000_000 / bc_fib) as i64);
    }
    serial::write_str(" calls/s\n");

    serial::write_str("  (+ 1 2) x20000\n");
    serial::write_str("    tree-walk : ");
    if tw_sum > 0 {
        serial::write_num((20_000u64 * 10_000_000 / tw_sum) as i64);
    }
    serial::write_str(" forms/s\n");
    serial::write_str("    bytecode  : ");
    if bc_sum > 0 {
        serial::write_num((20_000u64 * 10_000_000 / bc_sum) as i64);
    }
    serial::write_str(" forms/s\n");
    if bc_sum > 0 && tw_sum > 0 {
        serial::write_str("  ratio: ");
        serial::write_num(((tw_sum as u64 * 100) / bc_sum as u64) as i64);
        serial::write_str("% of the tree-walk time\n");
    }
    // Honest criterion: the compiler must win where the work is calls, and
    // must not collapse on a one-form workload (where dispatch dominates).
    c.check("bytecode is faster on call-heavy code", bc_fib < tw_fib);
    c.check("bytecode stays within 20% on a one-form workload", bc_sum <= tw_sum + tw_sum / 5);

    serial::write_str("\n[7] the image on disk: generations survive a reboot\n");
    if !k.blk.ok {
        serial::write_str("  SKIP no block device (add: -drive file=disk.img,format=raw,if=none,id=d0 \\\n");
        serial::write_str("       -device virtio-blk-device,drive=d0)\n");
    } else {
        serial::write_str("  disk capacity: ");
        serial::write_num(k.blk.capacity as i64);
        serial::write_str(" sectors\n");
        match store::save(k) {
            Ok(bytes) => {
                c.check("the root form is written to the disk", bytes > 0);
                let before = k.img.generation();
                match store::load(k) {
                    Ok(Some(_gen)) => {
                        c.check("the image reads back and parses", true);
                        c.check(
                            "what came back is what was saved",
                            same(k, k.img.root(), k.img.gen_at(k.img.count() - 2).root),
                        );
                        let _ = before;
                    }
                    Ok(None) => c.check("the image reads back and parses", false),
                    Err(e) => {
                        serial::write_str("  load error: ");
                        serial::write_str(e);
                        serial::write_str("\n");
                        c.check("the image reads back and parses", false);
                    }
                }
            }
            Err(e) => {
                serial::write_str("  save error: ");
                serial::write_str(e);
                serial::write_str("\n");
                c.check("the root form is written to the disk", false);
            }
        }
    }

    serial::write_str("\n[8] deadlines: the scheduler picks by promise, not by turn\n");
    let spawned = eval_str(
        k,
        r#"(spawn "edge" 4 (lambda () (fib 9)))
(spawn "tight" 12 (lambda () (fib 17)))
(spawn "loose" 300 (lambda () (fib 14)))"#,
    );
    c.check("three tasks spawned, each with its own stack", spawned.is_ok() && sched::sched().n == 3);
    let ran = eval_str(k, "(sched-run)");
    c.check("the preemptive run finished without error", ran.is_ok() && !sched::sched().failed);
    c.check("no task stack ran into the scheduler (.bss canary intact)", sched::stacks_intact());
    let s = sched::sched();
    c.check("the timer fired and the tasks were interrupted", s.ticks > 0);
    c.check(
        "all three ran to completion",
        s.tasks[0].finished && s.tasks[1].finished && s.tasks[2].finished,
    );
    c.check(
        "the earliest deadline ran first, then the next, then the furthest",
        s.n_started == 3 && s.start_order[0] == 0 && s.start_order[1] == 1 && s.start_order[2] == 2,
    );
    c.check(
        "the tight task was preempted more than once",
        s.tasks[1].slices > 1 && s.preemptions > 1,
    );
    c.check(
        "a task that can no longer make its deadline yields to one that can",
        s.n_finished == 3 && s.finish_order[2] == 1 && s.finish_order[1] == 2,
    );
    c.check(
        "one broken promise, counted and named",
        s.misses == 1 && s.tasks[1].missed && !s.tasks[0].missed && !s.tasks[2].missed,
    );
    let mut i = 0;
    while i < s.n {
        let t = s.tasks[i];
        serial::write_str("  ");
        serial::write_str(t.name());
        serial::write_str(": promised ");
        serial::write_num(t.deadline_ms as i64);
        serial::write_str(" ms, took ");
        serial::write_num(t.took_ms as i64);
        serial::write_str(" ms in ");
        serial::write_num(t.slices as i64);
        serial::write_str(" slices -> ");
        serial::write_str(if t.missed { "MISSED" } else { "kept" });
        serial::write_str("\n");
        i += 1;
    }
    serial::write_str("  order started: ");
    let mut j = 0;
    while j < s.n_started {
        serial::write_str(s.tasks[s.start_order[j] as usize].name());
        if j + 1 < s.n_started {
            serial::write_str(" -> ");
        }
        j += 1;
    }
    serial::write_str("\n  order finished: ");
    j = 0;
    while j < s.n_finished {
        serial::write_str(s.tasks[s.finish_order[j] as usize].name());
        if j + 1 < s.n_finished {
            serial::write_str(" -> ");
        }
        j += 1;
    }
    serial::write_str("\n  timer ticks: ");
    serial::write_num(s.ticks as i64);
    serial::write_str(", context switches: ");
    serial::write_num(s.preemptions as i64);
    serial::write_str(", decisions by deadline: ");
    serial::write_num(s.by_deadline as i64);
    serial::write_str("\n");

    serial::write_str("\n[9] what the machine is made of\n");
    serial::write_str("  generations ");
    serial::write_num(k.img.count() as i64);
    serial::write_str(" (root ");
    serial::write_num(k.img.generation() as i64);
    serial::write_str(", parent index ");
    serial::write_num(k.img.gen_at(k.img.count() - 1).parent);
    serial::write_str("), capabilities ");
    serial::write_num(k.img.caps_count() as i64);
    serial::write_str("\n  cons cells ");
    serial::write_num(k.heap.ncells() as i64);
    serial::write_str(", symbols ");
    serial::write_num(k.heap.nsyms() as i64);
    serial::write_str(", closures ");
    serial::write_num(k.heap.nclosures() as i64);
    serial::write_str(", code blocks ");
    serial::write_num(k.ncodes() as i64);
    serial::write_str(", constants ");
    serial::write_num(k.nconsts() as i64);
    serial::write_str("\n");

    c.failed
}

/// Compile a source string and run it on the VM; compare with the expected int.
fn compiled_int(k: &Kernel, src: &'static str, want: i64) -> bool {
    let mut r = read::Reader::new(src);
    let forms = match r.all(&k.heap) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let form = match k.heap.car(forms) {
        Ok(f) => f,
        Err(_) => return false,
    };
    match bytecode::compile(k, form) {
        Ok(ci) => matches!(bytecode::run(k, ci, NIL), Ok(v) if is_int(v, want)),
        Err(_) => false,
    }
}

fn list2(k: &Kernel, a: Val, b: Val) -> Val {
    match k.heap.cons(b, NIL) {
        Ok(tail) => match k.heap.cons(a, tail) {
            Ok(v) => v,
            Err(_) => NIL,
        },
        Err(_) => NIL,
    }
}

const REPL_BUF: usize = 512;

/// The shell is not a separate program: it reads a line, reads forms out of it,
/// evaluates them, prints the form back. Same reader, same printer, same
/// evaluator as the boot image - that is what "the language is the interface"
/// means in practice.
fn repl(k: &Kernel) -> ! {
    static mut SOURCE: [u8; REPL_BUF] = [0; REPL_BUF];
    // The input line lives on the stack; the heap's symbols are &'static str, so
    // the text handed to the reader lives in one static region, written only
    // between evaluations.
    let mut line = [0u8; REPL_BUF];
    let mut len = 0usize;

    serial::write_str("\neikon repl. type forms; (quit) leaves.\n> ");
    loop {
        match serial::try_read() {
            Some(b'\r') | Some(b'\n') => {
                serial::write_str("\n");
                if len > 0 {
                    let src: &'static mut [u8; REPL_BUF] =
                        unsafe { &mut *core::ptr::addr_of_mut!(SOURCE) };
                    let mut i = 0;
                    while i < len {
                        src[i] = line[i];
                        i += 1;
                    }
                    let text: &'static str = unsafe { core::str::from_utf8_unchecked(&src[..len]) };
                    if text.trim() == "(quit)" {
                        serial::write_str("bye. root is generation ");
                        serial::write_num(k.img.generation() as i64);
                        serial::write_str("\n");
                        loop {
                            core::hint::spin_loop();
                        }
                    }
                    match eval_str(k, text) {
                        Ok(v) => {
                            serial::write_str("=> ");
                            let mut buf = print::Buf::new();
                            match print::to_buf(&k.heap, v, &mut buf) {
                                Ok(t) => serial::write_str(t),
                                Err(e) => serial::write_str(e),
                            }
                            serial::write_str("\n");
                        }
                        Err(e) => {
                            serial::write_str("! ");
                            serial::write_str(e);
                            serial::write_str("\n");
                        }
                    }
                    len = 0;
                }
                serial::write_str("> ");
            }
            Some(b'\x08') | Some(b'\x7f') => {
                if len > 0 {
                    len -= 1;
                    serial::write_str("\u{8} \u{8}");
                }
            }
            Some(b) => {
                if len < REPL_BUF - 1 {
                    line[len] = b;
                    len += 1;
                    serial::write_byte(b); // echo
                }
            }
            // Busy poll on purpose: `wfi` with no enabled interrupt parks the
            // hart forever, and the REPL would never wake up to read again.
            None => core::hint::spin_loop(),
        }
    }
}

#[no_mangle]
pub extern "C" fn kernel_main(hartid: usize, _dtb: usize) -> ! {
    serial::init();
    serial::write_str("\n");
    serial::write_str("eikon 0.1.0 - a homoiconic kernel\n");
    serial::write_str("the image is the program; the program is the image\n");
    serial::write_str("hart ");
    serial::write_num(hartid as i64);
    serial::write_str(", riscv64, no mmu, no allocator, no crates\n");

    let k = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL) };
    // fixed seed on purpose: the same boot must produce the same image
    k.img.set_secret(0x5eed_1234_abcd_9876 ^ (hartid as u64));

    k.set_blk(virtio::Blk::init());
    if k.blk.ok {
        serial::write_str("virtio-blk: ");
        serial::write_num(k.blk.capacity as i64);
        serial::write_str(" sectors\n");
        match store::load(k) {
            Ok(Some(gen)) => {
                serial::write_str("image restored from disk: generation ");
                serial::write_num(gen as i64);
                serial::write_str("\n");
            }
            Ok(None) => serial::write_str("disk has no image yet\n"),
            Err(e) => {
                serial::write_str("disk image unreadable: ");
                serial::write_str(e);
                serial::write_str("\n");
            }
        }
    } else {
        serial::write_str("virtio-blk: not present (booting without a disk)\n");
        virtio::Blk::probe();
    }

    serial::write_str("\n=== boot self-test ===\n");
    let failed = checks(k);

    serial::write_str("\n=== ");
    serial::write_str(if failed == 0 { "PASS" } else { "FAIL" });
    serial::write_str(" (failures: ");
    serial::write_num(failed as i64);
    serial::write_str(") ===\n\n");
    serial::write_str("root is generation ");
    serial::write_num(k.img.generation() as i64);
    serial::write_str("\n");
    repl(k)
}
