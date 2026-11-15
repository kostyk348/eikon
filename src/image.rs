// eikon: the image. State as generations of a form.
//
// Everything the machine is, is one form (the root). A change does not mutate
// that form - it publishes a new one and remembers which generation it grew
// out of. Rollback is therefore not a feature: it is pointing at an older
// generation. Audit is not a log: it is the parent chain.
//
// Capabilities live here too, because authority over an image is part of the
// image model, not a separate subsystem.

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
    pub gens: [Gen; MAX_GENS],
    pub n: usize,
    pub caps: [u64; MAX_CAPS],
    pub ncaps: usize,
    pub secret: u64,
    pub counter: u64,
}

impl Image {
    pub const fn new() -> Self {
        Image {
            gens: [Gen { id: 0, root: Val::Nil, parent: -1 }; MAX_GENS],
            n: 0,
            caps: [0; MAX_CAPS],
            ncaps: 0,
            secret: 0,
            counter: 0,
        }
    }

    pub fn root(&self) -> Val {
        if self.n == 0 {
            Val::Nil
        } else {
            self.gens[self.n - 1].root
        }
    }

    pub fn generation(&self) -> u64 {
        if self.n == 0 {
            0
        } else {
            self.gens[self.n - 1].id
        }
    }

    /// Publish a new generation. The old one is not touched, not copied and not
    /// invalidated: it stays addressable for as long as the boot lasts, which is
    /// exactly what makes rollback free.
    pub fn commit(&mut self, form: Val) -> Result<u64, &'static str> {
        if self.n == MAX_GENS {
            return Err("ENOSPC: generation table full");
        }
        self.counter += 1;
        let id = self.counter;
        let parent = if self.n == 0 { -1 } else { (self.n - 1) as i64 };
        self.gens[self.n] = Gen { id, root: form, parent };
        self.n += 1;
        Ok(id)
    }

    pub fn rollback(&mut self, id: u64) -> Result<u64, &'static str> {
        for i in 0..self.n {
            if self.gens[i].id == id {
                // Rollback = publish that old root again under a new generation,
                // so the history keeps its shape instead of being rewritten.
                return self.commit(self.gens[i].root);
            }
        }
        Err("ENOENT: no such generation")
    }

    /// A handle is unforgeable because it is derived from a secret that never
    /// leaves the kernel. Knowing a generation number is not authority.
    pub fn mint(&mut self) -> Result<u64, &'static str> {
        if self.ncaps == MAX_CAPS {
            return Err("ENOSPC: capability table full");
        }
        self.counter += 1;
        let h = mix(self.secret, self.generation(), self.counter);
        self.caps[self.ncaps] = h;
        self.ncaps += 1;
        Ok(h)
    }

    pub fn holds(&self, h: u64) -> bool {
        for i in 0..self.ncaps {
            if self.caps[i] == h {
                return true;
            }
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
    pub codes: [crate::bytecode::Code; crate::bytecode::MAX_CODES],
    pub ncodes: usize,
    pub consts: [Val; crate::bytecode::MAX_CONSTS],
    pub nconsts: usize,
}

impl Kernel {
    pub const fn new() -> Self {
        Kernel {
            heap: Heap::new(),
            img: Image::new(),
            codes: [crate::bytecode::Code::new(); crate::bytecode::MAX_CODES],
            ncodes: 0,
            consts: [Val::Nil; crate::bytecode::MAX_CONSTS],
            nconsts: 0,
        }
    }
}
