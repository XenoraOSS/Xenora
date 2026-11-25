// SPDX-License-Identifier: GPL-3.0-or-later

//! Deterministic cooperative scheduler for guest threads.
//!
//! One host thread runs one guest thread at a time, in first-in, first-out
//! order, for a bounded number of instructions. The same program, quantum and
//! clock always interleave the same way. Guest threads never run concurrently,
//! so nothing here relies on a guest memory-ordering model.
//!
//! A service that has to wait returns [`ExitReason::GuestBlocked`] with RIP
//! still on its trampoline. The scheduler runs that call again when something it
//! may wait for has changed, oldest waiter first, until it completes.

use crate::cpu::{CpuState, ExitReason};
use crate::dbt::DbtEngine;
use crate::memory::GuestMemory;
use crate::services::ServicesState;
use crate::services::threads::{RunState, THREAD_RETURN};
use std::sync::Arc;

/// Default instructions a thread runs before the next one gets a turn.
pub const DEFAULT_QUANTUM: usize = 1024;

/// Why scheduling stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The main thread executed HLT, the synthetic process exit.
    Halted,
    /// Every thread ended.
    AllThreadsExited,
    /// No thread can run and no wait has a deadline.
    Deadlock {
        blocked: Vec<u32>,
        suspended: Vec<u32>,
    },
    BudgetExhausted,
    /// A thread stopped for a reason the guest cannot handle.
    Fault {
        thread: u32,
        reason: ExitReason,
        rip: u64,
    },
}

/// Result of one scheduled run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleReport {
    pub outcome: Outcome,
    /// Guest instructions and service calls, including repeated waits.
    pub instructions: usize,
    /// Turns given to threads.
    pub turns: usize,
    /// Largest number of guest threads alive at once.
    pub peak_threads: usize,
    /// Main thread state when it last stopped.
    pub main: CpuState,
}

pub struct Scheduler {
    state: Arc<ServicesState>,
    quantum: usize,
}

enum Turn {
    Continue,
    Stop(Outcome),
}

impl Scheduler {
    /// `quantum` is at least one instruction; a turn ends at the first block
    /// boundary that reaches it.
    pub fn new(state: Arc<ServicesState>, quantum: usize) -> Self {
        Self {
            state,
            quantum: quantum.max(1),
        }
    }

    /// Run `main` and every thread it creates. Only one scheduler may be
    /// attached to a service state at a time. Created threads still alive at
    /// the end are terminated with code 0 and reclaimed, so a run that had any
    /// cannot be continued.
    pub fn run(
        &self,
        engine: &mut DbtEngine,
        memory: &mut GuestMemory,
        main: &CpuState,
        budget: usize,
    ) -> Result<ScheduleReport, ExitReason> {
        let main_id = self.state.attach_scheduler(main)?;
        let mut report = ScheduleReport {
            outcome: Outcome::BudgetExhausted,
            instructions: 0,
            turns: 0,
            peak_threads: 1,
            main: main.clone(),
        };
        while report.instructions < budget {
            match self.turn(engine, memory, main_id, budget, &mut report) {
                Turn::Continue => {}
                Turn::Stop(outcome) => {
                    report.outcome = outcome;
                    break;
                }
            }
        }
        if report.outcome == Outcome::BudgetExhausted && self.all_ended() {
            report.outcome = Outcome::AllThreadsExited;
        }
        let continues = report.outcome == Outcome::BudgetExhausted;
        self.state
            .detach_scheduler(memory, continues.then_some(&report.main));
        Ok(report)
    }

    fn turn(
        &self,
        engine: &mut DbtEngine,
        memory: &mut GuestMemory,
        main_id: u32,
        budget: usize,
        report: &mut ScheduleReport,
    ) -> Turn {
        if let Some(outcome) = self.wake(engine, memory, main_id, budget, report) {
            return Turn::Stop(outcome);
        }
        let Some(id) = self.next_ready() else {
            // A spent budget may have left waiters untried, and must not sleep.
            if report.instructions >= budget {
                return Turn::Continue;
            }
            return self.idle();
        };
        report.turns += 1;
        let turn = self.take_credit(id).unwrap_or(self.quantum);
        let limit = turn.min(budget.saturating_sub(report.instructions));
        if limit == 0 {
            self.requeue(id);
            return Turn::Continue;
        }
        let before = report.instructions;
        let stop = self.run_thread(engine, memory, id, main_id, limit, report);
        match stop {
            Stop::Outcome(outcome) => Turn::Stop(outcome),
            Stop::Ended => Turn::Continue,
            Stop::Blocked => {
                let mut table = self.state.threads.lock().expect("guest thread table");
                let sequence = table.ticket();
                if let Some(thread) = table.threads.get_mut(&id) {
                    thread.run = RunState::Blocked(sequence);
                }
                Turn::Continue
            }
            Stop::Preempted { front } => {
                // Signaling keeps the caller's turn, but never renews it.
                let left = turn.saturating_sub(report.instructions - before);
                let mut table = self.state.threads.lock().expect("guest thread table");
                if let Some(thread) = table.threads.get_mut(&id) {
                    if front && left > 0 {
                        thread.credit = Some(left);
                        table.queue.push_front(id);
                    } else {
                        table.queue.push_back(id);
                    }
                }
                Turn::Continue
            }
        }
    }

    fn all_ended(&self) -> bool {
        let table = self.state.threads.lock().expect("guest thread table");
        table.threads.is_empty()
    }

