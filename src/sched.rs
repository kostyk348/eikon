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

use crate::image::Kernel;
use crate::val::Val;

/// One millisecond at the 10 MHz timebase: preempt often enough to see it.
pub const TICK: u64 = 10_000;
pub const MAX_TASKS: usize = 4;
/// Deep enough for `fib 17` through the interpreter: each recursive apply()
/// frame carries its argument array on the stack, so 32 KiB overflowed quietly
/// into the scheduler that lives next to the stacks in .bss. The canary below
/// makes that failure loud instead of mysterious.
/// One megabyte per task. The interpreter is one recursively inlined function:
/// once the optimiser folds eval/apply/eval_body together, a single `fib 17`
/// level costs kilobytes, and 128 KiB was not enough - the task ran off the
/// bottom of its stack into the heap and corrupted the values it was reading.
pub const STACK: usize = 1024 * 1024;

pub const POLICY_KEEP: u8 = 0;
pub const POLICY_CANCEL: u8 = 1;
pub const POLICY_RETRY: u8 = 2;

/// Set what happens to a task that misses its deadline. Returns the old policy.
/// Start a fresh batch: the task table is empty again, and so are the books.
/// The policy is deliberately kept - it is a decision about the machine, not
/// about one run.
pub fn reset() {
    let s = sched();
    s.tasks = [Task::empty(); MAX_TASKS];
    s.n = 0;
    s.cur = -1;
    s.idle_sp = 0;
    s.ticks = 0;
    s.preemptions = 0;
    s.misses = 0;
    s.failed = false;
    s.n_started = 0;
    s.n_finished = 0;
    s.start_order = [-1; MAX_TASKS];
    s.finish_order = [-1; MAX_TASKS];
    s.by_deadline = 0;
    s.cancelled = 0;
    s.retries = 0;
}

pub fn set_policy(p: u8) -> u8 {
    let s = sched();
    let old = s.policy;
    s.policy = p;
    old
}
const CANARY: u64 = 0xfeed_face_dead_beef;

#[derive(Clone, Copy)]
pub struct Task {
    /// Copied, not borrowed: a task outlives the arena it was named from.
    name: [u8; 16],
    name_len: u8,
    pub deadline_ms: u64,
    pub func: Val,
    pub sp: u64,
    /// Tick when the task was created: its deadline counts from here until it
    /// first runs, and from the first run after that.
    pub spawn_tick: u64,
    pub started: u64,
    pub slices: u64,
    pub finished: bool,
    pub missed: bool,
    pub took_ms: u64,
    /// The budget the task is judged against right now. A retry raises it.
    pub budget_ms: u64,
    pub cancelled: bool,
    pub retried: bool,
    pub runs: u64,
}

impl Task {
    pub const fn empty() -> Self {
        Task {
            name: [0; 16],
            name_len: 0,
            deadline_ms: 0,
            func: Val::Nil,
            sp: 0,
            spawn_tick: 0,
            started: 0,
            slices: 0,
            finished: false,
            missed: false,
            took_ms: 0,
            budget_ms: 0,
            cancelled: false,
            retried: false,
            runs: 0,
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
    /// Who ran first, and who finished first: EDF is a claim about order, so
    /// the order has to be observable.
    pub start_order: [i8; MAX_TASKS],
    pub n_started: usize,
    pub finish_order: [i8; MAX_TASKS],
    pub n_finished: usize,
    /// Times a task that could still make its deadline was running and the
    /// scheduler handed the CPU to an even tighter one.
    pub by_deadline: u64,
    /// What to do with a task that missed: 0 keep (it still runs), 1 cancel,
    /// 2 retry once with a bigger budget.
    pub policy: u8,
    pub cancelled: u64,
    pub retries: u64,
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
            start_order: [-1; MAX_TASKS],
            n_started: 0,
            finish_order: [-1; MAX_TASKS],
            n_finished: 0,
            by_deadline: 0,
            policy: 0,
            cancelled: 0,
            retries: 0,
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
            core::ptr::write_volatile(base[i].0.as_mut_ptr() as *mut u64, CANARY);
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
            spawn_tick: crate::trap::now(),
            budget_ms: deadline_ms,
            ..Task::empty()
        };
        self.n += 1;
        Ok((i + 1) as u64)
    }

