//! Global Descriptor Table and Task State Segment setup.
//!
//! The TSS gives us two things:
//! - an Interrupt Stack Table entry so the double-fault handler runs on its
//!   own known-good stack (a double fault can be caused by a kernel stack
//!   overflow, so handling it on the stack that just overflowed would
//!   trigger a silent triple fault instead of a diagnosable panic);
//! - `privilege_stack_table[0]` (RSP0), the stack the CPU switches to
//!   automatically whenever a ring 3 -> ring 0 transition happens (a
//!   syscall, or any exception/interrupt while user code is running). Ring
//!   3 code's own stack can't be trusted for handling its own faults.

use lazy_static::lazy_static;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

const STACK_SIZE: usize = 4096 * 5;

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();

        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            // `static mut`, not plain `static`: nothing in Rust ever writes
            // through this array, so LLVM is free to place a plain
            // immutable static in read-only memory -- and with CR0.WP set
            // (which the bootloader does), even a supervisor-mode hardware
            // push onto it then faults. `static mut` forces it into
            // writable .bss. `&raw const` (not `&STACK`) avoids the
            // static-mut-refs lint without going through a safe reference.
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
            VirtAddr::from_ptr(&raw const STACK) + STACK_SIZE as u64
        };

        tss.privilege_stack_table[0] = {
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
            VirtAddr::from_ptr(&raw const STACK) + STACK_SIZE as u64
        };

        tss
    };
}

struct Selectors {
    code_selector: SegmentSelector,
    tss_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));
        let user_code_selector = gdt.append(Descriptor::user_code_segment());
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        (
            gdt,
            Selectors {
                code_selector,
                tss_selector,
                user_code_selector,
                user_data_selector,
            },
        )
    };
}

/// The kernel code segment selector, needed to build the fake interrupt
/// frames that task.rs uses to bootstrap a new task's first context switch.
pub fn kernel_code_selector() -> SegmentSelector {
    GDT.1.code_selector
}

/// Ring-3 code/data selectors (already carry RPL=3), for building the
/// interrupt frame that takes a new process into user mode.
pub fn user_code_selector() -> SegmentSelector {
    GDT.1.user_code_selector
}

pub fn user_data_selector() -> SegmentSelector {
    GDT.1.user_data_selector
}

pub fn init() {
    use x86_64::instructions::segmentation::{Segment, CS, SS};
    use x86_64::instructions::tables::load_tss;
    use x86_64::structures::gdt::SegmentSelector;
    use x86_64::PrivilegeLevel;

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        load_tss(GDT.1.tss_selector);
        // The bootloader hands off with a nonzero SS selector from its own
        // (now-discarded) GDT. Our GDT reuses that same index for the TSS
        // descriptor, so leaving SS stale makes the next `iretq` fault with
        // a #GP when it tries to reload an invalid stack segment. A null
        // selector is valid for SS in 64-bit mode at CPL0, since
        // segmentation is otherwise unused.
        SS::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
    }
}
