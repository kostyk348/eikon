// eikon: the image. State as generations of a form.
//
// Everything the machine is, is one form (the root). A change does not mutate
// that form - it publishes a new one and remembers which generation it grew
// out of. Rollback is therefore not a feature: it is pointing at an older
// generation. Audit is not a log: it is the parent chain.
//
// Capabilities live here too, because authority over an image is part of the
// image model, not a separate subsystem.
//
// Like the heap, the image is shared (`&self`) with its mutable state behind
// `Cell`/`UnsafeCell`: a timer can interrupt any of this, and a `&mut Kernel`
// held across that interrupt would be a second unique borrow of the same object.

use core::cell::{Cell, UnsafeCell};

use crate::val::{Heap, Val};

pub const MAX_GENS: usize = 32;
pub const MAX_CAPS: usize = 32;

#[derive(Clone, Copy)]
pub struct Gen {
    pub id: u64,
    pub root: Val,
    pub parent: i64, // index into gens, -1 for the genesis
}

pub struct Image {
    gens: UnsafeCell<[Gen; MAX_GENS]>,
    n: Cell<usize>,
    caps: UnsafeCell<[u64; MAX_CAPS]>,
    ncaps: Cell<usize>,
    secret: Cell<u64>,
    counter: Cell<u64>,
}

impl Image {
    pub const fn new() -> Self {
        Image {
            gens: UnsafeCell::new([Gen { id: 0, root: Val::Nil, parent: -1 }; MAX_GENS]),
            n: Cell::new(0),
            caps: UnsafeCell::new([0; MAX_CAPS]),
            ncaps: Cell::new(0),
            secret: Cell::new(0),
            counter: Cell::new(0),
        }
    }

    fn gens(&self) -> &mut [Gen; MAX_GENS] {
        unsafe { &mut *self.gens.get() }
    }

    pub fn set_secret(&self, s: u64) {
        self.secret.set(s);
    }

    pub fn count(&self) -> usize {
        self.n.get()
    }

    pub fn gen_at(&self, i: usize) -> Gen {
        self.gens()[i]
    }

    pub fn caps_count(&self) -> usize {
        self.ncaps.get()
    }

    pub fn root(&self) -> Val {
        let n = self.n.get();
        if n == 0 {
            Val::Nil
        } else {
            self.gens()[n - 1].root
        }
    }

    pub fn generation(&self) -> u64 {
        let n = self.n.get();
        if n == 0 {
            0
        } else {
            self.gens()[n - 1].id
        }
    }

    /// Publish a new generation. The old one is not touched, not copied and not
    /// invalidated: it stays addressable for as long as the boot lasts, which is
    /// exactly what makes rollback free.
    pub fn commit(&self, form: Val) -> Result<u64, &'static str> {
        crate::trap::with_irq_off(|| {
            let n = self.n.get();
            if n == MAX_GENS {
                return Err("ENOSPC: generation table full");
            }
            let c = self.counter.get() + 1;
            self.counter.set(c);
            let parent = if n == 0 { -1 } else { (n - 1) as i64 };
            self.gens()[n] = Gen { id: c, root: form, parent };
            self.n.set(n + 1);
            Ok(c)
        })
    }

    pub fn rollback(&self, id: u64) -> Result<u64, &'static str> {
        let n = self.n.get();
        let mut i = 0;
        while i < n {
            if self.gens()[i].id == id {
                // Rollback = publish that old root again under a new generation,
                // so the history keeps its shape instead of being rewritten.
                return self.commit(self.gens()[i].root);
            }
            i += 1;
        }
        Err("ENOENT: no such generation")
    }

    /// A handle is unforgeable because it is derived from a secret that never
    /// leaves the kernel. Knowing a generation number is not authority.
    pub fn mint(&self) -> Result<u64, &'static str> {
        crate::trap::with_irq_off(|| {
            let nc = self.ncaps.get();
            if nc == MAX_CAPS {
                return Err("ENOSPC: capability table full");
            }
            let c = self.counter.get() + 1;
            self.counter.set(c);
            let h = mix(self.secret.get(), self.generation(), c);
            unsafe { (*self.caps.get())[nc] = h };
            self.ncaps.set(nc + 1);
            Ok(h)
        })
    }

    pub fn holds(&self, h: u64) -> bool {
        let caps = unsafe { &*self.caps.get() };
        let n = self.ncaps.get();
        let mut i = 0;
        while i < n {
            if caps[i] == h {
                return true;
            }
            i += 1;
        }
        false
    }
}

