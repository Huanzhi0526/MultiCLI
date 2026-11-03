//! Bounded byte ring for one terminal stream.
//!
//! Offsets are absolute stream offsets: `seq` in the IPC contract *is* the byte
//! offset, so a cursor is always meaningful even after the front was trimmed.
//!
//! Two invariants matter for a terminal:
//!
//! 1. Trimming the front must not leave the reader starting inside a UTF-8
//!    character or inside an escape sequence. Each retained segment records the
//!    parser state at its first byte, and the front is advanced to the first
//!    position that is both `Ground` and a UTF-8 start byte.
//! 2. A read must never split a multi-byte character or an escape sequence that
//!    is still incomplete, because the UI would then paint a replacement glyph
//!    or a mangled control sequence. The tail is held back and delivered with
//!    the next read.

use std::collections::VecDeque;

/// How far an incomplete escape sequence may hold a read back. A program that
/// prints a lone ESC must not stall the stream forever.
const MAX_HELD_SEQUENCE: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    /// Plain text; a byte here is safe to start a delivery at.
    Ground,
    /// Saw ESC.
    Escape,
    /// Inside `ESC [ ... final`.
    Csi,
    /// Inside `ESC ] ... BEL|ST`.
    Osc,
    /// Saw ESC inside OSC (possible string terminator).
    OscEscape,
    /// A two/three byte escape we only need to count to its end.
    Other,
}

impl StreamState {
    fn is_ground(self) -> bool {
        matches!(self, Self::Ground)
    }
}

/// Advances the parser by one byte and returns the new state.
pub fn advance(state: StreamState, byte: u8) -> StreamState {
    match state {
        StreamState::Ground => {
            if byte == 0x1b {
                StreamState::Escape
            } else {
                StreamState::Ground
            }
        }
        StreamState::Escape => match byte {
            b'[' => StreamState::Csi,
            b']' => StreamState::Osc,
            0x1b => StreamState::Escape,
            _ => StreamState::Ground,
        },
        StreamState::Csi => {
            // Final byte of a CSI sequence is 0x40..=0x7e.
            if (0x40..=0x7e).contains(&byte) {
                StreamState::Ground
            } else {
                StreamState::Csi
            }
        }
        StreamState::Osc => match byte {
            0x07 => StreamState::Ground,
            0x1b => StreamState::OscEscape,
            _ => StreamState::Osc,
        },
        StreamState::OscEscape => match byte {
            b'\\' => StreamState::Ground,
            0x1b => StreamState::OscEscape,
            _ => StreamState::Osc,
        },
        StreamState::Other => StreamState::Ground,
    }
}

fn is_utf8_start(byte: u8) -> bool {
    byte & 0xc0 != 0x80
}

#[derive(Debug)]
struct Segment {
    /// Absolute stream offset of `bytes[0]`.
    start: u64,
    bytes: Vec<u8>,
    /// Parser state at `bytes[0]`. `Ground` means the previous byte completed a
    /// character and any escape sequence.
    state_at_start: StreamState,
}

