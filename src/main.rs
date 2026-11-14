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

mod eval;
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

static mut KERNEL: Kernel = Kernel::new();
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
    if let Some(l) = info.location() {
        serial::write_str(l.file());
        serial::write_str(":");
        serial::write_num(l.line() as i64);
    }
    serial::write_str("\n");
    loop {}
}

fn sym(k: &mut Kernel, s: &'static str) -> Val {
    Val::Sym(k.heap.intern(s).unwrap_or(0))
}

/// Read and evaluate a source string in the global environment.
fn eval_str(k: &mut Kernel, src: &'static str) -> Result<Val, &'static str> {
    let mut r = read::Reader::new(src);
    let mut forms = r.all(&mut k.heap)?;
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
fn same(k: &mut Kernel, a: Val, b: Val) -> bool {
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

fn checks(k: &mut Kernel) -> u32 {
    let mut c = Checker { failed: 0 };

    serial::write_str("\n[1] the image is text, and text is the image\n");
    let mut src = read::Reader::new(BOOT_IMAGE);
    let forms = match src.all(&mut k.heap) {
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
    let reparsed = again.all(&mut k.heap).unwrap_or(Val::Int(-1));
    if !same(k, forms, reparsed) {
        serial::write_str("  printed: ");
        serial::write_str(printed_static);
        serial::write_str("\n  reparsed: ");
        let mut b2 = print::Buf::new();
        match print::to_buf(&k.heap, reparsed, &mut b2) {
            Ok(t) => serial::write_str(t),
            Err(e) => serial::write_str(e),
        }
        serial::write_str("\n");
    }
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

    serial::write_str("\n[3] state is a form: commit, audit, rollback\n");
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

    serial::write_str("\n[4] authority is a capability, not a number\n");
    let cap = k.img.mint().unwrap_or(0);
    c.check("the image mints a handle", cap != 0);
    let open_sym = sym(k, "open");
    let cap_form = list2(k, open_sym, Val::Cap(cap));
    let opened = eval::eval(k, NIL, cap_form);
    c.check("a held handle opens the root", opened.is_ok());
    let forged_form = list2(k, open_sym, Val::Cap(cap ^ 0x5555));
    let forged = eval::eval(k, NIL, forged_form);
    c.check("a forged handle is refused", forged.is_err());

    serial::write_str("\n[5] speed: a form on one hart, no MMU, no cache tricks\n");
    let fib_sym = sym(k, "fib");
    let fib_form = list2(k, fib_sym, Val::Int(12));
    // Everything live was allocated before this mark (the boot image, the
    // globals). What the benchmark allocates below is scratch, so the arena is
    // rewound after every iteration instead of being garbage collected.
    let mark = k.heap.mark();
    let mut peak = 0usize;
    let t0 = serial::ticks();
    let mut iters = 0u64;
    let mut acc = NIL;
    while iters < 50 {
        acc = match eval::eval(k, NIL, fib_form) {
            Ok(v) => v,
            Err(_) => Val::Int(-1),
        };
        let used = k.heap.ncells - mark;
        if used > peak {
            peak = used;
        }
        k.heap.rewind(mark);
        iters += 1;
    }
    let dt = serial::ticks().saturating_sub(t0);
    c.check("(fib 12) = 144, measured 50 times", is_int(acc, 144));
    // fib(12) makes 465 calls, so 200 of them is the workload being timed.
    let calls: u64 = 50 * 465;
    serial::write_str("  ");
    serial::write_num((dt / 100) as i64);
    serial::write_str(" ms for ");
    serial::write_str("23k calls (fib 12 x50)");
    serial::write_str("; ");
    if dt > 0 {
        serial::write_num((calls * 10_000_000 / dt) as i64);
        serial::write_str(" calls/s\n");
    } else {
        serial::write_str("unmeasurable (timer granularity)\n");
    }

    // A second, minimal workload: one form, no recursion, so the number says
    // what a form costs and not what the recursion costs.
    let sum_sym = sym(k, "+");
    let two = list2(k, sum_sym, Val::Int(1));
    let one_two = list2(k, two, Val::Int(2));
    let m2 = k.heap.mark();
    let t1 = serial::ticks();
    let mut n2 = 0u64;
    while n2 < 20_000 {
        let _ = eval::eval(k, NIL, one_two);
        k.heap.rewind(m2);
        n2 += 1;
    }
    let d2 = serial::ticks().saturating_sub(t1);
    serial::write_str("  (+ 1 2) x20000: ");
    if d2 > 0 {
        serial::write_num((20_000u64 * 10_000_000 / d2) as i64);
        serial::write_str(" forms/s\n");
    } else {
        serial::write_str("unmeasurable\n");
    }

    serial::write_str("  peak scratch per evaluation: ");
    serial::write_num(peak as i64);
    serial::write_str(" cons cells, rewound after every iteration\n");

    serial::write_str("\n[6] what the machine is made of\n");
    serial::write_str("  generations ");
    serial::write_num(k.img.n as i64);
    serial::write_str(", capabilities ");
    serial::write_num(k.img.ncaps as i64);
    serial::write_str(", cons cells ");
    serial::write_num(k.heap.ncells as i64);
    serial::write_str(", symbols ");
    serial::write_num(k.heap.nsyms as i64);
    serial::write_str(", closures ");
    serial::write_num(k.heap.nclosures as i64);
    serial::write_str("\n");

    c.failed
}

fn list2(k: &mut Kernel, a: Val, b: Val) -> Val {
    match k.heap.cons(b, NIL) {
        Ok(tail) => match k.heap.cons(a, tail) {
            Ok(v) => v,
            Err(_) => NIL,
        },
        Err(_) => NIL,
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
    k.img.secret = 0x5eed_1234_abcd_9876 ^ (hartid as u64);

    serial::write_str("\n=== boot self-test ===\n");
    let failed = checks(k);

    serial::write_str("\n=== ");
    serial::write_str(if failed == 0 { "PASS" } else { "FAIL" });
    serial::write_str(" (failures: ");
    serial::write_num(failed as i64);
    serial::write_str(") ===\n\n");
    serial::write_str("halted. root is generation ");
    serial::write_num(k.img.generation() as i64);
    serial::write_str("\n");
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}
