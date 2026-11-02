//! Page-table access and a physical frame allocator built from the
//! bootloader's memory map.
//!
//! This requires the bootloader to have mapped *all* physical memory at a
//! known virtual offset (`BootInfo::physical_memory_offset`), which is off
//! by default and enabled via the `BootloaderConfig` in `main.rs`.

use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use spin::{Mutex, Once};
use x86_64::{
    registers::control::Cr3,
    structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB},
    PhysAddr, VirtAddr,
};

static PHYSICAL_MEMORY_OFFSET: Once<VirtAddr> = Once::new();
static FRAME_ALLOCATOR: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);

/// Must be called exactly once, before any other function in this module:
/// records the bootloader's physical-memory mapping offset and takes
/// ownership of the frame allocator built from its memory map. Returns an
/// `OffsetPageTable` over the *active* (bootloader-installed) page table,
/// for the heap to map itself into.
///
/// # Safety
/// `physical_memory_offset` must be the virtual address at which the
/// bootloader mapped all physical memory, and `memory_regions` must be
/// accurate (every `Usable` region must really be unused RAM).
pub unsafe fn init(
    physical_memory_offset: VirtAddr,
    memory_regions: &'static MemoryRegions,
) -> OffsetPageTable<'static> {
    PHYSICAL_MEMORY_OFFSET.call_once(|| physical_memory_offset);
    *FRAME_ALLOCATOR.lock() = Some(BootInfoFrameAllocator {
        memory_regions,
        next: 0,
    });
    unsafe { active_mapper() }
}

fn physical_memory_offset() -> VirtAddr {
    *PHYSICAL_MEMORY_OFFSET
        .get()
        .expect("memory::init() was never called")
}

/// Translates a physical address to the virtual address it's mapped at in
/// the bootloader's all-physical-memory mapping. Valid for any frame that
/// was reported `Usable`, regardless of which page table is currently
/// active -- that mapping is shared by every address space in this kernel
/// (see `new_address_space` in process.rs).
pub fn phys_to_virt(phys: PhysAddr) -> VirtAddr {
    physical_memory_offset() + phys.as_u64()
}

/// An `OffsetPageTable` over the page table that's active in CR3 *right
/// now*. Different callers at different times may get a view over
/// different underlying tables, if CR3 has been switched between calls
/// (see process.rs).
///
/// # Safety
/// Must not be called before `init()`, and the caller must not create two
/// live `OffsetPageTable`s over the same underlying table at once (each
/// holds an effectively-`&mut` view of it).
pub unsafe fn active_mapper() -> OffsetPageTable<'static> {
    let (frame, _) = Cr3::read();
    unsafe { mapper_for(frame) }
}

/// An `OffsetPageTable` over an arbitrary level-4 table, whether or not
/// it's the currently-active one. Used to build a new process's page
/// tables before switching CR3 to them.
///
/// # Safety
/// `frame` must contain a valid, zero-or-otherwise-sensibly-initialized
/// level 4 page table. As with `active_mapper`, callers must not alias.
pub unsafe fn mapper_for(frame: PhysFrame) -> OffsetPageTable<'static> {
    let virt = phys_to_virt(frame.start_address());
    let table: &'static mut PageTable = unsafe { &mut *virt.as_mut_ptr() };
    unsafe { OffsetPageTable::new(table, physical_memory_offset()) }
}

/// Hands out one physical frame from the bootloader's memory map.
pub fn allocate_frame() -> Option<PhysFrame> {
    FRAME_ALLOCATOR
        .lock()
        .as_mut()
        .expect("memory::init() was never called")
        .allocate_frame()
}

/// A `FrameAllocator` that hands out the usable frames reported in the
/// bootloader's memory map, one after another. It never reuses a frame
/// (no frames are ever freed -- processes don't get torn down, just like
/// nothing else in this kernel does yet).
struct BootInfoFrameAllocator {
    memory_regions: &'static MemoryRegions,
    next: usize,
}

impl BootInfoFrameAllocator {
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

/// A `FrameAllocator` wrapper around the global allocator above, for
/// passing to APIs (like `Mapper::map_to`) that want `&mut impl
/// FrameAllocator`.
pub struct GlobalFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        allocate_frame()
    }
}