    /// When this task must be done, in ticks since boot. A task that has not
    /// run yet is judged from the moment it was created; one that has run, from
    /// the moment it was given the CPU.
    fn abs_deadline(&self, i: usize) -> u64 {
        let t = &self.tasks[i];
        let base = if t.started != 0 { t.started } else { t.spawn_tick };
        base.saturating_add(t.budget_ms.saturating_mul(10_000))
    }

    /// Earliest deadline first - with the part that makes it a *policy* and not
    /// a sort: a task whose deadline has already passed no longer competes with
    /// tasks that can still make theirs. The late one still runs (nothing is
    /// thrown away), but only when nothing feasible is left.
    fn pick(&self, now: u64) -> i32 {
        if self.n == 0 || self.n > MAX_TASKS {
            return -1;
        }
        let mut live: i32 = -1;
        let mut live_dl = u64::MAX;
        let mut late: i32 = -1;
        let mut late_dl = u64::MAX;
        let mut i = 0;
        while i < self.n {
            if !self.tasks[i].finished && !self.tasks[i].cancelled {
                let dl = self.abs_deadline(i);
                if now <= dl {
                    if dl < live_dl {
                        live_dl = dl;
                        live = i as i32;
                    }
                } else if dl < late_dl {
                    late_dl = dl;
                    late = i as i32;
                }
            }
            i += 1;
        }
        if live >= 0 {
            live
        } else {
            late
        }
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

        let now = crate::trap::now();

        // A broken contract is a decision, not a statistic. At the tick where
        // the deadline has passed: cancel the task, or give it one retry with a
        // bigger budget. "keep" (the default) only counts it and lets it run.
        if self.cur >= 0 {
            let i = self.cur as usize;
            let t = self.tasks[i];
            if !t.finished && !t.cancelled && now > self.abs_deadline(i) {
                if self.policy == POLICY_CANCEL {
                    self.tasks[i].cancelled = true;
                    self.cancelled += 1;
                } else if self.policy == POLICY_RETRY && !t.retried {
                    #[allow(function_casts_as_integer)]
                    // fresh frame on a fresh stack top: the old context is
                    // abandoned, so one stack per task is still enough
                    let entry: usize = task_entry as usize;
                    let sp = crate::trap::new_task_frame(stack_top(i), entry);
                    self.tasks[i].sp = sp as u64;
                    self.tasks[i].retried = true;
                    self.tasks[i].budget_ms = t.budget_ms.saturating_mul(8);
                    self.tasks[i].started = 0;
                    self.tasks[i].spawn_tick = now;
                    self.tasks[i].slices = 0;
                    self.retries += 1;
                }
            }
        }

        let next = self.pick(now);
        if next < 0 {
            self.cur = -1;
            return self.idle_sp;
        }
        if self.cur >= 0 && self.cur != next {
            self.by_deadline += 1;
        }
        self.cur = next;
        let ni = next as usize;
        if self.tasks[ni].started == 0 {
            self.tasks[ni].started = now;
            if self.n_started < MAX_TASKS {
                self.start_order[self.n_started] = next as i8;
                self.n_started += 1;
            }
        }
        self.tasks[ni].sp
    }
}

static mut SCHED: Sched = Sched::new();
/// 16-byte aligned: the trap frame offset and Rust's own aligned locals both
/// assume it, and a u8 array only guarantees one byte.
#[repr(align(16))]
struct Stack([u8; STACK]);

static mut STACKS: [Stack; MAX_TASKS] = [const { Stack([0; STACK]) }; MAX_TASKS];

/// Did any task's stack overflow? Checked after the run, so an overflow is a
/// named failure and not a mystery panic somewhere else in .bss.
pub fn stacks_intact() -> bool {
    let mut i = 0;
    let base = unsafe { &mut *core::ptr::addr_of_mut!(STACKS) };
    let n = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*sched_ptr()).n)) };
    while i < n {
        let magic = unsafe { core::ptr::read_volatile(base[i].0.as_mut_ptr() as *const u64) };
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
    let s: &'static mut [Stack; MAX_TASKS] = unsafe { &mut *core::ptr::addr_of_mut!(STACKS) };
    let base = s[i].0.as_mut_ptr() as usize;
    (base, base + STACK)
}

