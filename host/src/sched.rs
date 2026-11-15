// The scheduler is a kernel feature: it needs stacks, traps and a timer. On the
// host the calls exist so the language compiles, and they answer honestly.
use crate::val::Val;

pub const POLICY_KEEP: u8 = 0;
pub const POLICY_CANCEL: u8 = 1;
pub const POLICY_RETRY: u8 = 2;

#[derive(Clone, Copy)]
pub struct Task {
    pub missed: bool,
    pub took_ms: u64,
}

pub struct Sched {
    pub tasks: [Task; 4],
    pub n: usize,
    pub misses: u64,
    pub policy: u8,
}

impl Sched {
    pub fn spawn(&mut self, _name: &str, _deadline_ms: u64, _func: Val) -> Result<u64, &'static str> {
        Err("ENOSYS: no scheduler on the host")
    }
}

static mut SCHED: Sched = Sched {
    tasks: [Task { missed: false, took_ms: 0 }; 4],
    n: 0,
    misses: 0,
    policy: 0,
};

#[allow(static_mut_refs)]
pub fn sched() -> &'static mut Sched {
    unsafe { &mut SCHED }
}

pub fn start() {}

pub fn set_policy(p: u8) -> u8 {
    let s = sched();
    let old = s.policy;
    s.policy = p;
    old
}
