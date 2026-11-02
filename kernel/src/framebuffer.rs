//! Renders text onto the pixel framebuffer the bootloader hands us.
//!
//! Modern `bootloader` (0.11+) boots straight into a linear graphics
//! framebuffer on both BIOS and UEFI -- there is no classic 0xb8000 VGA text
//! mode available anymore -- so printable text has to be rasterized with a
//! bitmap font and blitted pixel-by-pixel.

use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use core::fmt;
use core::ptr;
use noto_sans_mono_bitmap::{get_raster, get_raster_width, FontWeight, RasterHeight, RasterizedChar};
use spin::Mutex;
use x86_64::instructions::interrupts;

const LINE_SPACING: usize = 2;
const LETTER_SPACING: usize = 0;
const BORDER_PADDING: usize = 1;

mod font_constants {
    use super::*;

    pub const CHAR_RASTER_HEIGHT: RasterHeight = RasterHeight::Size16;
    pub const CHAR_RASTER_WIDTH: usize = get_raster_width(FontWeight::Regular, CHAR_RASTER_HEIGHT);
    pub const BACKUP_CHAR: char = '\u{fffd}';
    pub const FONT_WEIGHT: FontWeight = FontWeight::Regular;
}

fn get_char_raster(c: char) -> RasterizedChar {
    fn get(c: char) -> Option<RasterizedChar> {
        get_raster(c, font_constants::FONT_WEIGHT, font_constants::CHAR_RASTER_HEIGHT)
    }
    get(c).unwrap_or_else(|| get(font_constants::BACKUP_CHAR).expect("backup char must exist"))
}

pub struct FrameBufferWriter {
    framebuffer: &'static mut [u8],
    info: FrameBufferInfo,
    x_pos: usize,
    y_pos: usize,
}

impl FrameBufferWriter {
    pub fn new(framebuffer: &'static mut [u8], info: FrameBufferInfo) -> Self {
        let mut writer = Self {
            framebuffer,
            info,
            x_pos: 0,
            y_pos: 0,
        };
        writer.clear();
        writer
    }

    fn newline(&mut self) {
        self.y_pos += font_constants::CHAR_RASTER_HEIGHT.val() + LINE_SPACING;
        self.carriage_return();
    }

    fn carriage_return(&mut self) {
        self.x_pos = BORDER_PADDING;
    }

    pub fn clear(&mut self) {
        self.x_pos = BORDER_PADDING;
        self.y_pos = BORDER_PADDING;
        self.framebuffer.fill(0);
    }

    fn width(&self) -> usize {
        self.info.width
    }

    fn height(&self) -> usize {
        self.info.height
    }

    fn write_char(&mut self, c: char) {
        match c {
            '\n' => self.newline(),
            '\r' => self.carriage_return(),
            '\u{8}' => {
                // Backspace: step back one cell and blank it, for the shell's
                // line editing. Does nothing at the start of a line -- we
                // don't track previous lines, so there's nowhere to go back
                // to.
                if self.x_pos > BORDER_PADDING {
                    self.x_pos -= font_constants::CHAR_RASTER_WIDTH + LETTER_SPACING;
                    self.write_rendered_char(get_char_raster(' '));
                    self.x_pos -= font_constants::CHAR_RASTER_WIDTH + LETTER_SPACING;
                }
            }
            c => {
                let new_xpos = self.x_pos + font_constants::CHAR_RASTER_WIDTH;
                if new_xpos >= self.width() {
                    self.newline();
                }
                let new_ypos = self.y_pos + font_constants::CHAR_RASTER_HEIGHT.val() + BORDER_PADDING;
                if new_ypos >= self.height() {
                    self.clear();
                }
                self.write_rendered_char(get_char_raster(c));
            }
        }
    }

    fn write_rendered_char(&mut self, rendered_char: RasterizedChar) {
        for (y, row) in rendered_char.raster().iter().enumerate() {
            for (x, byte) in row.iter().enumerate() {
                self.write_pixel(self.x_pos + x, self.y_pos + y, *byte);
            }
        }
        self.x_pos += rendered_char.width() + LETTER_SPACING;
    }

    fn write_pixel(&mut self, x: usize, y: usize, intensity: u8) {
        let pixel_offset = y * self.info.stride + x;
        let color = match self.info.pixel_format {
            PixelFormat::Rgb => [intensity, intensity, intensity / 2, 0],
            PixelFormat::Bgr => [intensity / 2, intensity, intensity, 0],
            PixelFormat::U8 => [if intensity > 200 { 0xf } else { 0 }, 0, 0, 0],
            other => {
                self.info.pixel_format = PixelFormat::Rgb;
                panic!("pixel format {other:?} not supported in framebuffer writer")
            }
        };
        let bytes_per_pixel = self.info.bytes_per_pixel;
        let byte_offset = pixel_offset * bytes_per_pixel;
        self.framebuffer[byte_offset..(byte_offset + bytes_per_pixel)]
            .copy_from_slice(&color[..bytes_per_pixel]);
        let _ = unsafe { ptr::read_volatile(&self.framebuffer[byte_offset]) };
    }
}

unsafe impl Send for FrameBufferWriter {}
unsafe impl Sync for FrameBufferWriter {}

impl fmt::Write for FrameBufferWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            self.write_char(c);
        }
        Ok(())
    }
}

pub static WRITER: Mutex<Option<FrameBufferWriter>> = Mutex::new(None);

/// Must be called exactly once, early in kernel_main, before any `println!`.
pub fn init(framebuffer: &'static mut FrameBuffer) {
    let info = framebuffer.info();
    let buffer = framebuffer.buffer_mut();
    *WRITER.lock() = Some(FrameBufferWriter::new(buffer, info));
}

pub fn clear() {
    interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.lock().as_mut() {
            writer.clear();
        }
    });
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::framebuffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;

    interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.lock().as_mut() {
            writer.write_fmt(args).unwrap();
        }
    });
}
