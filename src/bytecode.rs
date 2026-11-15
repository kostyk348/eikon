// eikon: a compiler from forms to bytecode, and a stack VM to run it.
//
// Why: the tree-walker in eval.rs dispatches on the shape of a form at every
// step, and every argument is boxed into a fixed array. A form is data, so it
// can be compiled once into a linear instruction stream - and the interface
// stays exactly the same, because the compiler eats the same forms the reader
// produced. A program cannot tell which engine ran it; the benchmark can.
//
// No registers, no JIT, no peephole: a stack machine is the smallest thing that
// is honestly faster than walking a tree, and it is easy to keep correct.

use crate::image::Kernel;
use crate::val::{Val, NIL};

pub const MAX_OPS: usize = 256;
pub const MAX_CODES: usize = 64;
pub const MAX_CONSTS: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Const(u16),   // push consts[i]
    Load(u16),    // push value of the symbol consts[i]
    Def(u16),     // pop value, bind symbol consts[i], push it back
    Jump(u16),
    JumpFalse(u16), // pop; jump when false
    Call(u8),     // stack: callee, arg1..argN
    MkClos(u16, u16, u16), // closure over the current env: (code, params const, body const)
    Pop,
    Add,
    Sub,
    Mul,
    Lt,
    Eq,
    Cons,
    Car,
    Cdr,
    Len,
    Return,
}

#[derive(Clone, Copy)]
pub struct Code {
    pub ops: [Op; MAX_OPS],
    pub n: usize,
    pub arity: u8,
}

impl Code {
    pub const fn new() -> Self {
        Code { ops: [Op::Return; MAX_OPS], n: 0, arity: 0 }
    }
}

struct Emitter<'a> {
    k: &'a mut Kernel,
    ci: u16,
}