fn stack_top(i: usize) -> usize {
    let s: &'static mut [Stack; MAX_TASKS] = unsafe { &mut *core::ptr::addr_of_mut!(STACKS) };
    s[i].0.as_mut_ptr() as usize + STACK - 16
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
        unsafe {
            let r = core::ptr::read_volatile(core::ptr::addr_of!((*p).tasks[i].runs));
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).tasks[i].runs), r + 1);
        }
        let k = unsafe { &*core::ptr::addr_of!(crate::KERNEL) };
        let res = crate::eval::apply(k, func, &[]);
        let ok = res.is_ok();
        if let Err(e) = res {
            crate::serial::write_str("\ntask ");
            crate::serial::write_num(i as i64);
            crate::serial::write_str(" failed: ");
            crate::serial::write_str(e);
            crate::serial::write_str("\n");
        }
        let took = crate::trap::now().saturating_sub(started) / 10_000;

        let p = sched_ptr();
        unsafe {
            // judged against the budget in force, which a retry may have raised
            let deadline = core::ptr::read_volatile(core::ptr::addr_of!((*p).tasks[i].budget_ms));
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
            let nf = core::ptr::read_volatile(core::ptr::addr_of!((*p).n_finished));
            if nf < MAX_TASKS {
                core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).finish_order[nf]), i as i8);
                core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).n_finished), nf + 1);
            }
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
        let c = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*p).tasks[i].cancelled)) };
        if !f && !c {
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

    // Run until every task is finished or cancelled. Not `wfi`: a tick that
    // interrupts the `wfi` itself leaves sepc pointing at it, so the trampoline
    // returns to the same `wfi` and the boot context sleeps forever.
    loop {
        if all_finished() {
            break;
        }
        core::hint::spin_loop();
    }
    crate::trap::disable_timer_irq();

    {
        let k = unsafe { &*core::ptr::addr_of!(crate::KERNEL) };
        // The outcome becomes a value in the generation chain: a broken promise
        // that only lives in a counter is a promise nobody can audit.
        let _ = commit_report(k);
        // Only the floor goes: rewinding here would free the cells the globals
        // and the root still point at, and the next form would read garbage.
        k.heap.set_floor(0);
    }
}

fn mk_list(k: &Kernel, items: &[Val]) -> Result<Val, &'static str> {
    let mut out = Val::Nil;
    let mut i = items.len();
    while i > 0 {
        i -= 1;
        out = k.heap.cons(items[i], out)?;
    }
    Ok(out)
}

/// `(report (t <i> <verdict> <promised> <took> <slices>) ...)`, one row per
/// task, newest generation. Verdicts are `kept`, `missed`, `cancelled` or
/// `retried`.
fn commit_report(k: &Kernel) -> Result<u64, &'static str> {
    let tag = k.heap.intern("report")?;
    let t_sym = k.heap.intern("t")?;
    let s = sched();
    let mut items = Val::Nil;
    let mut i = s.n;
    while i > 0 {
        i -= 1;
        let t = s.tasks[i];
        let verdict = if t.cancelled {
            "cancelled"
        } else if t.missed {
            "missed"
        } else if t.retried {
            "retried"
        } else {
            "kept"
        };
        let v = k.heap.intern(verdict)?;
        let row = mk_list(
            k,
            &[
                Val::Sym(t_sym),
                Val::Int(i as i64),
                Val::Sym(v),
                Val::Int(t.budget_ms as i64),
                Val::Int(t.took_ms as i64),
                Val::Int(t.slices as i64),
            ],
        )?;
        items = k.heap.cons(row, items)?;
    }
    let form = k.heap.cons(Val::Sym(tag), items)?;
    k.img.commit(form)
}

/// Trap-handler entry point: pure function of the current scheduler state.
fn dispatch(sp: u64) -> u64 {
    sched().switch(sp)
}
