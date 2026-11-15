// eikon: a preemptive scheduler of contracts, not of priorities.
//
// A priority says "I am important". A contract says "I promised to finish within
// 5 ms". Every task runs on its own stack; the timer takes the processor away
// every millisecond, the trampoline saves the frame on the task's own stack, and
// the scheduler hands back the frame of the next task. Switching contexts is
// therefore nothing but a different stack pointer.
//
// When a task finishes, what it promised is compared with what it cost - and a
// broken promise is counted and named. Silence is the one thing a scheduler must
// not do about a broken promise.

use crate::val::Val;

/// One millisecond at the 10 MHz timebase: preempt often enough to see it.
pub const TICK: u64 = 10_000;
pub const MAX_TASKS: usize = 2;
/// Deep enough for `fib 17` through the interpreter: each recursive apply()
/// frame carries its argument array on the stack, so 32 KiB overflowed quietly
/// into the scheduler that lives next to the stacks in .bss. The canary below
/// makes that failure loud instead of mysterious.
/// One megabyte per task. The interpreter is one recursively inlined function:
/// once the optimiser folds eval/apply/eval_body together, a single `fib 17`
/// level costs kilobytes, and 128 KiB was not enough - the task ran off the
/// bottom of its stack into the heap and corrupted the values it was reading.
pub const STACK: usize = 1024 * 1024;
const CANARY: u64 = 0xfeed_face_dead_beef;

#[derive(Clone, Copy)]
pub struct Task {
    /// Copied, not borrowed: a task outlives the arena it was named from.
    name: [u8; 16],
    name_len: u8,
    pub deadline_ms: u64,
    pub func: Val,
    pub sp: u64,
    pub started: u64,
    pub slices: u64,
    pub finished: bool,
    pub missed: bool,
    pub took_ms: u64,
}

impl Task {
    pub const fn empty() -> Self {
        Task {
            name: [0; 16],
            name_len: 0,
            deadline_ms: 0,
            func: Val::Nil,
            sp: 0,
            started: 0,
            slices: 0,
            finished: false,
            missed: false,
            took_ms: 0,
        }
    }
}

pub struct Sched {
    pub tasks: [Task; MAX_TASKS],
    pub n: usize,
    pub cur: i32,
    pub idle_sp: u64,
    pub ticks: u64,
    pub preemptions: u64,
    pub misses: u64,
    pub failed: bool,
}

impl Task {
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
    }
}

impl Sched {
    pub const fn new() -> Self {
        Sched {
            tasks: [Task::empty(); MAX_TASKS],
            n: 0,
            cur: -1,
            idle_sp: 0,
            ticks: 0,
            preemptions: 0,
            misses: 0,
            failed: false,
        }
    }

    pub fn spawn(&mut self, name: &str, deadline_ms: u64, func: Val) -> Result<u64, &'static str> {
        if self.n == MAX_TASKS {
            return Err("ENOSPC: task table full");
        }
        let bytes = name.as_bytes();
        if bytes.len() > 16 {
            return Err("ENAMETOOLONG: task name over 16 bytes");
        }
        let i = self.n;
        unsafe {
            let base = &mut *core::ptr::addr_of_mut!(STACKS);
            core::ptr::write_volatile(base[i].as_mut_ptr() as *mut u64, CANARY);
        }
        // a function address is an integer here: this is a kernel
        #[allow(function_casts_as_integer)]
        let entry: usize = task_entry as usize;
        let sp = crate::trap::new_task_frame(stack_top(i), entry);
        let mut nm = [0u8; 16];
        nm[..bytes.len()].copy_from_slice(bytes);
        self.tasks[i] = Task {
            name: nm,
            name_len: bytes.len() as u8,
            deadline_ms,
            func,
            sp: sp as u64,
            ..Task::empty()
        };
        self.n += 1;
        Ok((i + 1) as u64)
    }

    /// Round-robin over unfinished tasks, starting after `from`.
    fn next_task(&self, from: i32) -> i32 {
        if self.n == 0 || self.n > MAX_TASKS {
            return -1;
        }
        let mut k = 0;
        while k < self.n {
            let cand = ((from + 1 + k as i32).rem_euclid(self.n as i32)) as usize;
            if !self.tasks[cand].finished {
                return cand as i32;
            }
            k += 1;
        }
        -1
    }

    /// Take the interrupted frame, return the frame to resume with.
    /// Called from the trap trampoline with interrupts already disabled.
    pub fn switch(&mut self, sp: u64) -> u64 {
        // A task must stay inside its own stack. Checking it here turns a
        // silent smash of .bss into a named failure with the numbers attached.
        if self.cur >= 0 {
            let i = self.cur as usize;
            let (lo, hi) = stack_bounds(i);
            if (sp as usize) < lo || (sp as usize) >= hi {
                crate::serial::write_str("\nSTACK OVERFLOW: task ");
                crate::serial::write_num(i as i64);
                crate::serial::write_str(" sp=");
                crate::serial::write_num(sp as i64);
                crate::serial::write_str(" not in [");
                crate::serial::write_num(lo as i64);
                crate::serial::write_str(",");
                crate::serial::write_num(hi as i64);
                crate::serial::write_str(")\n");
                self.failed = true;
            }
        }
        // An invariant, not a hope: a smashed scheduler must say so instead of
        // indexing somewhere creative.
        if self.n > MAX_TASKS || self.cur >= MAX_TASKS as i32 {
            crate::serial::write_str("\nSCHEDULER CORRUPT: n=");
            crate::serial::write_num(self.n as i64);
            crate::serial::write_str(" cur=");
            crate::serial::write_num(self.cur as i64);
            crate::serial::write_str("\n");
            self.failed = true;
            self.n = 0;
            self.cur = -1;
        }
        if self.cur >= 0 {
            let i = self.cur as usize;
            if !self.tasks[i].finished {
                self.tasks[i].sp = sp;
                self.tasks[i].slices += 1;
                self.preemptions += 1;
            }
        } else {
            self.idle_sp = sp;
        }

        let next = self.next_task(self.cur);
        if next < 0 {
            self.cur = -1;
            return self.idle_sp;
        }
        self.cur = next;
        if self.tasks[next as usize].started == 0 {
            self.tasks[next as usize].started = crate::trap::now();
        }
        self.tasks[next as usize].sp
    }
}

