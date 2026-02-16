//! Terminal output helpers: conditional colored writing for errors and success.

use std::io::Write;

use owo_colors::OwoColorize;

/// Writes an error line to the given writer. When `use_color` is true, the message is red.
pub fn write_error(w: &mut impl Write, use_color: bool, msg: &str) -> std::io::Result<()> {
    if use_color {
        writeln!(w, "{}", msg.red())
    } else {
        writeln!(w, "{}", msg)
    }
}

/// Writes a success line to the given writer. When `use_color` is true, the message is green.
pub fn write_success(w: &mut impl Write, use_color: bool, msg: &str) -> std::io::Result<()> {
    if use_color {
        writeln!(w, "{}", msg.green())
    } else {
        writeln!(w, "{}", msg)
    }
}
