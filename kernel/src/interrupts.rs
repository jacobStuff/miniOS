//! IDT setup: CPU exception handlers plus the legacy 8259 PIC and the two
//! hardware interrupts (timer, keyboard) we handle for now.

use crate::gdt;
use crate::qemu::{exit_qemu, QemuExitCode};
use lazy_static::lazy_static;
use core::sync::atomic::{AtomicU64, Ordering};
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

pub(crate) static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn tick_count() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt.general_protection_fault
            .set_handler_fn(general_protection_fault_handler);
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        // The timer vector doesn't get a normal `extern "x86-interrupt"`
        // handler: task::task_timer_entry is a hand-written asm trampoline
        // that drives preemptive task switching. See task.rs for why.
        unsafe {
            idt[InterruptIndex::Timer.as_u8()].set_handler_addr(crate::task::timer_entry_addr());
        }
        idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);

        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

/// Brings up the GDT/TSS, IDT, remaps the PIC off the CPU exception vectors,
/// and finally enables maskable interrupts. Order matters: the IDT must be
/// loaded and the PIC remapped *before* interrupts are turned on, or a timer
/// tick could fire into an unconfigured/overlapping vector.
pub fn init() {
    gdt::init();
    init_idt();
    unsafe {
        let mut pics = PICS.lock();
        pics.initialize();
        // `initialize()` preserves whatever mask state it inherited, which
        // may have the keyboard line masked depending on what the
        // bootloader left behind. We only have handlers for timer (IRQ0)
        // and keyboard (IRQ1) so far, so explicitly unmask just those two
        // and leave everything else masked.
        let masks = pics.read_masks();
        pics.write_masks(masks[0] & !0b11, masks[1]);
    }
    x86_64::instructions::interrupts::enable();
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    serial_println!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
    exit_qemu(QemuExitCode::Failed);
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    serial_println!("EXCEPTION: PAGE FAULT");
    serial_println!("Accessed Address: {:?}", Cr2::read());
    serial_println!("Error Code: {:?}", error_code);
    serial_println!("{:#?}", stack_frame);
    exit_qemu(QemuExitCode::Failed);
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!(
        "EXCEPTION: GENERAL PROTECTION FAULT, error_code={}\n{:#?}",
        error_code,
        stack_frame
    );
    exit_qemu(QemuExitCode::Failed);
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use pc_keyboard::{layouts, DecodedKey, HandleControl, PS2Keyboard, ScancodeSet1};
    use x86_64::instructions::port::Port;

    lazy_static! {
        static ref KEYBOARD: Mutex<PS2Keyboard<layouts::Us104Key, ScancodeSet1>> =
            Mutex::new(PS2Keyboard::new(
                ScancodeSet1::new(),
                layouts::Us104Key,
                HandleControl::Ignore,
            ));
    }

    let mut keyboard = KEYBOARD.lock();
    let mut port: Port<u8> = Port::new(0x60);
    let scancode = unsafe { port.read() };

    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        if let Some(DecodedKey::Unicode(character)) = keyboard.process_keyevent(key_event) {
            crate::shell::push_input(character);
        }
    }

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}
