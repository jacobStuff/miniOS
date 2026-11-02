//! A tiny demo "user program", hand-written in position-independent x86_64
//! assembly and embedded directly in the kernel binary.
//!
//! Why hand-written asm instead of a separate Rust crate: a real user
//! program would be compiled separately, linked at a fixed address (or
//! relocated by a loader), and loaded from the filesystem as an ELF file
//! -- all of which is substantial machinery on its own (a linker script, an
//! ELF parser, relocation handling) that's out of scope for demonstrating
//! the actual ring-3/syscall/separate-page-table mechanism. Instead, this
//! program uses only RIP-relative addressing, which makes its whole byte
//! range (`user_program_start..user_program_end`) safely copyable to any
//! page-aligned address: every reference to `msg` is encoded as a fixed
//! displacement from the current instruction, not an absolute address, so
//! it stays correct no matter where the kernel maps these bytes.
//!
//! The syscall convention here is entirely our own invention (not Linux's):
//! `int 0x80` with the syscall number in `rax`.
//!   0 = exit(code: rdi)
//!   1 = write(ptr: rdi, len: rsi)

core::arch::global_asm!(
    ".section .user_program, \"ax\"",
    ".global user_program_start",
    ".global user_program_end",
    "user_program_start:",
    // `mov reg, absolute_symbol` assembles as a *memory load* in LLVM's
    // Intel-syntax parser (same as MASM/NASM convention), not an immediate
    // -- so the message length is computed at runtime from two addresses
    // instead of referencing a `. - msg`-style constant directly.
    "lea rdi, [rip + msg]",
    "lea rsi, [rip + msg_end]",
    "sub rsi, rdi", // rsi = msg_end - msg = length
    "mov rax, 1",   // syscall 1: write(rdi=ptr, rsi=len)
    "int 0x80",
    "mov rdi, 42",
    "mov rax, 0", // syscall 0: exit(rdi=code)
    "int 0x80",
    "hlt", // unreachable: exit never returns to here
    "msg:",
    ".ascii \"Hello from ring 3! This text was written by a user-mode process running under its own page table, and reached the screen through an int 0x80 syscall.\\n\"",
    "msg_end:",
    "user_program_end:",
);

unsafe extern "C" {
    static user_program_start: u8;
    static user_program_end: u8;
}

/// The demo program's raw bytes, as embedded in the kernel's own binary.
pub fn demo_program_bytes() -> &'static [u8] {
    unsafe {
        let start = &raw const user_program_start;
        let end = &raw const user_program_end;
        let len = end as usize - start as usize;
        core::slice::from_raw_parts(start, len)
    }
}
