// eikon: values, and the heap they live in.
//
// The whole point of a homoiconic kernel is that there is exactly one kind of
// thing: a value. Code is a value (a list), state is a value (a list), errors
// are values, capabilities are values. This file is therefore the entire data
// model of the system - there is no second representation hiding somewhere.

pub const NIL: Val = Val::Nil;

/// A value is 16 bytes, Copy, and self-describing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Val {
    Nil,
    /// Small integer. No bignums: a kernel that allocates on arithmetic is a
    /// kernel that pauses.
    Int(i64),
    /// Interned symbol; the name lives in the heap's symbol table.
    Sym(u32),
    /// Cons cell, indexed into the cell pool.
    Cons(u32),
    /// Closure: (params . body) plus the environment it closed over.
    Clos(u32),
    /// String literal; slices the (static) program image.
    Str(u32),
    /// An unforgeable capability handle minted by the image.
    Cap(u64),
}

pub const CONS_POOL: usize = 1 << 16; // 1 MiB of cells; the arena is the memory model
pub const SYM_POOL: usize = 256;
pub const STR_POOL: usize = 128;
pub const CLOS_POOL: usize = 128;
pub const GLOBAL_POOL: usize = 128;

pub struct Heap {
    pub cells: [(Val, Val); CONS_POOL],
    pub ncells: usize,
    pub syms: [&'static str; SYM_POOL],
    pub nsyms: usize,
    pub strs: [&'static str; STR_POOL],
    pub nstrs: usize,
    pub closures: [(Val, Val, Val, i64); CLOS_POOL], // params, body forms, env, code (-1 = none)
    pub nclosures: usize,
    pub globals: [(&'static str, Val); GLOBAL_POOL],
    pub nglobals: usize,
}

impl Heap {
    pub const fn new() -> Self {
        Heap {
            cells: [(Val::Nil, Val::Nil); CONS_POOL],
            ncells: 0,
            syms: [""; SYM_POOL],
            nsyms: 0,
            strs: [""; STR_POOL],
            nstrs: 0,
            closures: [(Val::Nil, Val::Nil, Val::Nil, -1); CLOS_POOL],
            nclosures: 0,
            globals: [("", Val::Nil); GLOBAL_POOL],
            nglobals: 0,
        }
    }

    /// Allocation is a bump. There is no free list, because nothing is ever
    /// freed: values are immutable, so a value stays valid for the life of the
    /// boot. This is the whole garbage-collector story of eikon.
    pub fn cons(&mut self, a: Val, b: Val) -> Result<Val, &'static str> {
        if self.ncells == CONS_POOL {
            return Err("ENOMEM: cons pool exhausted");
        }
        let i = self.ncells;
        self.cells[i] = (a, b);
        self.ncells += 1;
        Ok(Val::Cons(i as u32))
    }

    pub fn car(&self, v: Val) -> Result<Val, &'static str> {
        match v {
            Val::Cons(i) => Ok(self.cells[i as usize].0),
            Val::Nil => Err("car of nil"),
            _ => Err("car of non-list"),
        }
    }

    pub fn cdr(&self, v: Val) -> Result<Val, &'static str> {
        match v {
            Val::Cons(i) => Ok(self.cells[i as usize].1),
            Val::Nil => Err("cdr of nil"),
            _ => Err("cdr of non-list"),
        }
    }

    pub fn intern(&mut self, s: &'static str) -> Result<u32, &'static str> {
        for i in 0..self.nsyms {
            if self.syms[i] == s {
                return Ok(i as u32);
            }
        }
        if self.nsyms == SYM_POOL {
            return Err("ENOMEM: symbol table full");
        }
        self.syms[self.nsyms] = s;
        self.nsyms += 1;
        Ok((self.nsyms - 1) as u32)
    }

    pub fn sym_name(&self, i: u32) -> &'static str {
        self.syms[i as usize]
    }

    pub fn intern_str(&mut self, s: &'static str) -> Result<u32, &'static str> {
        for i in 0..self.nstrs {
            if self.strs[i] == s {
                return Ok(i as u32);
            }
        }
        if self.nstrs == STR_POOL {
            return Err("ENOMEM: string pool full");
        }
        self.strs[self.nstrs] = s;
        self.nstrs += 1;
        Ok((self.nstrs - 1) as u32)
    }

    /// A closure keeps its source forms *and*, when it came from the compiler,
    /// the code block that implements them. That is what lets the two engines
    /// call each other's functions: the forms are the truth, code is a cache.
    pub fn mk_closure(&mut self, params: Val, body: Val, env: Val, code: i64) -> Result<Val, &'static str> {
        if self.nclosures == CLOS_POOL {
            return Err("ENOMEM: closure pool full");
        }
        let i = self.nclosures;
        self.closures[i] = (params, body, env, code);
        self.nclosures += 1;
        Ok(Val::Clos(i as u32))
    }

    pub fn closure(&self, i: u32) -> (Val, Val, Val, i64) {
        self.closures[i as usize]
    }

    pub fn set_global(&mut self, name: &'static str, v: Val) {
        for i in 0..self.nglobals {
            if self.globals[i].0 == name {
                self.globals[i].1 = v;
                return;
            }
        }
        if self.nglobals < GLOBAL_POOL {
            self.globals[self.nglobals] = (name, v);
            self.nglobals += 1;
        }
    }

    pub fn get_global(&self, name: &str) -> Option<Val> {
        for i in 0..self.nglobals {
            if self.globals[i].0 == name {
                return Some(self.globals[i].1);
            }
        }
        None
    }

    /// Mark the arena. Everything allocated after this mark can be dropped by
    /// moving the bump pointer back - which is only sound because no value is
    /// ever mutated. This is eikon's whole garbage collector: rewind, not collect.
    pub fn mark(&self) -> usize {
        self.ncells
    }

    pub fn rewind(&mut self, mark: usize) {
        if mark <= self.ncells {
            self.ncells = mark;
        }
    }

    /// Proper list -> count. Used by the printer and by callers.
    pub fn list_len(&self, mut v: Val) -> usize {
        let mut n = 0;
        while let Val::Cons(i) = v {
            n += 1;
            v = self.cells[i as usize].1;
        }
        n
    }
}

/// A form built from a slice of values. Handy for tests and for the boot image.
#[allow(dead_code)]
pub fn list_of(h: &mut Heap, items: &[Val]) -> Result<Val, &'static str> {
    let mut out = Val::Nil;
    let mut i = items.len();
    while i > 0 {
        i -= 1;
        out = h.cons(items[i], out)?;
    }
    Ok(out)
}
