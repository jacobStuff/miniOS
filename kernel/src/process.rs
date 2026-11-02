//! Ring-3 process support: a from-scratch page table per process, and a
//! syscall interface (`int 0x80`) for user code to ask the kernel to do
//! anything on its behalf -- it can't touch hardware or kernel memory
//! directly, by construction (nothing but its own code and stack is
//! mapped PRESENT + USER_ACCESSIBLE in its page table).
//!
//! This is deliberately a single, synchronous "run one process to
//! completion" operation (see `run_demo`), not integrated into the
//! preemptive scheduler in task.rs: mixing per-process page tables with
//! that scheduler's context switching would mean every switch also has to
//! switch CR3, which is a real next step but a substantial one on its own.
//! For now the timer is masked for the duration of the run, so nothing
//! else gets scheduled while a different address space is active.

use crate::gdt;
use crate::interrupts;
use crate::memory::{self, GlobalFrameAllocator};
use crate::userland;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{Mapper, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::VirtAddr;

const USER_CODE_ADDR: u64 = 0x400000;
const USER_STACK_TOP: u64 = 0x7FFF_F000;
const USER_STACK_PAGES: u64 = 4;
const PAGE_SIZE: u64 = 4096;

static mut EXIT_CODE: i64 = 0;

/// Builds a fresh address space: a new PML4 that shares every kernel
/// mapping (code, heap, the physical-memory window, the framebuffer --
/// everything the active PML4's entries 1-511 cover) but starts with
/// entry 0 (virtual addresses `0..512GiB`) empty, for the process's own
/// exclusive use.
fn new_address_space() -> PhysFrame {
    let new_frame = memory::allocate_frame().expect("out of physical memory for a new PML4");

    let kernel_frame = Cr3::read().0;
    let kernel_table: &PageTable =
        unsafe { &*memory::phys_to_virt(kernel_frame.start_address()).as_ptr::<PageTable>() };
    let new_table: &mut PageTable =
        unsafe { &mut *memory::phys_to_virt(new_frame.start_address()).as_mut_ptr::<PageTable>() };

    *new_table = kernel_table.clone();
    new_table[0].set_unused();

    new_frame
}

/// Allocates one fresh zeroed frame per 4 KiB chunk of `data` (padding the
/// last chunk with zeros) and maps them starting at `start` in the given
/// address space.
fn map_user_data(pml4_frame: PhysFrame, start: VirtAddr, data: &[u8], flags: PageTableFlags) {
    let mut mapper = unsafe { memory::mapper_for(pml4_frame) };
    let mut offset = 0usize;
    while offset < data.len() {
        let frame = memory::allocate_frame().expect("out of physical memory for user pages");
        let dest = unsafe {
            core::slice::from_raw_parts_mut(
                memory::phys_to_virt(frame.start_address()).as_mut_ptr::<u8>(),
                PAGE_SIZE as usize,
            )
        };
        let chunk_len = core::cmp::min(PAGE_SIZE as usize, data.len() - offset);
        dest[..chunk_len].copy_from_slice(&data[offset..offset + chunk_len]);
        dest[chunk_len..].fill(0);

        let page = Page::<Size4KiB>::containing_address(start + offset as u64);
        unsafe {
            mapper
                .map_to(page, frame, flags, &mut GlobalFrameAllocator)
                .expect("failed to map a user page")
                .flush();
        }
        offset += PAGE_SIZE as usize;
    }
}

/// Allocates and maps `num_pages` fresh zeroed frames starting at `start`.
fn map_user_zeroed(pml4_frame: PhysFrame, start: VirtAddr, num_pages: u64, flags: PageTableFlags) {
    let mut mapper = unsafe { memory::mapper_for(pml4_frame) };
    for i in 0..num_pages {
        let frame = memory::allocate_frame().expect("out of physical memory for user pages");
        unsafe {
            core::ptr::write_bytes(
                memory::phys_to_virt(frame.start_address()).as_mut_ptr::<u8>(),
                0,
                PAGE_SIZE as usize,
            );
        }

        let page = Page::<Size4KiB>::containing_address(start + i * PAGE_SIZE);
        unsafe {
            mapper
                .map_to(page, frame, flags, &mut GlobalFrameAllocator)
                .expect("failed to map a user stack page")
                .flush();
        }
    }
}

/// Builds a fresh address space, loads the demo program into it, runs it
/// in ring 3 to completion, and returns the exit code it passed to the
/// exit syscall.
pub fn run_demo() -> i64 {
    serial_println!("process: building a new address space for the demo program");
    let pml4_frame = new_address_space();

    let program = userland::demo_program_bytes();
    let user_flags =
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    map_user_data(pml4_frame, VirtAddr::new(USER_CODE_ADDR), program, user_flags);

    let stack_bottom = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;
    map_user_zeroed(
        pml4_frame,
        VirtAddr::new(stack_bottom),
        USER_STACK_PAGES,
        user_flags,
    );

    serial_println!(
        "process: mapped {} bytes of program at {:#x}, {}-page stack ending at {:#x}",
        program.len(),
        USER_CODE_ADDR,
        USER_STACK_PAGES,
        USER_STACK_TOP
    );

    // Nothing else may run while a different address space is active: the
    // scheduler's timer-driven switches know nothing about per-process page
    // tables, so a tick landing here would switch to another task while CR3
    // still points at this process's table.
    interrupts::mask_timer();

    let (original_frame, flags) = Cr3::read();
    unsafe { Cr3::write(pml4_frame, flags) };

    serial_println!("process: entering ring 3 at {:#x}", USER_CODE_ADDR);
    unsafe {
        enter_user_mode(
            USER_CODE_ADDR,
            USER_STACK_TOP,
            gdt::user_code_selector().0 as u64,
            gdt::user_data_selector().0 as u64,
        );
    }
    // Execution resumes here once the process calls the exit syscall --
    // not via a normal `ret` from `enter_user_mode`, but via
    // `longjmp_to_kernel` restoring the stack/registers this call had at
    // the moment it entered ring 3. See the comment above that function.
    //
    // That path never runs an `iretq` (it skips straight past the syscall
    // trampoline's normal exit), so RFLAGS is never restored -- and the
    // syscall handler runs with interrupts disabled, per the IDT gate's
    // defaults. Without this, everything after run_demo() returns would
    // silently run with interrupts permanently off.
    unsafe { Cr3::write(original_frame, flags) };
    interrupts::unmask_timer();
    x86_64::instructions::interrupts::enable();

    let exit_code = unsafe { EXIT_CODE };
    serial_println!("process: back in the kernel, exit code {exit_code}");
    exit_code
}

#[repr(C)]
struct SyscallRegs {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
}

const SYS_EXIT: u64 = 0;
const SYS_WRITE: u64 = 1;

#[unsafe(no_mangle)]
extern "C" fn syscall_dispatch(regs: *mut SyscallRegs) {
    let regs = unsafe { &mut *regs };
    match regs.rax {
        SYS_EXIT => {
            unsafe { EXIT_CODE = regs.rdi as i64 };
            unsafe { longjmp_to_kernel() }
        }
        SYS_WRITE => {
            // A real OS validates that `ptr..ptr+len` actually lies within
            // this process's own mapped, user-accessible memory before
            // trusting it. We skip that here: the only code that can ever
            // reach this syscall is our own hard-coded demo program.
            let ptr = regs.rdi as *const u8;
            let len = regs.rsi as usize;
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
            if let Ok(s) = core::str::from_utf8(bytes) {
                print!("{s}");
                serial_print!("{s}");
            }
            regs.rax = 0;
        }
        other => {
            serial_println!("process: unknown syscall number {other}");
            regs.rax = u64::MAX;
        }
    }
}

/// The IDT handler address for vector 0x80.
pub fn syscall_entry_addr() -> VirtAddr {
    VirtAddr::new(syscall_entry as *const () as u64)
}

unsafe extern "C" {
    fn enter_user_mode(entry: u64, user_stack_top: u64, user_cs: u64, user_ss: u64);
    fn longjmp_to_kernel() -> !;
    fn syscall_entry();
}

// `enter_user_mode` is a hand-written setjmp: it snapshots the callee-saved
// registers and RSP (exactly what a function call's caller is owed back
// on return, per the System V ABI) into KERNEL_CONTEXT, then builds an
// iretq frame and drops into ring 3. `longjmp_to_kernel` is its other
// half, called from the exit syscall: it restores those registers/RSP and
// `ret`s, landing right back after the original `call enter_user_mode` in
// run_demo as if that call had returned normally -- even though control
// actually arrived via a completely different path (a syscall from
// user-mode code, handled on the RSP0 stack, not this one). This is the
// same "every context looks like an interrupt frame" trick task.rs uses
// for task switching, just with a single fixed save slot instead of a
// whole scheduler.
core::arch::global_asm!(
    ".global enter_user_mode",
    "enter_user_mode:",
    "mov [rip + KERNEL_CONTEXT + 0], rsp",
    "mov [rip + KERNEL_CONTEXT + 8], rbx",
    "mov [rip + KERNEL_CONTEXT + 16], rbp",
    "mov [rip + KERNEL_CONTEXT + 24], r12",
    "mov [rip + KERNEL_CONTEXT + 32], r13",
    "mov [rip + KERNEL_CONTEXT + 40], r14",
    "mov [rip + KERNEL_CONTEXT + 48], r15",
    "push rcx", // user_ss
    "push rsi", // user_stack_top (rsp)
    "pushfq",
    "pop rax",
    "or rax, 0x200", // force IF=1: interrupts stay enabled once in ring 3
    "push rax",      // rflags
    "push rdx",      // user_cs
    "push rdi",      // entry (rip)
    "iretq",
    ".global longjmp_to_kernel",
    "longjmp_to_kernel:",
    "mov rbx, [rip + KERNEL_CONTEXT + 8]",
    "mov rbp, [rip + KERNEL_CONTEXT + 16]",
    "mov r12, [rip + KERNEL_CONTEXT + 24]",
    "mov r13, [rip + KERNEL_CONTEXT + 32]",
    "mov r14, [rip + KERNEL_CONTEXT + 40]",
    "mov r15, [rip + KERNEL_CONTEXT + 48]",
    "mov rsp, [rip + KERNEL_CONTEXT + 0]",
    "ret",
    ".global syscall_entry",
    "syscall_entry:",
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
    "call syscall_dispatch",
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

#[repr(C)]
struct KernelContext {
    rsp: u64,
    rbx: u64,
    rbp: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
}

#[unsafe(no_mangle)]
static mut KERNEL_CONTEXT: KernelContext = KernelContext {
    rsp: 0,
    rbx: 0,
    rbp: 0,
    r12: 0,
    r13: 0,
    r14: 0,
    r15: 0,
};
