// eikon: a scheduler of contracts, not of priorities.
//
// A priority says "I am important". A contract says "I promised to finish within
// 5 ms". This scheduler runs tasks in earliest-deadline-first order and, after
// each one, compares what it promised with what it cost. A task that misses its
// deadline is counted and named - silence is the one thing a scheduler must not
// do about a broken promise.
//
// It is NOT preemptive: a task runs to completion. That is a real limitation and
// it is stated in ARCHITECTURE.md, not hidden behind the word "scheduler".

use crate::image::Kernel;
use crate::val::Val;

pub const MAX_TASKS: usize = 8;

#[derive(Clone, Copy)]
pub struct Task {
    pub name: &'static str,
    pub deadline_ms: u64,
    pub func: Val,
    pub took_ms: u64,
    pub missed: bool,
}

impl Task {
    pub const fn empty() -> Self {
        Task { name: "", deadline_ms: 0, func: Val::Nil, took_ms: 0, missed: false }
    }
}

pub struct Sched {
    pub tasks: [Task; MAX_TASKS],
    pub n: usize,
    pub misses: u64,
    pub runs: u64,
}

impl Sched {
    pub const fn new() -> Self {
        Sched { tasks: [Task::empty(); MAX_TASKS], n: 0, misses: 0, runs: 0 }
    }

    pub fn spawn(&mut self, name: &'static str, deadline_ms: u64, func: Val) -> Result<u64, &'static str> {
        if self.n == MAX_TASKS {
            return Err("ENOSPC: task table full");
        }
        self.tasks[self.n] = Task { name, deadline_ms, func, took_ms: 0, missed: false };
        self.n += 1;
        Ok(self.n as u64)
    }

    /// Earliest deadline first: index order of the run, by deadline.
    pub fn order(&self) -> [usize; MAX_TASKS] {
        let mut idx = [usize::MAX; MAX_TASKS];
        let mut i = 0;
        while i < self.n {
            idx[i] = i;
            i += 1;
        }
        // insertion sort: at most eight tasks, and stability matters for reading
        let mut i = 1;
        while i < self.n {
            let key = idx[i];
            let mut j = i;
            while j > 0 && self.tasks[idx[j - 1]].deadline_ms > self.tasks[key].deadline_ms {
                idx[j] = idx[j - 1];
                j -= 1;
            }
            idx[j] = key;
            i += 1;
        }
        idx
    }

    /// Run every task once, in deadline order, and account the promise.
    pub fn run_all(&mut self, k: &mut Kernel) -> Result<(), &'static str> {
        let order = self.order();
        let mut i = 0;
        while i < self.n {
            let t = order[i];
            let func = self.tasks[t].func;
            // the arena is rewound per task: a task's scratch is not the image
            let mark = k.heap.mark();
            let t0 = crate::serial::ticks();
            let res = crate::eval::apply(k, func, &[]);
            let dt = crate::serial::ticks().saturating_sub(t0);
            let _ = res;
            let took_ms = dt / 10_000; // 10 MHz timebase
            self.tasks[t].took_ms = took_ms;
            self.tasks[t].missed = took_ms > self.tasks[t].deadline_ms;
            if self.tasks[t].missed {
                self.misses += 1;
            }
            self.runs += 1;
            k.heap.rewind(mark);
            if let Err(e) = res {
                crate::serial::write_str("  task ");
                crate::serial::write_str(self.tasks[t].name);
                crate::serial::write_str(" failed: ");
                crate::serial::write_str(e);
                crate::serial::write_str("\n");
                return Err("EINVAL: a task failed");
            }
            i += 1;
        }
        Ok(())
    }
}