impl<'a> Emitter<'a> {
    fn push(&mut self, op: Op) -> Result<usize, &'static str> {
        let code = &mut self.k.codes[self.ci as usize];
        if code.n == MAX_OPS {
            return Err("ENOSPC: code block full");
        }
        code.ops[code.n] = op;
        code.n += 1;
        Ok(code.n - 1)
    }

    fn konst(&mut self, v: Val) -> Result<u16, &'static str> {
        if self.k.nconsts == MAX_CONSTS {
            return Err("ENOSPC: constant pool full");
        }
        self.k.consts[self.k.nconsts] = v;
        self.k.nconsts += 1;
        Ok((self.k.nconsts - 1) as u16)
    }

    fn patch(&mut self, at: usize, op: Op) {
        self.k.codes[self.ci as usize].ops[at] = op;
    }

    fn here(&self) -> u16 {
        self.k.codes[self.ci as usize].n as u16
    }

    /// Reserve a new code block, compile `body` into it, return its index.
    fn sub(&mut self, params_len: u8, body: Val) -> Result<u16, &'static str> {
        if self.k.ncodes == MAX_CODES {
            return Err("ENOSPC: code table full");
        }
        let idx = self.k.ncodes as u16;
        self.k.ncodes += 1;
        self.k.codes[idx as usize] = Code { ops: [Op::Return; MAX_OPS], n: 0, arity: params_len };
        let saved = self.ci;
        self.ci = idx;
        let mut body_cur = body;
        loop {
            if !matches!(body_cur, Val::Cons(_)) {
                break;
            }
            let form = self.k.heap.car(body_cur)?;
            let more = self.k.heap.cdr(body_cur)?;
            if matches!(more, Val::Cons(_)) {
                self.comp(form)?;
                self.push(Op::Pop)?;
            } else {
                self.comp(form)?;
                break;
            }
            body_cur = more;
        }
        self.push(Op::Return)?;
        self.ci = saved;
        Ok(idx)
    }

    fn comp(&mut self, form: Val) -> Result<(), &'static str> {
        match form {
            Val::Nil | Val::Int(_) | Val::Str(_) | Val::Cap(_) | Val::Clos(_) => {
                let i = self.konst(form)?;
                self.push(Op::Const(i))?;
                Ok(())
            }
            Val::Sym(_) => {
                let i = self.konst(form)?;
                self.push(Op::Load(i))?;
                Ok(())
            }
            Val::Cons(_) => self.comp_cons(form),
        }
    }

    fn comp_cons(&mut self, form: Val) -> Result<(), &'static str> {
        let head = self.k.heap.car(form)?;
        let rest = self.k.heap.cdr(form)?;
        if let Val::Sym(s) = head {
            match self.k.heap.sym_name(s) {
                "quote" => {
                    let v = self.k.heap.car(rest)?;
                    let i = self.konst(v)?;
                    return self.push(Op::Const(i)).map(|_| ());
                }
                "if" => {
                    self.comp(self.k.heap.car(rest)?)?;
                    let jf = self.push(Op::JumpFalse(0))?;
                    let then = self.k.heap.car(self.k.heap.cdr(rest)?)?;
                    self.comp(then)?;
                    let jend = self.push(Op::Jump(0))?;
                    let else_form = self.k.heap.car(self.k.heap.cdr(self.k.heap.cdr(rest)?)?)?;
                    self.patch(jf, Op::JumpFalse(self.here()));
                    self.comp(else_form)?;
                    self.patch(jend, Op::Jump(self.here()));
                    return Ok(());
                }
                "begin" => {
                    let mut cur = rest;
                    while matches!(cur, Val::Cons(_)) {
                        let f = self.k.heap.car(cur)?;
                        let more = self.k.heap.cdr(cur)?;
                        self.comp(f)?;
                        if matches!(more, Val::Cons(_)) {
                            self.push(Op::Pop)?;
                        }
                        cur = more;
                    }
                    return Ok(());
                }
                "let" => {
                    // (let ((n v) ...) body...) is an application of a closure:
                    // the compiler does not need a second mechanism for scope.
                    let bindings = self.k.heap.car(rest)?;
                    let body = self.k.heap.cdr(rest)?;
                    let mut names: [Val; 8] = [NIL; 8];
                    let mut values = [NIL; 8];
                    let mut n = 0usize;
                    let mut b = bindings;
                    while let Val::Cons(_) = b {
                        let pair = self.k.heap.car(b)?;
                        if n == names.len() {
                            return Err("EINVAL: let too long");
                        }
                        names[n] = self.k.heap.car(pair)?;
                        values[n] = self.k.heap.car(self.k.heap.cdr(pair)?)?;
                        n += 1;
                        b = self.k.heap.cdr(b)?;
                    }
                    let params = self.list(&names[..n])?;
                    let ci = self.sub(n as u8, body)?;
                    let pi = self.konst(params)?;
                    let bi = self.konst(body)?;
                    self.push(Op::MkClos(ci, pi, bi))?;
                    for v in values.iter().take(n) {
                        self.comp(*v)?;
                    }
                    self.push(Op::Call(n as u8))?;
                    return Ok(());
                }
                "lambda" => {
                    let params = self.k.heap.car(rest)?;
                    let body = self.k.heap.cdr(rest)?;
                    let arity = self.k.heap.list_len(params) as u8;
                    let ci = self.sub(arity, body)?;
                    let pi = self.konst(params)?;
                    let bi = self.konst(body)?;
                    self.push(Op::MkClos(ci, pi, bi))?;
                    return Ok(());
                }
                "def" => {
                    let target = self.k.heap.car(rest)?;
                    let body = self.k.heap.cdr(rest)?;
                    match target {
                        Val::Cons(_) => {
                            let name = self.k.heap.car(target)?;
                            let params = self.k.heap.cdr(target)?;
                            let arity = self.k.heap.list_len(params) as u8;
                            let ci = self.sub(arity, body)?;
                            let pi = self.konst(params)?;
                            let bi = self.konst(body)?;
                            self.push(Op::MkClos(ci, pi, bi))?;
                            let ni = self.konst(name)?;
                            self.push(Op::Def(ni))?;
                        }
                        Val::Sym(_) => {
                            let v = self.k.heap.car(body)?;
                            self.comp(v)?;
                            let ni = self.konst(target)?;
                            self.push(Op::Def(ni))?;
                        }
                        _ => return Err("EINVAL: bad def target"),
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
        // application. A primitive is its own value: compiling it as Const
        // keeps the VM from looking up a global that does not exist.
        if let Val::Sym(s) = head {
            let hname = self.k.heap.sym_name(s);
            // A primitive in call position does not need a call at all: emit
            // the instruction. This is the whole "compiler is worth it" claim
            // in five lines, and it is measurable on a one-form workload.
            let arity = self.k.heap.list_len(rest);
            let direct = match (hname, arity) {
                ("+", 2) => Some(Op::Add),
                ("-", 2) => Some(Op::Sub),
                ("*", 2) => Some(Op::Mul),
                ("<", 2) => Some(Op::Lt),
                ("=", 2) => Some(Op::Eq),
                ("cons", 2) => Some(Op::Cons),
                ("car", 1) => Some(Op::Car),
                ("cdr", 1) => Some(Op::Cdr),
                ("len", 1) => Some(Op::Len),
                (_, 1) => None,
                _ => None,
            };
            if let Some(op) = direct {
                let mut a = rest;
                while let Val::Cons(_) = a {
                    let arg = self.k.heap.car(a)?;
                    self.comp(arg)?;
                    a = self.k.heap.cdr(a)?;
                }
                return self.push(op).map(|_| ());
            }
            if callable_prim(hname) {
                let i = self.konst(head)?;
                self.push(Op::Const(i))?;
                let mut n = 0u8;
                let mut a = rest;
                while let Val::Cons(_) = a {
                    let arg = self.k.heap.car(a)?;
                    self.comp(arg)?;
                    n += 1;
                    a = self.k.heap.cdr(a)?;
                }
                return self.push(Op::Call(n)).map(|_| ());
            }
        }
        self.comp(head)?;
        let mut n = 0u8;
        let mut a = rest;
        while let Val::Cons(_) = a {
            let arg = self.k.heap.car(a)?;
            self.comp(arg)?;
            n += 1;
            a = self.k.heap.cdr(a)?;
        }
        self.push(Op::Call(n)).map(|_| ())
    }

    fn list(&mut self, items: &[Val]) -> Result<Val, &'static str> {
        let mut out = NIL;
        let mut i = items.len();
        while i > 0 {
            i -= 1;
            out = self.k.heap.cons(items[i], out)?;
        }
        Ok(out)
    }
}

/// Compile one form into a fresh code block; returns its index.
pub fn compile(k: &mut Kernel, form: Val) -> Result<u16, &'static str> {
    if k.ncodes == MAX_CODES {
        return Err("ENOSPC: code table full");
    }
    let idx = k.ncodes as u16;
    k.ncodes += 1;
    k.codes[idx as usize] = Code::new();
    let mut e = Emitter { k, ci: idx };
    e.comp(form)?;
    e.push(Op::Return)?;
    Ok(idx)
}

