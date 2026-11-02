//! Page-table access and a physical frame allocator built from the
//! bootloader's memory map.
//!
//! This requires the bootloader to have mapped *all* physical memory at a
//! known virtual offset (`BootInfo::physical_memory_offset`), which is off
//! by default and enabled via the `BootloaderConfig` in `main.rs`.

use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use x86_64::{
    structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB},
    PhysAddr, VirtAddr,
};

/// Builds an `OffsetPageTable` from the active (bootloader-installed) level
/// 4 page table.
///
/// # Safety
/// `physical_memory_offset` must be the virtual address at which the
/// bootloader mapped all physical memory, and this must be called only
/// once (it hands out a `&'static mut` to the page table).
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;

    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    unsafe { &mut *page_table_ptr }
}

/// A `FrameAllocator` that hands out the usable frames reported in the
/// bootloader's memory map, one after another. It never reuses a frame, so
/// it's only good for the one-time setup allocations we need (page tables,
/// the heap) -- not a general-purpose allocator.
pub struct BootInfoFrameAllocator {
    memory_regions: &'static MemoryRegions,
    next: usize,
}

impl BootInfoFrameAllocator {
    /// # Safety
    /// `memory_regions` must be accurate: every region marked `Usable` must
    /// actually be unused RAM.
    pub unsafe fn init(memory_regions: &'static MemoryRegions) -> Self {
        BootInfoFrameAllocator {
            memory_regions,
            next: 0,
        }
    }

    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        self.memory_regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .flat_map(|r| r.start..r.end)
            .step_by(4096)
            .map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}
