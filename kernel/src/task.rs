//! Preemptive kernel-thread scheduler.
//!
//! All tasks run in ring 0 and share one address space (no process
//! isolation) -- this is "multitasking" in the sense of independently
//! scheduled, preemptible streams of execution, not full process support.
//!
//! The trick that makes preemption possible: every context, when it is not
//! the one currently running, sits on its own stack formatted to look
//! *exactly* like a CPU just delivered a timer interrupt into it (the same
//! 5-word interrupt frame plus the 15 general-purpose registers our own
//! trampoline pushes). Switching to a task is then just "point RSP at its
//! stack and run the normal interrupt-return path" -- the CPU/trampoline
//! can't tell a freshly-created task from one that was genuinely preempted
//! mid-execution, which is exactly what lets the same code path create a
//! task's very first run and resume one that already ran before.

use crate::gdt;
use crate::interrupts::{PICS, PIC_1_OFFSET, TICKS};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use core::mem::size_of;
use core::sync::atomic::Ordering;
use spin::Mutex;
use x86_64::VirtAddr;

const STACK_SIZE: usize = 16 * 1024;

/// Mirrors exactly what the CPU pushes on interrupt entry in 64-bit long
/// mode (always all 5 words, regardless of whether the privilege level
/// changes).
#[repr(C)]
struct InterruptFrame {
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

/// Number of general-purpose registers the asm trampoline below pushes.
const NUM_SAVED_GP_REGS: usize = 15;

struct SavedContext {
    rsp: u64,
    /// Keeps the task's stack allocation alive. `None` for the original
    /// boot stack (kernel_main's own thread), which we don't own.
    _stack: Option<Box<[u8]>>,
}

struct Scheduler {
    running: Option<SavedContext>,
    ready: VecDeque<SavedContext>,
}

static SCHEDULER: Mutex<Option<Scheduler>> = Mutex::new(None);

pub fn init() {
    *SCHEDULER.lock() = Some(Scheduler {
        running: None,
        ready: VecDeque::new(),
    });
}

/// Returns the IDT handler address to install for the timer vector. Must be
/// used in place of a normal `extern "x86-interrupt"` handler, since the
/// whole scheme relies on controlling the exact register save/restore
/// sequence by hand.
pub fn timer_entry_addr() -> VirtAddr {
    VirtAddr::new(task_timer_entry as *const () as u64)
}

/// Spawns a new task. `entry` must never return (a task that falls off the
/// end of its function has nothing sensible to resume into).
pub fn spawn(entry: extern "C" fn() -> !) {
    let mut stack = alloc::vec![0u8; STACK_SIZE].into_boxed_slice();
    let stack_bottom = stack.as_mut_ptr() as u64;
    let stack_top = (stack_bottom + STACK_SIZE as u64) & !0xf;

    let frame_addr = stack_top - size_of::<InterruptFrame>() as u64;
    unsafe {
        (frame_addr as *mut InterruptFrame).write(InterruptFrame {
            rip: entry as *const () as u64,
            cs: gdt::kernel_code_selector().0 as u64,
            rflags: 0x202, // IF=1 (interrupts enabled once this task runs), bit 1 reserved=1
            rsp: stack_top,
            ss: 0,
        });
    }

    // The trampoline pops 15 GP registers before the CPU's own iretq pops
    // the frame above. A freshly-spawned task has never saved real
    // registers, so zero-fill that region -- their values don't matter,
    // execution starts at `entry` with no assumptions about register state.
    let regs_addr = frame_addr - (NUM_SAVED_GP_REGS * 8) as u64;
    unsafe {
        core::ptr::write_bytes(regs_addr as *mut u8, 0, NUM_SAVED_GP_REGS * 8);
    }

    let ctx = SavedContext {
        rsp: regs_addr,
        _stack: Some(stack),
    };

    SCHEDULER
        .lock()
        .as_mut()
        .expect("task::init() must run before task::spawn()")
        .ready
        .push_back(ctx);
}

/// Called from `task_timer_entry` with the interrupted context's current
/// RSP (pointing at the top of its saved-registers block); returns the RSP
/// to switch to. Runs with interrupts disabled (standard IDT interrupt
/// gate), so this never races with itself.
#[unsafe(no_mangle)]
extern "C" fn schedule(incoming_rsp: u64) -> u64 {
    TICKS.fetch_add(1, Ordering::Relaxed);
    unsafe {
        PICS.lock().notify_end_of_interrupt(PIC_1_OFFSET);
    }

    let mut guard = SCHEDULER.lock();
    let sched = guard.as_mut().expect("task::init() was never called");

    match sched.running.take() {
        Some(mut prev) => {
            prev.rsp = incoming_rsp;
            sched.ready.push_back(prev);
        }
        None => {
            // First tick ever: `incoming_rsp` belongs to the original boot
            // thread (kernel_main), which isn't tracked as a task yet.
            sched.ready.push_back(SavedContext {
                rsp: incoming_rsp,
                _stack: None,
            });
        }
    }

    let next = sched
        .ready
        .pop_front()
        .expect("just pushed a context, so the ready queue can't be empty");
    let next_rsp = next.rsp;
    sched.running = Some(next);
    next_rsp
}

core::arch::global_asm!(
    ".global task_timer_entry",
    "task_timer_entry:",
    "push rax",
    "push rbx",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push rbp",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov rdi, rsp",
    "call schedule",
    "mov rsp, rax",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rbp",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rbx",
    "pop rax",
    "iretq",
);

unsafe extern "C" {
    fn task_timer_entry();
}
