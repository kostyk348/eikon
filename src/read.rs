// eikon: the reader. Text -> form.
//
// The reader and the printer are inverses of each other. That is not a
// convenience: it is the definition of a homoiconic image. Anything the kernel
// prints can be read back, byte for byte, into the same form - which is how
// state, code and the boot image become one artifact.

use crate::val::{Heap, Val};

pub struct Reader {
    s: &'static str,
    i: usize,
}

impl Reader {
    pub fn new(s: &'static str) -> Self {
        Reader { s, i: 0 }
    }

    fn bytes(&self) -> &'static [u8] {
        self.s.as_bytes()
    }

    fn skip_ws(&mut self) {
        let b = self.bytes();
        while self.i < b.len() {
            let c = b[self.i];
            if c == b' ' || c == b'\n' || c == b'\r' || c == b'\t' {
                self.i += 1;
            } else if c == b';' {
                while self.i < b.len() && b[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }

    fn at_end(&mut self) -> bool {
        self.skip_ws();
        self.i >= self.bytes().len()
    }

    fn token_end(&mut self) -> usize {
        let b = self.bytes();
        while self.i < b.len() {
            let c = b[self.i];
            if c == b' ' || c == b'\n' || c == b'\r' || c == b'\t' || c == b'(' || c == b')' || c == b';' {
                break;
            }
            self.i += 1;
        }
        self.i
    }

    fn parse_int(&mut self, neg: bool) -> Result<Val, &'static str> {
        let b = self.bytes();
        let mut n: i64 = 0;
        let mut any = false;
        while self.i < b.len() {
            let c = b[self.i];
            if !c.is_ascii_digit() {
                break;
            }
            n = n.checked_mul(10).ok_or("EINVAL: integer overflow")?;
            n = n.checked_add((c - b'0') as i64).ok_or("EINVAL: integer overflow")?;
            self.i += 1;
            any = true;
        }
        if !any {
            return Err("EINVAL: lone minus");
        }
        Ok(Val::Int(if neg { -n } else { n }))
    }

    /// One form. `'` is sugar for `(quote ...)`, so quoted data needs no special
    /// node type: a quote is an ordinary form, like everything else.
    pub fn parse(&mut self, h: &mut Heap) -> Result<Val, &'static str> {
        self.skip_ws();
        if self.i >= self.bytes().len() {
            return Err("EOF");
        }
        let c = self.bytes()[self.i];
        match c {
            b'(' => {
                self.i += 1;
                let mut items: [Val; 64] = [Val::Nil; 64];
                let mut n = 0usize;
                loop {
                    self.skip_ws();
                    if self.i >= self.bytes().len() {
                        return Err("EOF inside list");
                    }
                    if self.bytes()[self.i] == b')' {
                        self.i += 1;
                        break;
                    }
                    if n == items.len() {
                        return Err("EINVAL: list too long");
                    }
                    items[n] = self.parse(h)?;
                    n += 1;
                }
                let mut out = Val::Nil;
                let mut j = n;
                while j > 0 {
                    j -= 1;
                    out = h.cons(items[j], out)?;
                }
                Ok(out)
            }
            b')' => Err("EINVAL: unexpected )"),
            b'\'' => {
                self.i += 1;
                let inner = self.parse(h)?;
                let q = Val::Sym(h.intern("quote")?);
                let tail = h.cons(inner, Val::Nil)?;
                h.cons(q, tail)
            }
            b'"' => {
                self.i += 1;
                let b = self.bytes();
                let start = self.i;
                while self.i < b.len() && b[self.i] != b'"' {
                    if b[self.i] == b'\\' && self.i + 1 < b.len() {
                        self.i += 2;
                    } else {
                        self.i += 1;
                    }
                }
                if self.i >= b.len() {
                    return Err("EOF in string");
                }
                let raw = &self.s[start..self.i];
                self.i += 1; // closing quote
                Ok(Val::Str(h.intern_str(raw)?))
            }
            b'-' => {
                self.i += 1;
                if self.i < self.bytes().len() && self.bytes()[self.i].is_ascii_digit() {
                    self.parse_int(true)
                } else {
                    // a symbol that starts with '-', e.g. (- x y) at top level
                    let e = self.token_end();
                    let name = &self.s[self.i - 1..e];
                    Ok(Val::Sym(h.intern(name)?))
                }
            }
            c if c.is_ascii_digit() => self.parse_int(false),
            _ => {
                let start = self.i;
                let e = self.token_end();
                if e == start {
                    return Err("EINVAL: empty token");
                }
                let name = &self.s[start..e];
                Ok(Val::Sym(h.intern(name)?))
            }
        }
    }

    /// Everything in the source, as one list of forms: the program is a value.
    pub fn all(&mut self, h: &mut Heap) -> Result<Val, &'static str> {
        let mut items: [Val; 128] = [Val::Nil; 128];
        let mut n = 0;
        while !self.at_end() {
            if n == items.len() {
                return Err("EINVAL: too many top-level forms");
            }
            items[n] = self.parse(h)?;
            n += 1;
        }
        let mut out = Val::Nil;
        let mut j = n;
        while j > 0 {
            j -= 1;
            out = h.cons(items[j], out)?;
        }
        Ok(out)
    }
}
