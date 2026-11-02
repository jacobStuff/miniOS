//! A minimal interactive command-line shell, driven by keystrokes the
//! keyboard interrupt handler pushes into a shared queue (see
//! `push_input`). Runs as one more task in the preemptive scheduler.

use crate::{fs, qemu};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

static INPUT_QUEUE: Mutex<VecDeque<char>> = Mutex::new(VecDeque::new());

/// Called from the keyboard interrupt handler with each decoded character.
pub fn push_input(c: char) {
    let mut queue = INPUT_QUEUE.lock();
    // Bound the queue so a flood of input (or nobody ever reading it)
    // can't grow it unboundedly.
    if queue.len() < 256 {
        queue.push_back(c);
    }
}

fn pop_input() -> Option<char> {
    INPUT_QUEUE.lock().pop_front()
}

pub extern "C" fn run() -> ! {
    serial_println!("shell: ready for input");
    println!();
    println!("miniOS shell. Type 'help' for a list of commands.");
    print!("> ");

    let mut line = String::new();
    loop {
        match pop_input() {
            Some('\n') => {
                println!();
                execute(&line);
                line.clear();
                print!("> ");
            }
            Some('\u{8}') => {
                if line.pop().is_some() {
                    print!("\u{8}");
                }
            }
            Some(c) => {
                line.push(c);
                print!("{c}");
            }
            None => x86_64::instructions::hlt(),
        }
    }
}

/// Prints to both the on-screen framebuffer and the serial console. Command
/// output uses this (rather than plain `println!`) so a headless QEMU run
/// -- the only way this kernel gets tested in CI -- can see the shell
/// actually working, not just the keystrokes going in.
macro_rules! outln {
    ($($arg:tt)*) => {{
        println!($($arg)*);
        serial_println!($($arg)*);
    }};
}

fn execute(line: &str) {
    serial_println!("shell> {line}");
    let mut parts = line.split_whitespace();
    match parts.next() {
        None => {}
        Some("help") => {
            outln!(
                "commands: help, ls, cat <file>, lsdisk, catdisk <file>, echo <text...>, uptime, clear, exit"
            );
            outln!("('ls'/'cat' read files compiled into the kernel; 'lsdisk'/'catdisk' read a real FAT16 disk via the ATA driver)");
        }
        Some("ls") => {
            for name in fs::list() {
                outln!("{name}");
            }
        }
        Some("cat") => match parts.next() {
            Some(name) => match fs::read(name) {
                Some(contents) => {
                    print!("{contents}");
                    serial_println!("{contents}");
                }
                None => outln!("cat: {name}: no such file"),
            },
            None => outln!("usage: cat <file>"),
        },
        Some("lsdisk") => {
            for entry in crate::fat16::list() {
                outln!("{} ({} bytes)", entry.name, entry.size);
            }
        }
        Some("catdisk") => match parts.next() {
            Some(name) => match crate::fat16::read(name) {
                Some(contents) => match core::str::from_utf8(&contents) {
                    Ok(text) => {
                        print!("{text}");
                        serial_println!("{text}");
                    }
                    Err(_) => outln!("catdisk: {name}: not valid UTF-8 ({} bytes)", contents.len()),
                },
                None => outln!("catdisk: {name}: no such file"),
            },
            None => outln!("usage: catdisk <file>"),
        },
        Some("echo") => {
            let rest: Vec<&str> = parts.collect();
            outln!("{}", rest.join(" "));
        }
        Some("uptime") => {
            outln!("{} timer ticks since boot", crate::interrupts::tick_count());
        }
        Some("clear") => {
            crate::framebuffer::clear();
        }
        Some("exit") => {
            serial_println!("shell: exit command received");
            qemu::exit_qemu(qemu::QemuExitCode::Success);
        }
        Some(other) => {
            outln!("unknown command: {other} (try 'help')");
        }
    }
}