    /// Put back a thread that got no turn.
    fn requeue(&self, id: u32) {
        let mut table = self.state.threads.lock().expect("guest thread table");
        table.queue.push_front(id);
    }

    fn take_credit(&self, id: u32) -> Option<usize> {
        let mut table = self.state.threads.lock().expect("guest thread table");
        table.threads.get_mut(&id)?.credit.take()
    }

    /// Remove and return the first thread that can run.
    fn next_ready(&self) -> Option<u32> {
        let mut table = self.state.threads.lock().expect("guest thread table");
        let position = table
            .queue
            .iter()
            .position(|id| table.threads.get(id).is_some_and(|t| t.suspend == 0))?;
        table.queue.remove(position)
    }

    /// Run the pending call of every blocked thread, oldest first. A thread that
    /// ends with its call may be what an older one waits for, so that repeats
    /// the pass.
    fn wake(
        &self,
        engine: &mut DbtEngine,
        memory: &mut GuestMemory,
        main_id: u32,
        budget: usize,
        report: &mut ScheduleReport,
    ) -> Option<Outcome> {
        loop {
            let mut ended = false;
            for id in self.blocked(report) {
                if report.instructions >= budget {
                    return None;
                }
                match self.run_thread(engine, memory, id, main_id, 1, report) {
                    Stop::Outcome(outcome) => return Some(outcome),
                    Stop::Blocked => {}
                    Stop::Ended => ended = true,
                    Stop::Preempted { .. } => {
                        let mut table = self.state.threads.lock().expect("guest thread table");
                        if let Some(thread) = table.threads.get_mut(&id) {
                            thread.run = RunState::Ready;
                            table.queue.push_back(id);
                        }
                    }
                }
            }
            if !ended {
                return None;
            }
        }
    }

    /// Blocked threads that are not suspended, oldest first.
    fn blocked(&self, report: &mut ScheduleReport) -> Vec<u32> {
        let table = self.state.threads.lock().expect("guest thread table");
        report.peak_threads = report.peak_threads.max(table.threads.len());
        let mut blocked: Vec<(u64, u32)> = table
            .threads
            .iter()
            .filter(|(_, thread)| thread.suspend == 0)
            .filter_map(|(&id, thread)| match thread.run {
                RunState::Blocked(sequence) => Some((sequence, id)),
                RunState::Ready => None,
            })
            .collect();
        blocked.sort_unstable();
        blocked.into_iter().map(|(_, id)| id).collect()
    }

    /// Nothing can run: advance to the next deadline or stop.
    fn idle(&self) -> Turn {
        let table = self.state.threads.lock().expect("guest thread table");
        if table.threads.is_empty() {
            return Turn::Stop(Outcome::AllThreadsExited);
        }
        if let Some(deadline) = table.next_deadline() {
            drop(table);
            let now = self.state.clock.performance_count();
            self.state.clock.sleep(deadline.saturating_sub(now));
            return Turn::Continue;
        }
        let mut blocked = Vec::new();
        let mut suspended = Vec::new();
        for (&id, thread) in &table.threads {
            if thread.suspend > 0 {
                suspended.push(id);
            } else {
                blocked.push(id);
            }
        }
        Turn::Stop(Outcome::Deadlock { blocked, suspended })
    }

    fn run_thread(
        &self,
        engine: &mut DbtEngine,
        memory: &mut GuestMemory,
        id: u32,
        main_id: u32,
        limit: usize,
        report: &mut ScheduleReport,
    ) -> Stop {
        let cpu = {
            let mut table = self.state.threads.lock().expect("guest thread table");
            table.threads.get_mut(&id).and_then(|t| t.cpu.take())
        };
        let Some(mut cpu) = cpu else {
            return Stop::Ended;
        };
        self.state.hooks.take_switch();
        let result = engine.run_slice(&mut cpu, memory, limit);
        // The engine does not count a call that stops it.
        let stopped = matches!(
            result.exit_reason,
            ExitReason::GuestYield | ExitReason::GuestBlocked | ExitReason::GuestThreadExited
        );
        report.instructions += result.instructions_executed + usize::from(stopped);
        let rip = cpu.rip;
        let returned = (rip == THREAD_RETURN && id != main_id)
            .then(|| cpu.get(crate::cpu::Register::Rax) as u32);
        if id == main_id {
            report.main = cpu.clone();
        }
        let exit = {
            let mut table = self.state.threads.lock().expect("guest thread table");
            let Some(thread) = table.threads.get_mut(&id) else {
                return Stop::Ended;
            };
            thread.cpu = Some(cpu);
            thread.exit
        };
        if let Some(code) = exit.or(returned) {
            self.state.end_thread(id, code, memory);
            return Stop::Ended;
        }
        match result.exit_reason {
            ExitReason::StepLimitExceeded => Stop::Preempted { front: false },
            ExitReason::GuestYield => Stop::Preempted {
                front: !self.state.hooks.take_switch(),
            },
            ExitReason::GuestBlocked => Stop::Blocked,
            ExitReason::Halt if id == main_id => Stop::Outcome(Outcome::Halted),
            reason => Stop::Outcome(Outcome::Fault {
                thread: id,
                reason,
                rip,
            }),
        }
    }
}

enum Stop {
    Blocked,
    Ended,
    Preempted { front: bool },
    Outcome(Outcome),
}
