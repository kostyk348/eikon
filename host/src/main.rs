// eikon-host: the same core, compiled for a normal operating system.
//
// Why this exists: every serious bug in this kernel so far was found by a long,
// slow, badly-instrumented loop (QEMU + serial + patience). A hosted build puts
// the parser, the evaluator, the compiler and the image model under a debugger,
// a sanitiser and a fuzzer, where 100 000 cases take seconds instead of hours.
//
// What it is not: the kernel. Traps, stacks, the timer, the disk and the guest
// live behind shims that answer honestly ("no scheduler on the host"), because a
// fake them would hide exactly the bugs this build is meant to catch.

#[path = "../../src/val.rs"]
mod val;
#[path = "../../src/read.rs"]
mod read;
#[path = "../../src/print.rs"]
mod print;
#[path = "../../src/eval.rs"]
mod eval;
#[path = "../../src/bytecode.rs"]
mod bytecode;
#[path = "../../src/image.rs"]
mod image;

mod sched;
mod serial;
mod store;
mod trap;
mod virtio;

use image::Kernel;
use val::{Val, NIL};

static mut KERNEL: Kernel = Kernel::new();

#[allow(static_mut_refs)]
fn kernel() -> &'static mut Kernel {
    unsafe { &mut *core::ptr::addr_of_mut!(KERNEL) }
}

const IMAGE: &str = r#"
(def (fact n) (if (< n 2) 1 (* n (fact (- n 1)))))
(def (fib n)  (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
(def sq (lambda (x) (* x x)))
(def base (list 'plant 200 1))
(def (sum-to n) (if (< n 1) 0 (+ n (sum-to (- n 1)))))
"#;

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

/// Structure, not identity: `Val::Cons` is an index into the arena, so two
/// structurally equal forms allocated at different cells are not `==`.
fn same(k: &Kernel, a: Val, b: Val) -> bool {
    match (a, b) {
        (Val::Cons(_), Val::Cons(_)) => {
            let (ha, ta) = (k.heap.car(a), k.heap.cdr(a));
            let (hb, tb) = (k.heap.car(b), k.heap.cdr(b));
            match (ha, ta, hb, tb) {
                (Ok(ha), Ok(ta), Ok(hb), Ok(tb)) => same(k, ha, hb) && same(k, ta, tb),
                _ => false,
            }
        }
        _ => a == b,
    }
}

fn render(k: &Kernel, v: Val) -> String {
    let mut buf = print::Buf::new();
    match print::to_buf(&k.heap, v, &mut buf) {
        Ok(s) => s.to_string(),
        Err(e) => e.to_string(),
    }
}

fn selftest() -> i32 {
    let k = kernel();
    let mut failed = 0;
    let mut check = |name: &str, ok: bool| {
        println!("  {} {}", if ok { "PASS" } else { "FAIL" }, name);
        if !ok {
            failed += 1;
        }
    };

    println!("eikon-host: the core on a normal operating system\n");

    // [1] the image is text, and text is the image
    let forms = {
        let mut r = read::Reader::new(IMAGE);
        r.all(&k.heap)
    };
    check("the image reads", forms.is_ok());
    // and the whole image is evaluated, the way the kernel does at boot
    let booted = eval_str(k, IMAGE);
    check("the image evaluates", booted.is_ok());
    let last = eval_str(k, "(def sq (lambda (x) (* x x)))").is_ok();
    check("a form evaluates and defines", last);
    let printed = eval_str(k, "(print 'test)").is_ok();
    check("print is a value going out", printed);

    // [2] the language
    let fact = eval_str(k, "(fact 10)");
    check(
        "(fact 10) = 3628800",
        matches!(fact, Ok(Val::Int(3628800))),
    );
    let fib = eval_str(k, "(fib 15)");
    check("(fib 15) = 610", matches!(fib, Ok(Val::Int(610))));
    let sq = eval_str(k, "(sq 7)");
    check("(sq 7) = 49 through a closure", matches!(sq, Ok(Val::Int(49))));

    // [3] the compiler agrees with the tree-walker
    let mut c1 = 0;
    let mut c2 = 0;
    let mut agree = true;
    for src in ["(fact 9)", "(fib 12)", "(sum-to 100)", "(sq 12)", "(+ (* 3 4) (- 10 2))"] {
        let a = eval_str(k, src);
        let b = {
            let mut r = read::Reader::new(src);
            match r.all(&k.heap).and_then(|f| k.heap.car(f)) {
                Ok(form) => bytecode::compile(k, form).and_then(|ci| bytecode::run(k, ci, NIL)),
                Err(e) => Err(e),
            }
        };
        match (a, b) {
            (Ok(x), Ok(y)) if same(k, x, y) => {}
            _ => agree = false,
        }
        c1 += 1;
        c2 += 1;
    }
    check("bytecode and tree-walk agree on every form", agree && c1 == c2 && c1 > 0);

    // [4] state is a form: commit, audit, rollback
    let root0 = k.img.root();
    let g1 = k.img.commit(Val::Int(1)).unwrap_or(0);
    let g2 = k.img.commit(Val::Int(2)).unwrap_or(0);
    let back = k.img.rollback(g1);
    check(
        "rollback republishes an old root as a new generation",
        back.is_ok() && k.img.generation() > g2 && root0 != k.img.root(),
    );

    // [5] authority is a capability, not a number
    let minted = k.img.mint().unwrap_or(0);
    check("a minted handle is accepted", k.img.holds(minted));
    check("a forged handle is refused", !k.img.holds(minted ^ 0xdead_beef));

    // the printer and the reader must agree on what a value is
    let v = eval_str(k, "(list 1 'two \"three\" (list 4 5))").unwrap_or(NIL);
    let text = render(k, v);
    let again = {
        let leaked: &'static str = Box::leak(text.clone().into_boxed_str());
        let mut r = read::Reader::new(leaked);
        r.all(&k.heap).map(|forms| k.heap.car(forms))
    };
    let roundtrip = matches!(&again, Ok(Ok(x)) if same(k, *x, v));
    check("print -> read yields the same form", roundtrip);
    if !roundtrip {
        println!("    printed: {:?}", text);
        if let Ok(Ok(x)) = &again {
            println!("    re-read: {:?}", render(k, *x));
        }
    }

    println!(
        "\n=== {} ({} failures) ===",
        if failed == 0 { "PASS" } else { "FAIL" },
        failed
    );
    failed as i32
}

// ---- fuzzing ------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const ATOMS: &[&str] = &[
    "(", ")", " ", "'", "1", "-7", "0", "sym", "\"str\"", "nil", "lambda", "def", "if", "#t",
    "\n", "\\", "|", "1234567890123456789", "(", ")", "(", ")",
];

fn fuzz_parser(iterations: usize) -> i32 {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let k = kernel();
    let mut panics = 0;
    let mut i = 0;
    while i < iterations {
        let len = 1 + rng.below(24);
        let mut s = String::new();
        let mut j = 0;
        while j < len {
            s.push_str(ATOMS[rng.below(ATOMS.len())]);
            j += 1;
        }
        let src: &'static str = Box::leak(s.clone().into_boxed_str());
        let heap_before = k.heap.ncells();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut r = read::Reader::new(src);
            let _ = r.all(&k.heap);
        }));
        if res.is_err() {
            println!("PANIC while parsing: {:?}", s);
            panics += 1;
            if panics > 3 {
                break;
            }
        }
        if k.heap.ncells() < heap_before {
            println!("the parser moved the arena backwards on: {:?}", s);
            panics += 1;
        }
        i += 1;
    }
    println!(
        "parser fuzz: {} inputs, {} panics",
        iterations, panics
    );
    panics as i32
}

