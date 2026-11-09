//! Text buffers that are reused without allocating after construction.

use std::fmt::{self, Write};

/// A string with fixed capacity. Writes that do not fit are cut at a character boundary instead of growing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedText {
    text: String,
}

impl FixedText {
    pub fn new(capacity: usize) -> Self {
        Self {
            text: String::with_capacity(capacity),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }

    /// Replaces the contents with `text`, cut to fit.
    pub fn set(&mut self, text: &str) {
        self.text.clear();
        self.push(text);
    }

    pub fn push(&mut self, text: &str) {
        for c in text.chars() {
            if self.text.len() + c.len_utf8() > self.text.capacity() {
                break;
            }
            self.text.push(c);
        }
    }
}

impl Write for FixedText {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push(s);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_grows() {
        let mut text = FixedText::new(5);
        text.set("Jos\u{e9} Silva");
        assert_eq!(text.as_str(), "Jos\u{e9}", "cut at a character boundary");
        let _ = write!(text, "{:.1}", 12.345);
        assert_eq!(text.as_str(), "Jos\u{e9}", "full");
        text.clear();
        let _ = write!(text, "{:+.1}", -1.25);
        assert_eq!(text.as_str(), "-1.2");
        assert_eq!(text.text.capacity(), 5);
    }
}