static mut SCHED: Sched = Sched::new();
static mut STACKS: [[u8; STACK]; MAX_TASKS] = [[0; STACK]; MAX_TASKS];

/// Did any task's stack overflow? Checked after the run, so an overflow is a
/// named failure and not a mystery panic somewhere else in .bss.
pub fn stacks_intact() -> bool {
    let mut i = 0;
    let base = unsafe { &mut *core::ptr::addr_of_mut!(STACKS) };
    let n = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*sched_ptr()).n)) };
    while i < n {
        let magic = unsafe { core::ptr::read_volatile(base[i].as_mut_ptr() as *const u64) };
        if magic != CANARY {
            return false;
        }
        i += 1;
    }
    true
}

pub fn sched_ptr() -> *mut Sched {
    core::ptr::addr_of_mut!(SCHED)
}

pub fn sched() -> &'static mut Sched {
    unsafe { &mut *sched_ptr() }
}

fn stack_bounds(i: usize) -> (usize, usize) {
    let s: &'static mut [[u8; STACK]; MAX_TASKS] = unsafe { &mut *core::ptr::addr_of_mut!(STACKS) };
    let base = s[i].as_mut_ptr() as usize;
    (base, base + STACK)
}

fn stack_top(i: usize) -> usize {
    let s: &'static mut [[u8; STACK]; MAX_TASKS] = unsafe { &mut *core::ptr::addr_of_mut!(STACKS) };
    s[i].as_mut_ptr() as usize + STACK - 16
}

/// One task's whole life: run the closure, account the promise, yield forever.
///
/// No `&mut Sched` is held across `apply`: a timer tick hands the handler a
/// second one, and two live `&mut` to the same object is undefined behaviour
/// the optimiser is free to exploit - it was, and the counters disagreed.
#[no_mangle]
pub extern "C" fn task_entry() -> ! {
    let p = sched_ptr();
    let cur = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*p).cur)) };

    if cur >= 0 {
        let i = cur as usize;
        let func = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*p).tasks[i].func)) };
        let started = crate::trap::now();
        let k = unsafe { &*core::ptr::addr_of!(crate::KERNEL) };
        let res = crate::eval::apply(k, func, &[]);
        let ok = res.is_ok();
        if let Err(e) = res {
            crate::serial::write_str("\ntask failed: ");
            crate::serial::write_str(e);
            crate::serial::write_str("\n");
        }
        let took = crate::trap::now().saturating_sub(started) / 10_000;

        let p = sched_ptr();
        unsafe {
            let deadline = core::ptr::read_volatile(core::ptr::addr_of!((*p).tasks[i].deadline_ms));
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).tasks[i].took_ms), took);
            let missed = took > deadline;
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).tasks[i].missed), missed);
            if missed {
                let m = core::ptr::read_volatile(core::ptr::addr_of!((*p).misses));
                core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).misses), m + 1);
            }
            if !ok {
                core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).failed), true);
            }
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).tasks[i].finished), true);
        }
    }
    loop {
        // Wait for the next tick: the handler will pick another runnable frame
        // and this one is never resumed, because it is marked finished.
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

fn all_finished() -> bool {
    let p = sched_ptr();
    let n = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*p).n)) };
    let mut i = 0;
    while i < n {
        let f = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*p).tasks[i].finished)) };
        if !f {
            return false;
        }
        i += 1;
    }
    n > 0
}

/// Start preemption and run until every task has finished.
pub fn start() {
    // Nothing may be reclaimed under a running task's feet: the floor is where
    // the arena stood when the first task was about to run.
    {
        let k = unsafe { &*core::ptr::addr_of!(crate::KERNEL) };
        k.heap.set_floor(k.heap.mark());
    }
        crate::trap::init();
    unsafe { crate::trap::SWITCH = Some(dispatch) };
    crate::trap::arm_timer(TICK);
    crate::trap::enable_timer_irq();


    // Run until every task is finished. Not `wfi`: a tick that interrupts the
    // `wfi` itself leaves sepc pointing at it, so the trampoline returns to the
    // same `wfi` and the boot context sleeps forever. Spinning, with interrupts
    // enabled, means the handler can always preempt this loop and the loop
    // itself decides when there is nothing left to do.
        loop {
        if all_finished() {
            break;
        }
        core::hint::spin_loop();
    }
        crate::trap::disable_timer_irq();
        // every task is finished: the whole arena may be reclaimed again
    {
        let k = unsafe { &*core::ptr::addr_of!(crate::KERNEL) };
        // Only the floor goes: rewinding here would free the cells the globals
        // and the root still point at, and the next form would read garbage.
        k.heap.set_floor(0);
    }
    }

/// Trap-handler entry point: pure function of the current scheduler state.
fn dispatch(sp: u64) -> u64 {
    sched().switch(sp)
}