/// Random *valid* forms, then the invariant that makes this language what it is:
/// print(read(x)) is read(x), and reading that again changes nothing.
fn fuzz_roundtrip(iterations: usize) -> i32 {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let k = kernel();
    let mut bad = 0;
    let mut i = 0;
    while i < iterations {
        let depth = rng.below(4);
        let src = gen_form(&mut rng, depth);
        let leaked: &'static str = Box::leak(src.into_boxed_str());
        let once = {
            let mut r = read::Reader::new(leaked);
            r.all(&k.heap).map(|f| k.heap.car(f))
        };
        if let Ok(Ok(v)) = once {
            let text = render(k, v);
            let text2: &'static str = Box::leak(text.clone().into_boxed_str());
            let twice = {
                let mut r = read::Reader::new(text2);
                r.all(&k.heap).map(|f| k.heap.car(f))
            };
            match twice {
                Ok(Ok(w)) if same(k, w, v) => {}
                _ => {
                    if bad < 3 {
                        println!("roundtrip broke: {:?} -> {:?}", leaked, text);
                    }
                    bad += 1;
                }
            }
        }
        i += 1;
    }
    println!("roundtrip fuzz: {} forms, {} mismatches", iterations, bad);
    bad as i32
}

fn gen_form(rng: &mut Rng, depth: usize) -> String {
    let pick = rng.below(if depth == 0 { 4 } else { 7 });
    match pick {
        0 => format!("{}", rng.below(1000) as i64 - 500),
        1 => "sym".to_string(),
        2 => "\"text\"".to_string(),
        3 => "nil".to_string(),
        4 | 5 => {
            let n = rng.below(3) + 1;
            let mut s = String::from("(");
            let mut i = 0;
            while i < n {
                s.push_str(&gen_form(rng, depth - 1));
                if i + 1 < n {
                    s.push(' ');
                }
                i += 1;
            }
            s.push(')');
            s
        }
        _ => {
            let n = rng.below(3) + 1;
            let mut s = String::from("(+");
            let mut i = 0;
            while i < n {
                s.push(' ');
                s.push_str(&gen_form(rng, depth - 1));
                i += 1;
            }
            s.push(')');
            s
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("selftest");
    let code = match mode {
        "fuzz" => {
            let n = args
                .get(2)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(20_000);
            let a = fuzz_parser(n);
            let b = fuzz_roundtrip(n);
            a + b
        }
        _ => selftest(),
    };
    std::process::exit(if code == 0 { 0 } else { 1 });
}