/// splitmix-style finaliser: cheap, and enough to make a handle unguessable
/// without carrying a hash library into a kernel.
pub fn mix(secret: u64, a: u64, b: u64) -> u64 {
    let mut x = secret ^ a.rotate_left(17) ^ b.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

pub struct Kernel {
    pub heap: Heap,
    pub img: Image,
    // the compiled image: code blocks and their constant pool
    codes: UnsafeCell<[crate::bytecode::Code; crate::bytecode::MAX_CODES]>,
    ncodes: Cell<usize>,
    consts: UnsafeCell<[Val; crate::bytecode::MAX_CONSTS]>,
    nconsts: Cell<usize>,
    pub blk: crate::virtio::Blk,
}

impl Kernel {
    pub const fn new() -> Self {
        Kernel {
            heap: Heap::new(),
            img: Image::new(),
            codes: UnsafeCell::new([crate::bytecode::Code::new(); crate::bytecode::MAX_CODES]),
            ncodes: Cell::new(0),
            consts: UnsafeCell::new([Val::Nil; crate::bytecode::MAX_CONSTS]),
            nconsts: Cell::new(0),
            blk: crate::virtio::Blk::none(),
        }
    }

    pub fn set_blk(&self, b: crate::virtio::Blk) {
        unsafe { core::ptr::write(core::ptr::addr_of!(self.blk) as *mut crate::virtio::Blk, b) };
    }

    pub fn ncodes(&self) -> usize {
        self.ncodes.get()
    }

    pub fn nconsts(&self) -> usize {
        self.nconsts.get()
    }

    /// One instruction, copied out. `code()` used to copy the whole block
    /// (256 ops) per fetch and made the VM twice as slow as the tree-walker -
    /// a benchmark is what caught it.
    pub fn op(&self, code: usize, ip: usize) -> crate::bytecode::Op {
        unsafe { (*self.codes.get())[code].ops[ip] }
    }

    pub fn code_len(&self, code: usize) -> usize {
        unsafe { (*self.codes.get())[code].n }
    }

    pub fn code_arity(&self, code: usize) -> u8 {
        unsafe { (*self.codes.get())[code].arity }
    }

    #[allow(dead_code)]
    pub fn code(&self, i: usize) -> crate::bytecode::Code {
        unsafe { (*self.codes.get())[i] }
    }

    /// Raw access for the compiler's inner loop; the caller must hold the
    /// interrupt-off guard (see `Emitter::push`).
    pub fn code_ptr(&self, i: usize) -> *mut crate::bytecode::Code {
        unsafe { core::ptr::addr_of_mut!((*self.codes.get())[i]) }
    }

    pub fn code_mut(&self, i: usize, f: impl FnOnce(&mut crate::bytecode::Code)) {
        crate::trap::with_irq_off(|| {
            let c = unsafe { &mut (*self.codes.get())[i] };
            f(c);
        });
    }

    pub fn alloc_code(&self, c: crate::bytecode::Code) -> Result<u16, &'static str> {
        crate::trap::with_irq_off(|| {
            let n = self.ncodes.get();
            if n == crate::bytecode::MAX_CODES {
                return Err("ENOSPC: code table full");
            }
            unsafe { (*self.codes.get())[n] = c };
            self.ncodes.set(n + 1);
            Ok(n as u16)
        })
    }

    pub fn constant(&self, i: usize) -> Val {
        unsafe { (*self.consts.get())[i] }
    }

    pub fn push_const(&self, v: Val) -> Result<u16, &'static str> {
        crate::trap::with_irq_off(|| {
            let n = self.nconsts.get();
            if n == crate::bytecode::MAX_CONSTS {
                return Err("ENOSPC: constant pool full");
            }
            unsafe { (*self.consts.get())[n] = v };
            self.nconsts.set(n + 1);
            Ok(n as u16)
        })
    }
}
