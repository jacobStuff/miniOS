//! Global Descriptor Table and Task State Segment setup.
//!
//! The only reason we need a TSS here is to give the double-fault handler
//! its own known-good stack (via the Interrupt Stack Table) -- a double
//! fault can be caused by a kernel stack overflow, so handling it on the
//! same stack that just overflowed would trigger a silent triple fault
//! instead of a diagnosable panic.

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
            static STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            let stack_start = VirtAddr::from_ptr(&raw const STACK);
            stack_start + STACK_SIZE as u64
        };
        tss
    };
}

struct Selectors {
    code_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));
        (
            gdt,
            Selectors {
                code_selector,
                tss_selector,
            },
        )
    };
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
