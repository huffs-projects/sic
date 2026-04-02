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

/// Left-aligns `s` in a field of `width` character cells (counting Unicode scalar values).
/// Truncates with `"..."` when `s` is longer than `width` (for `width > 3`).
pub fn format_fixed_width(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= width {
        return format!("{}{}", s, " ".repeat(width - count));
    }
    if width <= 3 {
        return s.chars().take(width).collect();
    }
    let take = width - 3;
    let truncated: String = s.chars().take(take).collect();
    format!("{}{}", truncated, "...")
}

#[cfg(test)]
mod tests {
    use super::format_fixed_width;

    #[test]
    fn format_fixed_width_pads_short() {
        assert_eq!(format_fixed_width("hi", 4), "hi  ");
    }

    #[test]
    fn format_fixed_width_truncates_long() {
        assert_eq!(format_fixed_width("abcdefgh", 5), "ab...");
    }
}