#[derive(Debug)]
pub struct ByteRing {
    cap: usize,
    segments: VecDeque<Segment>,
    /// Absolute offset of the oldest retained byte.
    base: u64,
    /// Absolute offset of the next byte to be appended.
    end: u64,
    dropped: u64,
    buffered: usize,
    tail_state: StreamState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOutcome {
    pub from_seq: u64,
    pub to_seq: u64,
    pub text: String,
    pub bytes: u64,
    /// Bytes discarded before `from_seq` relative to the caller's cursor.
    pub dropped_before: u64,
    pub buffered_bytes: u64,
    pub dropped_bytes: u64,
}

impl ByteRing {
    pub fn new(cap: usize) -> Self {
        // The wire contract enforces a 256 KiB minimum; this guard only keeps a
        // degenerate test capacity from making the ring unusable.
        let cap = cap.max(16);
        Self {
            cap,
            segments: VecDeque::new(),
            base: 0,
            end: 0,
            dropped: 0,
            buffered: 0,
            tail_state: StreamState::Ground,
        }
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn buffered_bytes(&self) -> u64 {
        self.buffered as u64
    }

    pub fn dropped_bytes(&self) -> u64 {
        self.dropped
    }

    pub fn base(&self) -> u64 {
        self.base
    }

    pub fn end(&self) -> u64 {
        self.end
    }

    /// Appends one PTY read. Returns the stream offset assigned to its first
    /// byte.
    pub fn push(&mut self, bytes: &[u8]) -> u64 {
        if bytes.is_empty() {
            return self.end;
        }
        let start = self.end;
        let safe_start = self.tail_state.is_ground() && is_utf8_start(bytes[0]);
        let state_at_start = self.tail_state;
        // Keep the tracker honest even for bytes that will be trimmed later.
        let mut state = self.tail_state;
        for byte in bytes {
            state = advance(state, *byte);
        }
        self.tail_state = state;
        self.end += bytes.len() as u64;
        self.buffered += bytes.len();
        self.segments.push_back(Segment {
            start,
            bytes: bytes.to_vec(),
            state_at_start,
        });
        if safe_start {
            // Mark the first segment of the stream: it is safe by definition.
        }
        self.enforce_capacity();
        start
    }

    fn enforce_capacity(&mut self) {
        while self.buffered > self.cap {
            if self.segments.len() > 1 {
                if let Some(front) = self.segments.pop_front() {
                    self.buffered -= front.bytes.len();
                    self.dropped += front.bytes.len() as u64;
                    self.base = front.start + front.bytes.len() as u64;
                }
                continue;
            }
            // A single read larger than the whole budget: keep the newest
            // bytes, aligned to a character and sequence boundary.
            let excess = self.buffered - self.cap;
            let Some(front) = self.segments.front_mut() else {
                break;
            };
            let Some(offset) = first_safe_offset(front, excess) else {
                break;
            };
            if offset == 0 {
                break;
            }
            front.bytes.drain(..offset);
            front.start += offset as u64;
            front.state_at_start = StreamState::Ground;
            self.buffered -= offset;
            self.dropped += offset as u64;
            self.base = front.start;
        }
        self.align_front();
    }

    /// Advances the front to a position that is safe to start a delivery at.
    /// Loops, because dropping one unsafe segment can expose another one that is
    /// still inside the same escape sequence.
    fn align_front(&mut self) {
        loop {
            let Some(front) = self.segments.front() else {
                return;
            };
            if front.bytes.is_empty() {
                self.segments.pop_front();
                continue;
            }
            if front.state_at_start.is_ground() && is_utf8_start(front.bytes[0]) {
                return;
            }
            let offset = first_safe_offset(front, 0).unwrap_or(front.bytes.len());
            if offset == 0 {
                return;
            }
            if offset >= front.bytes.len() {
                let Some(front) = self.segments.pop_front() else {
                    return;
                };
                self.buffered -= front.bytes.len();
                self.dropped += front.bytes.len() as u64;
                self.base = front.start + front.bytes.len() as u64;
                continue;
            }
            let Some(front) = self.segments.front_mut() else {
                return;
            };
            front.bytes.drain(..offset);
            front.start += offset as u64;
            front.state_at_start = StreamState::Ground;
            self.buffered -= offset;
            self.dropped += offset as u64;
            self.base = front.start;
            return;
        }
    }

    /// Reads up to `limit` bytes from `cursor`, holding back an incomplete
    /// trailing character or escape sequence.
    pub fn read(&self, cursor: Option<u64>, limit: usize) -> Result<ReadOutcome, String> {
        let limit = limit.max(1);
        let mut dropped_before = 0u64;
        let from = match cursor {
            None => self.base,
            Some(value) => {
                if value > self.end {
                    return Err(format!(
                        "cursor {value} is past the end of the stream ({}); reset the cursor",
                        self.end
                    ));
                }
                if value < self.base {
                    dropped_before = self.base - value;
                    self.base
                } else {
                    value
                }
            }
        };
        if from >= self.end {
            return Ok(ReadOutcome {
                from_seq: from.min(self.end),
                to_seq: self.end.min(from),
                text: String::new(),
                bytes: 0,
                dropped_before,
                buffered_bytes: self.buffered as u64,
                dropped_bytes: self.dropped,
            });
        }
        let mut collected: Vec<u8> = Vec::new();
        let want = (limit as u64).min(self.end - from) as usize;
        for segment in &self.segments {
            let seg_end = segment.start + segment.bytes.len() as u64;
            if seg_end <= from || collected.len() >= want {
                continue;
            }
            let begin = from.saturating_sub(segment.start) as usize;
            let take = ((want - collected.len()) as u64).min(segment.bytes.len() as u64 - begin as u64)
                as usize;
            collected.extend_from_slice(&segment.bytes[begin..begin + take]);
            if collected.len() >= want {
                break;
            }
        }
        let cut = safe_cut(&collected);
        let delivered = &collected[..cut];
        let text = String::from_utf8_lossy(delivered).into_owned();
        Ok(ReadOutcome {
            from_seq: from,
            to_seq: from + delivered.len() as u64,
            text,
            bytes: delivered.len() as u64,
            dropped_before,
            buffered_bytes: self.buffered as u64,
            dropped_bytes: self.dropped,
        })
    }
}

/// First offset at or after `from` that is safe to start a delivery at:
/// the parser is in `Ground` there and the byte is a UTF-8 start byte.
fn first_safe_offset(segment: &Segment, from: usize) -> Option<usize> {
    let mut state = segment.state_at_start;
    let from = from.min(segment.bytes.len());
    for (index, byte) in segment.bytes.iter().enumerate() {
        if index < from {
            state = advance(state, *byte);
            continue;
        }
        if state.is_ground() && is_utf8_start(*byte) {
            return Some(index);
        }
        state = advance(state, *byte);
    }
    None
}

/// Largest prefix of `bytes` that ends on a character and sequence boundary.
fn safe_cut(bytes: &[u8]) -> usize {
    safe_cut_inner(bytes, true)
}

/// Log rotation must use complete sequences even when a malformed stream
/// exceeds the interactive reader's escape holdback window.
pub(super) fn safe_log_cut(bytes: &[u8]) -> usize {
    safe_cut_inner(bytes, false)
}

fn safe_cut_inner(bytes: &[u8], release_long_sequence: bool) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    // 1. Do not split a multi-byte UTF-8 character. An invalid byte *inside*
    //    the batch is not a reason to stall: it is delivered as U+FFFD so the
    //    stream keeps moving, which is what a terminal renderer shows anyway.
    let mut cut = bytes.len();
    let mut checked = 0;
    while checked < bytes.len() {
        match std::str::from_utf8(&bytes[checked..]) {
            Ok(_) => break,
            Err(error) => {
                checked += error.valid_up_to();
                if let Some(invalid) = error.error_len() {
                    checked += invalid;
                } else {
                    // An earlier invalid byte must not hide an incomplete
                    // valid character at the end of this batch.
                    cut = checked;
                    break;
                }
            }
        }
    }
    if cut == 0 {
        return 0;
    }
    // 2. Do not split a control sequence that is still open at `cut`.
    let mut state = StreamState::Ground;
    let mut open_from: Option<usize> = None;
    for (index, byte) in bytes[..cut].iter().enumerate() {
        let before = state;
        state = advance(state, *byte);
        if before.is_ground() && state != StreamState::Ground {
            open_from = Some(index);
        }
        if state.is_ground() {
            open_from = None;
        }
    }
    if let Some(start) = open_from {
        if release_long_sequence && cut - start > MAX_HELD_SEQUENCE {
            // Pathological: deliver it rather than stall the stream.
            return cut;
        }
        cut = start;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_is_an_absolute_stream_offset() {
        let mut ring = ByteRing::new(1024);
        ring.push(b"hello ");
        ring.push(b"world");
        assert_eq!(ring.end(), 11);
        let read = ring.read(None, 1024).unwrap();
        assert_eq!(read.from_seq, 0);
        assert_eq!(read.to_seq, 11);
        assert_eq!(read.text, "hello world");
        let next = ring.read(Some(read.to_seq), 1024).unwrap();
        assert_eq!(next.bytes, 0);
        assert_eq!(next.to_seq, 11);
    }

    #[test]
    fn a_split_utf8_character_is_held_back_until_complete() {
        let mut ring = ByteRing::new(1024);
        let text = "中文😀".as_bytes();
        // One byte at a time: an incomplete character is never delivered, so a
        // reader can never see a replacement glyph at a chunk boundary.
        for byte in text {
            ring.push(&[*byte]);
            let read = ring.read(None, 1024).unwrap();
            assert!(
                !read.text.contains('\u{fffd}'),
                "a split character leaked as a replacement glyph: {:?}",
                read.text
            );
        }
        let read = ring.read(None, 1024).unwrap();
        assert_eq!(read.text, "中文😀");
    }

    #[test]
    fn an_incomplete_csi_is_held_back_and_completed_next_read() {
        let mut ring = ByteRing::new(1024);
        ring.push(b"a\x1b[3");
        let first = ring.read(None, 1024).unwrap();
        assert_eq!(first.text, "a");
        assert_eq!(first.to_seq, 1);
        ring.push(b"1mB");
        let second = ring.read(Some(first.to_seq), 1024).unwrap();
        assert_eq!(second.text, "\x1b[31mB");
    }

    #[test]
    fn a_lone_escape_does_not_stall_the_stream_forever() {
        let mut ring = ByteRing::new(2048);
        ring.push(b"\x1b");
        ring.push(&vec![b'x'; MAX_HELD_SEQUENCE + 16]);
        let read = ring.read(None, 4096).unwrap();
        assert!(read.bytes > 0, "held sequence must be released eventually");
    }

    #[test]
    fn trimming_keeps_the_front_on_a_character_boundary() {
        let mut ring = ByteRing::new(1024);
        let chunk = "中文".repeat(200); // 1200 bytes, each char 3 bytes
        ring.push(chunk.as_bytes());
        assert!(ring.buffered_bytes() <= 1024);
        let read = ring.read(None, 4096).unwrap();
        assert!(
            !read.text.starts_with('\u{fffd}'),
            "front must not start mid-character: {:?}",
            &read.text[..read.text.len().min(8)]
        );
        assert!(read.dropped_bytes > 0);
        assert!(read.text.len() > 0);
    }

    #[test]
    fn trimming_skips_a_sequence_that_started_before_the_new_front() {
        let mut ring = ByteRing::new(64);
        ring.push(b"\x1b[38;5;196m"); // complete sequence
        ring.push(&vec![b'a'; 100]);
        let read = ring.read(None, 4096).unwrap();
        assert!(!read.text.contains('\u{fffd}'));
        // A sequence that is cut in the middle must not be delivered partially.
        let mut ring = ByteRing::new(64);
        ring.push(b"\x1b[38;5;196mAAAA");
        ring.push(&vec![b'b'; 100]);
        let read = ring.read(None, 4096).unwrap();
        assert!(!read.text.starts_with("\x1b["));
    }

    #[test]
    fn reading_less_than_available_advances_only_by_delivered_bytes() {
        let mut ring = ByteRing::new(1024);
        ring.push(b"0123456789");
        let first = ring.read(None, 4).unwrap();
        assert_eq!(first.text, "0123");
        let second = ring.read(Some(first.to_seq), 100).unwrap();
        assert_eq!(second.from_seq, 4);
        assert_eq!(second.text, "456789");
    }

    #[test]
    fn cursor_past_the_end_is_a_real_error_not_silent_empty_output() {
        let mut ring = ByteRing::new(1024);
        ring.push(b"abc");
        assert!(ring.read(Some(10), 16).is_err());
    }

    #[test]
    fn dropping_history_is_counted_and_reported_to_the_next_reader() {
        let mut ring = ByteRing::new(1024);
        for _ in 0..10 {
            ring.push(&vec![b'x'; 512]);
        }
        let read = ring.read(Some(0), 128).unwrap();
        assert!(read.dropped_before > 0);
        assert_eq!(read.from_seq, ring.base());
        assert!(read.dropped_bytes >= read.dropped_before);
    }

    #[test]
    fn interior_invalid_bytes_are_replaced_but_the_stream_continues() {
        let mut ring = ByteRing::new(1024);
        ring.push(b"a\xffb");
        let read = ring.read(None, 1024).unwrap();
        assert_eq!(read.text, "a\u{fffd}b");
        assert_eq!(read.bytes, 3);
    }

    #[test]
    fn invalid_bytes_do_not_consume_the_next_split_character() {
        let mut ring = ByteRing::new(1024);
        ring.push(&[0xff, 0xe4, 0xb8]);
        let first = ring.read(None, 1024).unwrap();
        assert_eq!(first.text, "\u{fffd}");
        ring.push(&[0xad]);
        let second = ring.read(Some(first.to_seq), 1024).unwrap();
        assert_eq!(second.text, "中");
    }
}
