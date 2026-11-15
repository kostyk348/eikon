// The host's "UART" is stdout.
use std::io::Write;

pub fn write_str(s: &str) {
    let out = std::io::stdout();
    let mut lock = out.lock();
    let _ = lock.write_all(s.as_bytes());
}

pub fn write_num(v: i64) {
    write_str(&v.to_string());
}
