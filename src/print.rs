// eikon: the printer. Form -> text, canonically.
//
// Two forms are equal exactly when their printed text is equal: there is one
// spelling per value. That invariant is what makes a generation of the machine
// a document with a hash instead of a pile of memory.

use crate::val::{Heap, Val};

pub fn emit(h: &Heap, v: Val, out: &mut dyn FnMut(u8)) -> Result<(), &'static str> {
    match v {
        Val::Nil => out_str(out, "()"),
        Val::Int(n) => out_str(out, int_str(n)),
        Val::Sym(i) => out_str(out, h.sym_name(i)),
        Val::Str(i) => {
            out(b'"');
            for b in h.strs[i as usize].as_bytes() {
                if *b == b'"' || *b == b'\\' {
                    out(b'\\');
                }
                out(*b);
            }
            out(b'"');
        }
        Val::Cap(id) => {
            out_str(out, "#cap:");
            out_str(out, hex_u64(id));
        }
        Val::Cons(_) => {
            out(b'(');
            let mut cur = v;
            let mut first = true;
            let mut guard = 0;
            loop {
                guard += 1;
                if guard > 100_000 {
                    return Err("ELOOP: list is cyclic or too long");
                }
                match cur {
                    Val::Cons(i) => {
                        let (car, cdr) = h.cells[i as usize];
                        if !first {
                            out(b' ');
                        }
                        first = false;
                        emit(h, car, out)?;
                        match cdr {
                            Val::Nil => break,
                            Val::Cons(_) => cur = cdr,
                            other => {
                                out_str(out, " . ");
                                emit(h, other, out)?;
                                break;
                            }
                        }
                    }
                    Val::Nil => break,
                    other => {
                        out_str(out, " . ");
                        emit(h, other, out)?;
                        break;
                    }
                }
            }
            out(b')');
        }
        Val::Clos(_) => out_str(out, "#<closure>"),
    }
    Ok(())
}

/// Fixed-size buffer, no allocator, no heap.
pub const BUF: usize = 2048;

pub struct Buf {
    pub data: [u8; BUF],
    pub len: usize,
}

impl Buf {
    pub fn new() -> Self {
        Buf { data: [0; BUF], len: 0 }
    }
    pub fn push(&mut self, b: u8) {
        if self.len < BUF {
            self.data[self.len] = b;
            self.len += 1;
        }
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.data[..self.len]).unwrap_or("<bad utf8>")
    }
}

pub fn to_buf<'a>(h: &Heap, v: Val, buf: &'a mut Buf) -> Result<&'a str, &'static str> {
    buf.len = 0;
    emit(h, v, &mut |b| buf.push(b))?;
    Ok(buf.as_str())
}

fn out_str(out: &mut dyn FnMut(u8), s: &str) {
    for b in s.as_bytes() {
        out(*b);
    }
}

/// i64 -> decimal, without format! (no alloc in the kernel).
pub fn int_str(n: i64) -> &'static str {
    static mut BUF: [u8; 24] = [0; 24];
    // Single hart, no preemption: one scratch buffer is safe here, and it is the
    // price of printing integers without an allocator.
    unsafe {
        let buf = &mut *core::ptr::addr_of_mut!(BUF);
        let mut neg = false;
        let mut x = n;
        if x < 0 {
            neg = true;
            x = -x;
        }
        let mut tmp = [0u8; 24];
        let mut k = 0;
        if x == 0 {
            tmp[0] = b'0';
            k = 1;
        }
        while x > 0 {
            tmp[k] = b'0' + (x % 10) as u8;
            x /= 10;
            k += 1;
        }
        let mut p = 0;
        if neg {
            buf[p] = b'-';
            p += 1;
        }
        while k > 0 {
            k -= 1;
            buf[p] = tmp[k];
            p += 1;
        }
        core::str::from_utf8_unchecked(&buf[..p])
    }
}

fn hex_u64(mut v: u64) -> &'static str {
    static mut BUF: [u8; 17] = [0; 17];
    unsafe {
        let buf = &mut *core::ptr::addr_of_mut!(BUF);
        for i in (0..16).rev() {
            let d = (v & 0xf) as u8;
            buf[i] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
            v >>= 4;
        }
        core::str::from_utf8_unchecked(&buf[..16])
    }
}
