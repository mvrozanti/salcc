//! TEXT/CTRL run segmentation, stateful across chunk boundaries. TEXT is
//! what the hold buffer may delay and the styler may touch: printable UTF-8,
//! SGR sequences (CSI ... m with numeric/:;-only params), CR and LF. CTRL is
//! everything else — cursor movement, erases, OSC, DCS, mode flips, stray
//! control bytes, invalid UTF-8 — and is sacred: emitted in order,
//! unmodified, and it flushes the hold so styled text never reorders across
//! a control boundary. Partial escape sequences and split UTF-8 scalars
//! buffer in `pending` until complete; a pathological unterminated sequence
//! larger than MAX_PENDING is dumped as CTRL rather than held forever, and
//! `take_pending` drains the tail at stream end.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Text,
    Ctrl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub kind: RunKind,
    pub bytes: Vec<u8>,
}

const MAX_PENDING: usize = 4096;

enum Piece {
    Complete(RunKind, usize),
    Incomplete,
}

#[derive(Debug, Default)]
pub struct Segmenter {
    pending: Vec<u8>,
}

impl Segmenter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Run> {
        self.pending.extend_from_slice(chunk);
        let buf = std::mem::take(&mut self.pending);
        let mut runs: Vec<Run> = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            match classify(&buf[i..]) {
                Piece::Complete(kind, len) => {
                    push_run(&mut runs, kind, &buf[i..i + len]);
                    i += len;
                }
                Piece::Incomplete => {
                    if buf.len() - i > MAX_PENDING {
                        push_run(&mut runs, RunKind::Ctrl, &buf[i..]);
                        i = buf.len();
                    }
                    break;
                }
            }
        }
        self.pending = buf[i..].to_vec();
        runs
    }

    pub fn take_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

fn push_run(runs: &mut Vec<Run>, kind: RunKind, bytes: &[u8]) {
    if let Some(last) = runs.last_mut()
        && last.kind == kind
    {
        last.bytes.extend_from_slice(bytes);
        return;
    }
    runs.push(Run {
        kind,
        bytes: bytes.to_vec(),
    });
}

fn classify(buf: &[u8]) -> Piece {
    match buf[0] {
        0x1b => classify_escape(buf),
        b'\r' | b'\n' => Piece::Complete(RunKind::Text, 1),
        0x00..=0x1f | 0x7f => Piece::Complete(RunKind::Ctrl, 1),
        0x20..=0x7e => Piece::Complete(RunKind::Text, 1),
        _ => classify_utf8(buf),
    }
}

fn classify_escape(buf: &[u8]) -> Piece {
    let Some(&kind) = buf.get(1) else {
        return Piece::Incomplete;
    };
    match kind {
        b'[' => classify_csi(buf),
        b']' => classify_string_terminated(buf, true),
        b'P' | b'X' | b'^' | b'_' => classify_string_terminated(buf, false),
        0x20..=0x2f => classify_nf_escape(buf),
        0x30..=0x7e => Piece::Complete(RunKind::Ctrl, 2),
        _ => Piece::Complete(RunKind::Ctrl, 1),
    }
}

fn classify_csi(buf: &[u8]) -> Piece {
    let mut sgr_safe = true;
    let mut i = 2;
    while i < buf.len() {
        match buf[i] {
            b'0'..=b'9' | b';' | b':' => i += 1,
            0x20..=0x3f => {
                sgr_safe = false;
                i += 1;
            }
            0x40..=0x7e => {
                let kind = if sgr_safe && buf[i] == b'm' {
                    RunKind::Text
                } else {
                    RunKind::Ctrl
                };
                return Piece::Complete(kind, i + 1);
            }
            _ => return Piece::Complete(RunKind::Ctrl, i),
        }
    }
    Piece::Incomplete
}

fn classify_string_terminated(buf: &[u8], bel_terminates: bool) -> Piece {
    let mut i = 2;
    while i < buf.len() {
        match buf[i] {
            0x07 if bel_terminates => return Piece::Complete(RunKind::Ctrl, i + 1),
            0x1b => {
                if let Some(&next) = buf.get(i + 1) {
                    if next == b'\\' {
                        return Piece::Complete(RunKind::Ctrl, i + 2);
                    }
                    return Piece::Complete(RunKind::Ctrl, i);
                }
                return Piece::Incomplete;
            }
            _ => i += 1,
        }
    }
    Piece::Incomplete
}

fn classify_nf_escape(buf: &[u8]) -> Piece {
    let mut i = 2;
    while i < buf.len() {
        match buf[i] {
            0x20..=0x2f => i += 1,
            0x30..=0x7e => return Piece::Complete(RunKind::Ctrl, i + 1),
            _ => return Piece::Complete(RunKind::Ctrl, i),
        }
    }
    Piece::Incomplete
}

fn classify_utf8(buf: &[u8]) -> Piece {
    let len = match buf[0] {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Piece::Complete(RunKind::Ctrl, 1),
    };
    if buf.len() < len {
        return Piece::Incomplete;
    }
    if std::str::from_utf8(&buf[..len]).is_ok() {
        Piece::Complete(RunKind::Text, len)
    } else {
        Piece::Complete(RunKind::Ctrl, 1)
    }
}