/// Run a code block. Calls recurse in Rust: correct first, threaded dispatch
/// later if the benchmark says it matters.
pub fn run(k: &mut Kernel, ci: u16, env: Val) -> Result<Val, &'static str> {
    let mut stack: [Val; 64] = [NIL; 64];
    let mut sp = 0usize;
    let mut ip = 0usize;
    let arity = k.codes[ci as usize].arity as usize;
    let _ = arity;
    loop {
        let op = k.codes[ci as usize].ops[ip];
        ip += 1;
        match op {
            Op::Const(i) => {
                stack[sp] = k.consts[i as usize];
                sp += 1;
            }
            Op::Load(i) => {
                let name = k.consts[i as usize];
                stack[sp] = crate::eval::lookup(k, env, name)?;
                sp += 1;
            }
            Op::Def(i) => {
                let name = k.consts[i as usize];
                let v = stack[sp - 1];
                if let Val::Sym(s) = name {
                    k.heap.set_global(k.heap.sym_name(s), v);
                }
            }
            Op::Jump(to) => ip = to as usize,
            Op::JumpFalse(to) => {
                sp -= 1;
                if !crate::eval::truthy(stack[sp]) {
                    ip = to as usize;
                }
            }
            Op::Call(n) => {
                let n = n as usize;
                let mut args: [Val; 16] = [NIL; 16];
                let mut i = 0;
                while i < n {
                    args[i] = stack[sp - n + i];
                    i += 1;
                }
                sp -= n;
                sp -= 1; // callee
                let callee = stack[sp];
                let v = match callee {
                    Val::Clos(c) => {
                        let (params, _body, closure_env, code) = k.heap.closure(c);
                        let mut e = closure_env;
                        let mut p = params;
                        let mut j = 0;
                        while let Val::Cons(_) = p {
                            let pname = k.heap.car(p)?;
                            let binding = k.heap.cons(pname, args[j])?;
                            e = k.heap.cons(binding, e)?;
                            p = k.heap.cdr(p)?;
                            j += 1;
                        }
                        if j != n {
                            return Err("EINVAL: wrong number of arguments");
                        }
                        if code >= 0 {
                            run(k, code as u16, e)?
                        } else {
                            // an interpreted closure: the VM can still call it
                            crate::eval::apply(k, callee, &args[..n])?
                        }
                    }
                    Val::Sym(_s) => crate::eval::apply(k, callee, &args[..n])?,
                    _ => return Err("EINVAL: not callable"),
                };
                stack[sp] = v;
                sp += 1;
            }
            Op::MkClos(ci2, pi, bi) => {
                // The closure keeps the whole current environment: no free
                // variable analysis, no sugar, just a pointer. It also carries
                // the source forms, so the tree-walker can call it too.
                let params = k.consts[pi as usize];
                let body = k.consts[bi as usize];
                stack[sp] = k.heap.mk_closure(params, body, env, ci2 as i64)?;
                sp += 1;
            }
            Op::Pop => sp -= 1,
            Op::Add | Op::Sub | Op::Mul | Op::Lt | Op::Eq => {
                let b = stack[sp - 1];
                let a = stack[sp - 2];
                sp -= 2;
                let (x, y) = match (a, b) {
                    (Val::Int(x), Val::Int(y)) => (x, y),
                    _ => return Err("EINVAL: integer expected"),
                };
                stack[sp] = match op {
                    Op::Add => Val::Int(x + y),
                    Op::Sub => Val::Int(x - y),
                    Op::Mul => Val::Int(x * y),
                    Op::Lt => Val::Int((x < y) as i64),
                    _ => Val::Int((x == y) as i64),
                };
                sp += 1;
            }
            Op::Cons => {
                let b = stack[sp - 1];
                let a = stack[sp - 2];
                sp -= 2;
                stack[sp] = k.heap.cons(a, b)?;
                sp += 1;
            }
            Op::Car => {
                let v = stack[sp - 1];
                stack[sp - 1] = k.heap.car(v)?;
            }
            Op::Cdr => {
                let v = stack[sp - 1];
                stack[sp - 1] = k.heap.cdr(v)?;
            }
            Op::Len => {
                let v = stack[sp - 1];
                stack[sp - 1] = Val::Int(k.heap.list_len(v) as i64);
            }
            Op::Return => return Ok(stack[sp - 1]),
        }
    }
}

/// Every function that is a value rather than a binding.
pub fn callable_prim(name: &str) -> bool {
    crate::eval::is_prim(name)
        || matches!(name, "commit" | "root" | "gen" | "rollback" | "cap" | "open" | "print" | "audit" | "save" | "load" | "spawn" | "sched-run" | "misses")
}
