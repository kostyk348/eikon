// eikon: values, and the heap they live in.
//
// The whole point of a homoiconic kernel is that there is exactly one kind of
// thing: a value. Code is a value (a list), state is a value (a list), errors
// are values, capabilities are values. This file is therefore the entire data
// model of the system - there is no second representation hiding somewhere.
//
// Interior mutability, on purpose. Once a timer can take the processor away in
// the middle of anything, a `&mut Heap` is a lie: two tasks would each hold one.
// Rust is allowed to assume `&mut` is unique and cache what it reads through it,
// and it did - the scheduler's counters and a task's deadline disagreed with
// reality depending on how the optimiser laid the code out. So the heap is
// shared (`&self`) and its mutable state lives behind `Cell`/`UnsafeCell`:
// every access goes through a pointer that the compiler must not assume is
// unaliased. The mutation that must not be split by a tick (the bump pointer)
// runs with interrupts off; everything else is a single word or a single cell.

use core::cell::{Cell, UnsafeCell};

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
    cells: UnsafeCell<[(Val, Val); CONS_POOL]>,
    ncells: Cell<usize>,
    /// The lowest cell any live task may still reach. Rewinding below the floor
    /// would hand this task's cells to the next one - which is exactly what a
    /// preemptive scheduler makes possible and a cooperative one hides.
    floor: Cell<usize>,
    syms: UnsafeCell<[&'static str; SYM_POOL]>,
    nsyms: Cell<usize>,
    strs: UnsafeCell<[&'static str; STR_POOL]>,
    nstrs: Cell<usize>,
    closures: UnsafeCell<[(Val, Val, Val, i64); CLOS_POOL]>,
    nclosures: Cell<usize>,
    globals: UnsafeCell<[(&'static str, Val); GLOBAL_POOL]>,
    nglobals: Cell<usize>,
}

impl Heap {
    pub const fn new() -> Self {
        Heap {
            cells: UnsafeCell::new([(Val::Nil, Val::Nil); CONS_POOL]),
            ncells: Cell::new(0),
            floor: Cell::new(0),
            syms: UnsafeCell::new([""; SYM_POOL]),
            nsyms: Cell::new(0),
            strs: UnsafeCell::new([""; STR_POOL]),
            nstrs: Cell::new(0),
            closures: UnsafeCell::new([(Val::Nil, Val::Nil, Val::Nil, -1); CLOS_POOL]),
            nclosures: Cell::new(0),
            globals: UnsafeCell::new([("", Val::Nil); GLOBAL_POOL]),
            nglobals: Cell::new(0),
        }
    }

    #[inline]
    fn cells(&self) -> &mut [(Val, Val); CONS_POOL] {
        unsafe { &mut *self.cells.get() }
    }

    /// Allocation is a bump. There is no free list, because nothing is ever
    /// freed: values are immutable, so a value stays valid for the life of the
    /// boot. This is the whole garbage-collector story of eikon.
    pub fn cons(&self, a: Val, b: Val) -> Result<Val, &'static str> {
        if self.ncells.get() == CONS_POOL {
            return Err("ENOMEM: cons pool exhausted");
        }
        // A tick between reading and advancing the pointer would let a second
        // task take the same cell: the bump runs with interrupts off.
        Ok(crate::trap::with_irq_off(|| {
            let i = self.ncells.get();
            self.cells()[i] = (a, b);
            self.ncells.set(i + 1);
            Val::Cons(i as u32)
        }))
    }

    pub fn car(&self, v: Val) -> Result<Val, &'static str> {
        match v {
            Val::Cons(i) => {
                let i = i as usize;
                if i >= self.ncells.get() {
                    return Err("EINVAL: dangling cell");
                }
                Ok(self.cells()[i].0)
            }
            Val::Nil => Err("car of nil"),
            _ => Err("car of non-list"),
        }
    }

    pub fn cdr(&self, v: Val) -> Result<Val, &'static str> {
        match v {
            Val::Cons(i) => {
                let i = i as usize;
                if i >= self.ncells.get() {
                    return Err("EINVAL: dangling cell");
                }
                Ok(self.cells()[i].1)
            }
            Val::Nil => Err("cdr of nil"),
            _ => Err("cdr of non-list"),
        }
    }

    /// One cell, copied out. Callers that need a pair from the environment
    /// cannot hold a reference into the cell array across a possible tick.
    pub fn cell(&self, i: u32) -> Option<(Val, Val)> {
        let i = i as usize;
        if i >= self.ncells.get() {
            return None;
        }
        Some(self.cells()[i])
    }

    pub fn intern(&self, s: &'static str) -> Result<u32, &'static str> {
        let syms = unsafe { &mut *self.syms.get() };
        let n = self.nsyms.get();
        let mut i = 0;
        while i < n {
            if syms[i] == s {
                return Ok(i as u32);
            }
            i += 1;
        }
        if n == SYM_POOL {
            return Err("ENOMEM: symbol table full");
        }
        syms[n] = s;
        self.nsyms.set(n + 1);
        Ok(n as u32)
    }

    pub fn sym_name(&self, i: u32) -> &'static str {
        unsafe { (*self.syms.get())[i as usize] }
    }

    pub fn intern_str(&self, s: &'static str) -> Result<u32, &'static str> {
        let strs = unsafe { &mut *self.strs.get() };
        let n = self.nstrs.get();
        let mut i = 0;
        while i < n {
            if strs[i] == s {
                return Ok(i as u32);
            }
            i += 1;
        }
        if n == STR_POOL {
            return Err("ENOMEM: string pool full");
        }
        strs[n] = s;
        self.nstrs.set(n + 1);
        Ok(n as u32)
    }

    pub fn str_of(&self, i: u32) -> &'static str {
        unsafe { (*self.strs.get())[i as usize] }
    }

    /// A closure keeps its source forms *and*, when it came from the compiler,
    /// the code block that implements them. That is what lets the two engines
    /// call each other's functions: the forms are the truth, code is a cache.
    pub fn mk_closure(&self, params: Val, body: Val, env: Val, code: i64) -> Result<Val, &'static str> {
        crate::trap::with_irq_off(|| {
            let n = self.nclosures.get();
            if n == CLOS_POOL {
                return Err("ENOMEM: closure pool full");
            }
            unsafe { (*self.closures.get())[n] = (params, body, env, code) };
            self.nclosures.set(n + 1);
            Ok(Val::Clos(n as u32))
        })
    }

    pub fn closure(&self, i: u32) -> (Val, Val, Val, i64) {
        unsafe { (*self.closures.get())[i as usize] }
    }

    pub fn set_global(&self, name: &'static str, v: Val) {
        crate::trap::with_irq_off(|| {
            let g = unsafe { &mut *self.globals.get() };
            let n = self.nglobals.get();
            let mut i = 0;
            while i < n {
                if g[i].0 == name {
                    g[i].1 = v;
                    return;
                }
                i += 1;
            }
            if n < GLOBAL_POOL {
                g[n] = (name, v);
                self.nglobals.set(n + 1);
            }
        });
    }

    #[allow(dead_code)]
    pub fn debug_globals(&self) -> (usize, &'static str) {
        let n = self.nglobals.get();
        if n == 0 {
            (0, "")
        } else {
            (n, unsafe { (*self.globals.get())[0].0 })
        }
    }

    pub fn get_global(&self, name: &str) -> Option<Val> {
        let g = unsafe { &*self.globals.get() };
        let n = self.nglobals.get();
        let mut i = 0;
        while i < n {
            if g[i].0 == name {
                return Some(g[i].1);
            }
            i += 1;
        }
        None
    }

    /// Mark the arena. Everything allocated after this mark can be dropped by
    /// moving the bump pointer back - which is only sound because no value is
    /// ever mutated. This is eikon's whole garbage collector: rewind, not collect.
    pub fn ncells(&self) -> usize {
        self.ncells.get()
    }

    pub fn nsyms(&self) -> usize {
        self.nsyms.get()
    }

    pub fn nstrs(&self) -> usize {
        self.nstrs.get()
    }

    pub fn nclosures(&self) -> usize {
        self.nclosures.get()
    }

    pub fn nglobals(&self) -> usize {
        self.nglobals.get()
    }

    pub fn mark(&self) -> usize {
        self.ncells.get()
    }

    pub fn floor(&self) -> usize {
        self.floor.get()
    }

    pub fn set_floor(&self, v: usize) {
        self.floor.set(v);
    }

    pub fn rewind(&self, mark: usize) {
        let f = self.floor.get();
        let m = if mark > f { mark } else { f };
        if m <= self.ncells.get() {
            self.ncells.set(m);
        }
    }

    /// Proper list -> count. Used by the printer and by callers.
    pub fn list_len(&self, mut v: Val) -> usize {
        let mut n = 0;
        while let Val::Cons(i) = v {
            n += 1;
            if (i as usize) >= self.ncells.get() {
                break;
            }
            v = self.cells()[i as usize].1;
        }
        n
    }
}

/// A form built from a slice of values. Handy for tests and for the boot image.
#[allow(dead_code)]
pub fn list_of(h: &Heap, items: &[Val]) -> Result<Val, &'static str> {
    let mut out = Val::Nil;
    let mut i = items.len();
    while i > 0 {
        i -= 1;
        out = h.cons(items[i], out)?;
    }
    Ok(out)
}
