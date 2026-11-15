// eikon: the evaluator. A form in, a form out - and nothing else in the system.
//
// There is no compiler stage and no bytecode yet: a form is walked and reduced.
// That is a deliberate first step (see ARCHITECTURE.md, "fast but not yet"):
// the interface is stable, so a compiler can be dropped in behind it without
// changing a single program or a single state image.
//
// Environments are ordinary forms too - a list of (name . value) pairs - so
// there is no second kind of state hiding inside the interpreter.

use crate::image::Kernel;
use crate::print::Buf;
use crate::val::{Val, NIL};

pub fn eval(k: &mut Kernel, env: Val, form: Val) -> Result<Val, &'static str> {
    match form {
        Val::Nil | Val::Int(_) | Val::Str(_) | Val::Cap(_) | Val::Clos(_) => Ok(form),
        Val::Sym(_) => lookup(k, env, form),
        Val::Cons(_) => {
            let head = k.heap.car(form)?;
            if let Val::Sym(s) = head {
                let name = k.heap.sym_name(s);
                let rest = k.heap.cdr(form)?;
                match name {
                    "quote" => return k.heap.car(rest),
                    "if" => {
                        let c = eval(k, env, k.heap.car(rest)?)?;
                        let then = k.heap.car(k.heap.cdr(rest)?)?;
                        // the else branch is a *form*, not the list of remaining
                        // forms: (if c a b) has exactly three parts
                        let els = k.heap.car(k.heap.cdr(k.heap.cdr(rest)?)?)?;
                        return if truthy(c) { eval(k, env, then) } else { eval(k, env, els) };
                    }
                    "begin" => return eval_body(k, env, rest),
                    "let" => {
                        let bindings = k.heap.car(rest)?;
                        let body = k.heap.cdr(rest)?;
                        let mut e = env;
                        let mut b = bindings;
                        while let Val::Cons(_) = b {
                            let pair = k.heap.car(b)?;
                            let bname = k.heap.car(pair)?;
                            let bval = eval(k, env, k.heap.car(k.heap.cdr(pair)?)?)?;
                            let binding = k.heap.cons(bname, bval)?;
                            e = k.heap.cons(binding, e)?;
                            b = k.heap.cdr(b)?;
                        }
                        return eval_body(k, e, body);
                    }
                    "lambda" => {
                        let params = k.heap.car(rest)?;
                        let body = k.heap.cdr(rest)?;
                        return k.heap.mk_closure(params, body, env, -1);
                    }
                    "def" => {
                        let target = k.heap.car(rest)?;
                        let body = k.heap.cdr(rest)?;
                        return match target {
                            // (def (name args...) body...) - a function is a binding
                            // to a closure, and a closure is a form.
                            Val::Cons(_) => {
                                let fname = k.heap.car(target)?;
                                let params = k.heap.cdr(target)?;
                                let clo = k.heap.mk_closure(params, body, env, -1)?;
                                let sym = match fname {
                                    Val::Sym(s) => s,
                                    _ => return Err("EINVAL: function name must be a symbol"),
                                };
                                k.heap.set_global(k.heap.sym_name(sym), clo);
                                Ok(clo)
                            }
                            // (def name value)
                            Val::Sym(s) => {
                                let v = eval(k, env, k.heap.car(body)?)?;
                                k.heap.set_global(k.heap.sym_name(s), v);
                                Ok(v)
                            }
                            _ => Err("EINVAL: bad def target"),
                        };
                    }
                    "print" => {
                        let v = eval(k, env, k.heap.car(rest)?)?;
                        let mut buf = Buf::new();
                        let text = crate::print::to_buf(&k.heap, v, &mut buf)?;
                        crate::serial::write_str(text);
                        crate::serial::write_str("\n");
                        return Ok(v);
                    }
                    // ---- the image is part of the language -------------------
                    "commit" => {
                        let v = eval(k, env, k.heap.car(rest)?)?;
                        return Ok(Val::Int(k.img.commit(v)? as i64));
                    }
                    "audit" => {
                        // History *is* the parent chain; asking for it is walking
                        // the generations that were published.
                        let mut out = NIL;
                        let mut i = 0;
                        while i < k.img.n {
                            let id = k.img.gens[i].id as i64;
                            out = k.heap.cons(Val::Int(id), out)?;
                            i += 1;
                        }
                        return Ok(out);
                    }
                    "root" => return Ok(k.img.root()),
                    "gen" => return Ok(Val::Int(k.img.generation() as i64)),
                    "rollback" => {
                        let id = eval(k, env, k.heap.car(rest)?)?;
                        return match id {
                            Val::Int(n) => k.img.rollback(n as u64).map(|g| Val::Int(g as i64)),
                            _ => Err("EINVAL: rollback needs a generation number"),
                        };
                    }
                    "cap" => return Ok(Val::Cap(k.img.mint()?)),
                    "open" => {
                        let h = eval(k, env, k.heap.car(rest)?)?;
                        return match h {
                            Val::Cap(id) if k.img.holds(id) => Ok(k.img.root()),
                            Val::Cap(_) => Err("EPERM: no such capability"),
                            _ => Err("EINVAL: open needs a capability"),
                        };
                    }
                    _ => {}
                }
            }
            // ordinary application. A primitive is a value of its own: the
            // symbol *is* the function, so it is applied without a lookup.
            if let Val::Sym(s) = head {
                if is_prim(k.heap.sym_name(s)) {
                    let mut args: [Val; 16] = [NIL; 16];
                    let mut n = 0usize;
                    let mut a = k.heap.cdr(form)?;
                    while let Val::Cons(_) = a {
                        if n == args.len() {
                            return Err("EINVAL: too many arguments");
                        }
                        args[n] = eval(k, env, k.heap.car(a)?)?;
                        n += 1;
                        a = k.heap.cdr(a)?;
                    }
                    return apply(k, head, &args[..n]);
                }
            }
            let f = eval(k, env, head)?;
            let mut args: [Val; 16] = [NIL; 16];
            let mut n = 0usize;
            let mut a = k.heap.cdr(form)?;
            while let Val::Cons(_) = a {
                if n == args.len() {
                    return Err("EINVAL: too many arguments");
                }
                args[n] = eval(k, env, k.heap.car(a)?)?;
                n += 1;
                a = k.heap.cdr(a)?;
            }
            apply(k, f, &args[..n])
        }
    }
}

