//! Ring-buffer scrollback for a PTY session.
//!
//! Keeps at most `max` bytes of recent output. When trimming, the cut point is
//! advanced to the next newline so a reattaching client never replays a
//! half-line (best effort - long escape sequences without newlines may still
//! be cut, which xterm tolerates).

/// Byte ring buffer with a fixed maximum size.
#[derive(Debug, Clone)]
pub struct Scrollback {
    buf: Vec<u8>,
    max: usize,
}

impl Scrollback {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            buf: Vec::new(),
            max: max_bytes,
        }
    }

    /// Append bytes, trimming the head to stay within `max`.
    pub fn append(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        self.trim();
    }

    /// The currently retained bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn trim(&mut self) {
        if self.buf.len() <= self.max {
            return;
        }
        let excess = self.buf.len() - self.max;
        // Advance the cut to the next newline boundary (inclusive).
        let mut cut = excess;
        while cut < self.buf.len() && self.buf[cut] != b'\n' {
            cut += 1;
        }
        if cut < self.buf.len() {
            cut += 1; // include the newline itself
        }
        self.buf.drain(..cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_under_max() {
        let mut sb = Scrollback::new(10);
        sb.append(b"hello");
        assert_eq!(sb.bytes(), b"hello");
        sb.append(b" world"); // 11 bytes -> trimmed
        assert!(sb.len() <= 10);
    }

    #[test]
    fn trims_to_newline_boundary() {
        let mut sb = Scrollback::new(8);
        sb.append(b"one\ntwo\n");
        // "one\ntwo\n" is 8 bytes, still fits; append more to force a trim.
        sb.append(b"three");
        let bytes = sb.bytes();
        // Cut is advanced past the first newline; the replay never starts
        // mid-line (only the first retained byte may be at a line start).
        assert!(bytes.starts_with(b"two\n") || bytes.starts_with(b"three"));
    }

    #[test]
    fn empty_scrollback() {
        let sb = Scrollback::new(10);
        assert!(sb.is_empty());
        assert_eq!(sb.bytes(), b"");
    }
}
