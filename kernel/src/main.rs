#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};

#[macro_use]
pub mod framebuffer;
#[macro_use]
pub mod serial;
pub mod allocator;
pub mod ata;
pub mod fat16;
pub mod fs;
pub mod gdt;
pub mod interrupts;
pub mod memory;
pub mod process;
pub mod qemu;
pub mod shell;
pub mod task;
pub mod userland;

use qemu::{exit_qemu, QemuExitCode};

/// The default bootloader config does *not* map all physical memory, since
/// most kernels don't need it. We do, to walk/modify page tables and hand
/// out physical frames for the heap.
pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // Interrupts stay off until init() below has a GDT/TSS (for the
    // double-fault IST stack), an IDT, and a remapped PIC all in place.
    x86_64::instructions::interrupts::disable();

    serial_println!("miniOS booting (serial)...");

    if let Some(fb) = boot_info.framebuffer.as_mut() {
        framebuffer::init(fb);
    }

    serial_println!("memory regions: {}", boot_info.memory_regions.len());

    println!("miniOS booting...");
    println!("Hello, World!");

    // Heap must be up before interrupts are enabled: once the timer starts
    // firing, every tick runs the scheduler, and even its first no-op
    // `VecDeque::push_back` needs to grow the queue's backing allocation.
    let physical_memory_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("bootloader did not map physical memory (check BOOTLOADER_CONFIG)");
    let physical_memory_offset = x86_64::VirtAddr::new(physical_memory_offset);

    let mut mapper = unsafe { memory::init(physical_memory_offset, &boot_info.memory_regions) };

    allocator::init_heap(&mut mapper, &mut memory::GlobalFrameAllocator)
        .expect("heap initialization failed");
    serial_println!("heap mapped at {:#x}, {} bytes", allocator::HEAP_START, allocator::HEAP_SIZE);

    // Exercise the allocator: a Box (single heap allocation) and a Vec that
    // grows past its initial capacity (multiple allocations/reallocations).
    let boxed = Box::new(41);
    serial_println!("boxed value at {:p}: {}", boxed, *boxed);

    let mut vec = Vec::new();
    for i in 0..500 {
        vec.push(i);
    }
    serial_println!("vec len={} sum={}", vec.len(), vec.iter().sum::<i32>());
    assert_eq!(vec.iter().sum::<i32>(), (0..500).sum());

    println!("Heap allocator online.");

    // The IDT's timer entry points at task::task_timer_entry, which calls
    // into the scheduler on every tick -- so the scheduler must exist
    // before interrupts::init() enables interrupts, or the first tick
    // panics on an uninitialized SCHEDULER.
    task::init();
    interrupts::init();
    serial_println!("GDT/IDT/PIC initialized, interrupts enabled");

    // Deliberately trigger a breakpoint exception to prove the IDT handler
    // runs and execution resumes afterwards, instead of faulting.
    x86_64::instructions::interrupts::int3();
    serial_println!("resumed after int3 -- breakpoint handler worked");

    println!("Interrupts online.");

    task::spawn(demo_task_a);
    task::spawn(demo_task_b);
    task::spawn(demo_task_c);
    serial_println!("spawned 3 demo tasks, waiting for them to interleave...");

    // Each demo task bumps STEPS once per loop iteration; once enough steps
    // have happened across all three, we've proven real preemptive
    // round-robin scheduling (not just one task hogging the CPU) and can
    // report success. kernel_main's own thread is itself just another
    // context in the same round-robin from here on.
    while STEPS.load(Ordering::Relaxed) < 15 {
        x86_64::instructions::hlt();
    }
    serial_println!(
        "multitasking demo complete after {} steps, {} timer ticks",
        STEPS.load(Ordering::Relaxed),
        interrupts::tick_count()
    );
    println!("Multitasking online.");

    task::spawn(shell::run);
    serial_println!("shell task spawned, handing off to it");

    // The shell task handles everything from here, including calling
    // exit_qemu() on an "exit" command. kernel_main's own thread still
    // takes its turn in the round-robin, but has nothing left to do.
    loop {
        x86_64::instructions::hlt();
    }
}

/// Total loop iterations across all demo tasks; kernel_main polls this to
/// know when it's seen enough interleaved progress to call the
/// multitasking demo verified.
static STEPS: AtomicU64 = AtomicU64::new(0);

macro_rules! demo_task {
    ($name:ident, $label:literal) => {
        extern "C" fn $name() -> ! {
            let mut n: u64 = 0;
            loop {
                n += 1;
                if n <= 5 {
                    serial_println!("[{}] step {}", $label, n);
                    STEPS.fetch_add(1, Ordering::Relaxed);
                }
                // Done proving interleaving; quiesce rather than spam the
                // log forever once the shell takes over.
                x86_64::instructions::hlt();
            }
        }
    };
}

demo_task!(demo_task_a, "task-a");
demo_task!(demo_task_b, "task-b");
demo_task!(demo_task_c, "task-c");

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    serial_println!("PANIC: {}", info);
    exit_qemu(QemuExitCode::Failed);
}
