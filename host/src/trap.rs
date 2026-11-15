// On the host there are no interrupts to mask: the judge is a real operating
// system with its own scheduler, and pretending otherwise would only hide bugs.
pub fn with_irq_off<R>(f: impl FnOnce() -> R) -> R {
    f()
}

pub fn now() -> u64 {
    0
}