fn eval_body(k: &mut Kernel, env: Val, mut body: Val) -> Result<Val, &'static str> {
    let mut last = NIL;
    while let Val::Cons(_) = body {
        last = eval(k, env, k.heap.car(body)?)?;
        body = k.heap.cdr(body)?;
    }
    Ok(last)
}

pub fn is_prim(name: &str) -> bool {
    matches!(name, "+" | "-" | "*" | "<" | "=" | "cons" | "car" | "cdr" | "list" | "len")
}

pub fn truthy(v: Val) -> bool {
    !matches!(v, Val::Nil) && !matches!(v, Val::Int(0))
}

pub fn lookup(k: &Kernel, mut env: Val, sym: Val) -> Result<Val, &'static str> {
    let name = match sym {
        Val::Sym(s) => k.heap.sym_name(s),
        _ => return Err("EINVAL: lookup of a non-symbol"),
    };
    while let Val::Cons(i) = env {
        let (binding, rest) = k.heap.cells[i as usize];
        if let Val::Cons(j) = binding {
            let (key, val) = k.heap.cells[j as usize];
            if let Val::Sym(s) = key {
                if k.heap.sym_name(s) == name {
                    return Ok(val);
                }
            }
        }
        env = rest;
    }
    if let Some(v) = k.heap.get_global(name) {
        return Ok(v);
    }
    crate::serial::write_str("unbound: ");
    crate::serial::write_str(name);
    crate::serial::write_str("\n");
    Err("EUNBOUND: symbol has no value")
}

pub fn apply(k: &mut Kernel, f: Val, args: &[Val]) -> Result<Val, &'static str> {
    match f {
        Val::Clos(c) => {
            let (params, body, closure_env, _code) = k.heap.closure(c);
            if k.heap.list_len(params) != args.len() {
                return Err("EINVAL: wrong number of arguments");
            }
            let mut env = closure_env;
            let mut p = params;
            let mut i = 0;
            while let Val::Cons(_) = p {
                let name = k.heap.car(p)?;
                let binding = k.heap.cons(name, args[i])?;
                env = k.heap.cons(binding, env)?;
                p = k.heap.cdr(p)?;
                i += 1;
            }
            eval_body(k, env, body)
        }
        Val::Sym(s) => prim(k, k.heap.sym_name(s), args),
        _ => Err("EINVAL: not callable"),
    }
}

fn prim(k: &mut Kernel, name: &'static str, args: &[Val]) -> Result<Val, &'static str> {
    match name {
        // image operations as ordinary primitives, so a compiled program and an
        // interpreted program see exactly the same functions
        "commit" => Ok(Val::Int(k.img.commit(args[0])? as i64)),
        "root" => Ok(k.img.root()),
        "gen" => Ok(Val::Int(k.img.generation() as i64)),
        "rollback" => match args[0] {
            Val::Int(n) => k.img.rollback(n as u64).map(|g| Val::Int(g as i64)),
            _ => Err("EINVAL: rollback needs a generation number"),
        },
        "cap" => Ok(Val::Cap(k.img.mint()?)),
        "open" => match args[0] {
            Val::Cap(id) if k.img.holds(id) => Ok(k.img.root()),
            Val::Cap(_) => Err("EPERM: no such capability"),
            _ => Err("EINVAL: open needs a capability"),
        },
        "print" => {
            let mut buf = Buf::new();
            let text = crate::print::to_buf(&k.heap, args[0], &mut buf)?;
            crate::serial::write_str(text);
            crate::serial::write_str("\n");
            Ok(args[0])
        }
        "audit" => {
            let mut out = NIL;
            let mut i = 0;
            while i < k.img.n {
                let id = k.img.gens[i].id as i64;
                out = k.heap.cons(Val::Int(id), out)?;
                i += 1;
            }
            Ok(out)
        }
        "+" | "-" | "*" | "<" | "=" => {
            if args.len() != 2 {
                return Err("EINVAL: binary primitive");
            }
            let (a, b) = match (args[0], args[1]) {
                (Val::Int(x), Val::Int(y)) => (x, y),
                _ => return Err("EINVAL: integer expected"),
            };
            Ok(match name {
                "+" => Val::Int(a + b),
                "-" => Val::Int(a - b),
                "*" => Val::Int(a * b),
                "<" => Val::Int((a < b) as i64),
                _ => Val::Int((a == b) as i64),
            })
        }
        "cons" => {
            if args.len() != 2 {
                return Err("EINVAL: cons takes 2");
            }
            k.heap.cons(args[0], args[1])
        }
        "car" => k.heap.car(args[0]),
        "cdr" => k.heap.cdr(args[0]),
        "list" => {
            let mut out = NIL;
            let mut i = args.len();
            while i > 0 {
                i -= 1;
                out = k.heap.cons(args[i], out)?;
            }
            Ok(out)
        }
        "len" => Ok(Val::Int(k.heap.list_len(args[0]) as i64)),
        _ => Err("EUNBOUND: unknown function"),
    }
}
