//! Line-atomic hold buffer. Complete TEXT lines wait here for a finality
//! verdict, remembering the AbsRow and row generation captured when the line
//! landed so stale overlays are detectable at emission. Deadlines are
//! assigned at completion time (hold_ms); the pipeline resolves expired
//! fronts on tick. The incomplete trailing line has its own deadline and is
//! never held hostage: once it bleeds out raw (deadline, CTRL flush, or
//! overflow), the rest of that line streams raw until its newline resets the
//! state. Total held bytes are capped (HOLD_CAP) by the pipeline forcing
//! front resolution. FIFO is absolute — nothing ever reorders.

use std::collections::VecDeque;
use std::time::Instant;

use crate::term::telemetry::SgrState;
use crate::types::AbsRow;

pub const HOLD_CAP: usize = 8 * 1024;

#[derive(Debug)]
pub struct HeldLine {
    pub bytes: Vec<u8>,
    pub row: AbsRow,
    pub generation: u64,
    pub sgr_before: SgrState,
    pub deadline: Instant,
}

#[derive(Debug, Default)]
pub struct Hold {
    queue: VecDeque<HeldLine>,
    partial: Vec<u8>,
    partial_deadline: Option<Instant>,
    bled: bool,
    held_bytes: usize,
}

impl Hold {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_line_start(&self) -> bool {
        self.partial.is_empty() && !self.bled
    }

    pub fn push_partial(&mut self, piece: &[u8], deadline: Instant) -> bool {
        if self.bled {
            return true;
        }
        if self.partial.is_empty() {
            self.partial_deadline = Some(deadline);
        }
        self.partial.extend_from_slice(piece);
        false
    }

    pub fn complete_line(
        &mut self,
        piece: &[u8],
        row: AbsRow,
        generation: u64,
        sgr_before: SgrState,
        deadline: Instant,
    ) -> bool {
        self.partial_deadline = None;
        if self.bled {
            self.bled = false;
            return true;
        }
        let mut bytes = std::mem::take(&mut self.partial);
        bytes.extend_from_slice(piece);
        self.held_bytes += bytes.len();
        self.queue.push_back(HeldLine {
            bytes,
            row,
            generation,
            sgr_before,
            deadline,
        });
        false
    }

    pub fn front(&self) -> Option<&HeldLine> {
        self.queue.front()
    }

    pub fn pop_front(&mut self) -> Option<HeldLine> {
        let line = self.queue.pop_front()?;
        self.held_bytes -= line.bytes.len();
        Some(line)
    }

    pub fn pop_expired(&mut self, now: Instant) -> Option<HeldLine> {
        if self.queue.front()?.deadline <= now {
            self.pop_front()
        } else {
            None
        }
    }

    pub fn partial_expired(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.bled || self.partial.is_empty() || self.partial_deadline? > now {
            return None;
        }
        self.bled = true;
        self.partial_deadline = None;
        Some(std::mem::take(&mut self.partial))
    }

    pub fn take_partial_raw(&mut self) -> Vec<u8> {
        if self.partial.is_empty() {
            return Vec::new();
        }
        self.bled = true;
        self.partial_deadline = None;
        std::mem::take(&mut self.partial)
    }

    pub fn held_bytes(&self) -> usize {
        self.held_bytes
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty() && self.partial.is_empty()
    }
}
