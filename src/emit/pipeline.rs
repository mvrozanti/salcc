//! StylingTransform: the emission-channel orchestrator plugged into the
//! io_loop seam. Per chunk: segment into runs; CTRL runs flush the hold and
//! pass through untouched; TEXT runs feed the shadow grid piece-by-piece
//! (split at newlines) so each completed line captures the AbsRow and row
//! generation it landed on, then wait in the hold for a finality verdict.
//! ScrollbackBound fronts drain eagerly; everything else resolves at its
//! deadline (tick-driven `flush`), on CTRL boundaries, on overflow, on
//! resize (a layout disruption: the hold flushes under the old geometry
//! before grid and finalizer resize), or at stream end — ScrollbackBound +
//! generation-still-valid emits styled, any other verdict emits the
//! original bytes untouched. Quiet gaps (>= 100ms
//! without child output) end the burst for the finalizer. Alt-screen output
//! and disabled caps pass through bit-perfect. Reverse video is budgeted to
//! one span per flush batch. Replay drives the same object offline via
//! `note_virtual_quiet` + `finish`, where wall-clock deadlines simply never
//! fire and verdicts settle at end of cast.

use std::time::{Duration, Instant};

use crate::config::Config;
use crate::emit::hold::{HOLD_CAP, HeldLine, Hold};
use crate::emit::segment::{RunKind, Segmenter};
use crate::emit::styler::Styler;
use crate::proxy::io_loop::ChunkTransform;
use crate::score::heuristic::HeuristicScorer;
use crate::term::caps::Caps;
use crate::term::finalize::Finalizer;
use crate::term::grid::ShadowGrid;
use crate::term::telemetry::SgrState;
use crate::types::{AbsRow, Finality};

const QUIET_GAP: Duration = Duration::from_millis(100);

pub struct StylingTransform {
    grid: ShadowGrid,
    fin: Finalizer,
    seg: Segmenter,
    hold: Hold,
    styler: Styler,
    scorer: HeuristicScorer,
    caps: Caps,
    hold_ms: Duration,
    last_activity: Option<Instant>,
    burst_open: bool,
    line_sgr: SgrState,
    reverse_used: bool,
}

impl StylingTransform {
    pub fn new(config: &Config, caps: Caps, rows: u16, cols: u16) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        Self {
            grid: ShadowGrid::new(rows, cols),
            fin: Finalizer::new(rows),
            seg: Segmenter::new(),
            hold: Hold::new(),
            styler: Styler::new(config.theme.clone(), caps, config.density_budget),
            scorer: HeuristicScorer::new(config),
            caps,
            hold_ms: Duration::from_millis(config.hold_ms),
            last_activity: None,
            burst_open: false,
            line_sgr: SgrState::default(),
            reverse_used: false,
        }
    }

    pub fn note_virtual_quiet(&mut self, out: &mut Vec<u8>) {
        if !self.caps.styling_enabled {
            return;
        }
        self.reverse_used = false;
        self.end_burst();
        self.drain_ready(out);
    }

    fn end_burst(&mut self) {
        self.fin.note_burst_end();
        self.grid.telemetry_mut().end_burst();
        self.burst_open = false;
    }

    fn ingest_events(&mut self) {
        let events = self.grid.telemetry_mut().take_events();
        self.fin.ingest(events);
    }

    fn process_text_piece(&mut self, piece: &[u8], now: Instant, out: &mut Vec<u8>) {
        if self.hold.is_line_start() {
            self.line_sgr = self.grid.telemetry().live_sgr().clone();
        }
        self.grid.process(piece);
        self.ingest_events();
        if self.grid.alt_screen_active() {
            self.flush_all(out);
            out.extend_from_slice(piece);
            return;
        }
        let deadline = now + self.hold_ms;
        if piece.last() == Some(&b'\n') {
            let (cursor_row, _) = self.grid.telemetry().cursor();
            let abs_after = self.grid.telemetry().scrolled_total() + u64::from(cursor_row);
            let row = AbsRow(abs_after.saturating_sub(1));
            let generation = self.grid.row_generation(row);
            if self
                .hold
                .complete_line(piece, row, generation, self.line_sgr.clone(), deadline)
            {
                out.extend_from_slice(piece);
            }
            self.drain_ready(out);
        } else if self.hold.push_partial(piece, deadline) {
            out.extend_from_slice(piece);
        }
    }

    fn drain_ready(&mut self, out: &mut Vec<u8>) {
        while let Some(front) = self.hold.front() {
            if self.fin.classify(front.row) != Finality::ScrollbackBound {
                break;
            }
            let line = self.hold.pop_front().expect("front exists");
            self.emit_resolved(line, out);
        }
    }

    fn emit_resolved(&mut self, line: HeldLine, out: &mut Vec<u8>) {
        let verdict = self.fin.classify(line.row);
        if verdict == Finality::ScrollbackBound
            && self.fin.is_still_valid(&self.grid, line.row, line.generation)
        {
            let allow_reverse = !self.reverse_used;
            let styled = self.styler.style_line(&line.bytes, &line.sgr_before, allow_reverse, |clean| {
                self.scorer.score_line(clean)
            });
            if let Some(styled) = styled {
                tracing::trace!(row = line.row.0, "line emitted styled");
                self.reverse_used |= styled.used_reverse;
                out.extend_from_slice(&styled.bytes);
                return;
            }
        } else {
            tracing::trace!(row = line.row.0, ?verdict, "line emitted raw");
        }
        out.extend_from_slice(&line.bytes);
    }

    fn flush_all(&mut self, out: &mut Vec<u8>) {
        while let Some(line) = self.hold.pop_front() {
            self.emit_resolved(line, out);
        }
        let partial = self.hold.take_partial_raw();
        out.extend_from_slice(&partial);
    }

    fn enforce_cap(&mut self, out: &mut Vec<u8>) {
        while self.hold.held_bytes() > HOLD_CAP {
            let Some(line) = self.hold.pop_front() else {
                break;
            };
            self.emit_resolved(line, out);
        }
    }
}

impl ChunkTransform for StylingTransform {
    fn transform(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        if !self.caps.styling_enabled {
            out.extend_from_slice(chunk);
            return;
        }
        let now = Instant::now();
        self.reverse_used = false;
        if self.burst_open
            && self
                .last_activity
                .is_some_and(|last| now.duration_since(last) >= QUIET_GAP)
        {
            self.end_burst();
            self.drain_ready(out);
        }
        self.last_activity = Some(now);
        self.burst_open = true;
        for run in self.seg.feed(chunk) {
            match run.kind {
                RunKind::Ctrl => {
                    self.flush_all(out);
                    self.grid.process(&run.bytes);
                    self.ingest_events();
                    out.extend_from_slice(&run.bytes);
                }
                RunKind::Text => {
                    let mut rest = run.bytes.as_slice();
                    while !rest.is_empty() {
                        let split = memchr::memchr(b'\n', rest).map_or(rest.len(), |i| i + 1);
                        let (piece, tail) = rest.split_at(split);
                        rest = tail;
                        self.process_text_piece(piece, now, out);
                    }
                }
            }
        }
        self.enforce_cap(out);
    }

    fn flush(&mut self, out: &mut Vec<u8>) {
        if !self.caps.styling_enabled {
            return;
        }
        let now = Instant::now();
        self.reverse_used = false;
        if self.burst_open
            && self
                .last_activity
                .is_some_and(|last| now.duration_since(last) >= QUIET_GAP)
        {
            self.end_burst();
        }
        while let Some(line) = self.hold.pop_expired(now) {
            self.emit_resolved(line, out);
        }
        if let Some(partial) = self.hold.partial_expired(now) {
            out.extend_from_slice(&partial);
        }
        self.drain_ready(out);
    }

    fn resize(&mut self, rows: u16, cols: u16, out: &mut Vec<u8>) {
        if !self.caps.styling_enabled {
            return;
        }
        self.reverse_used = false;
        self.flush_all(out);
        let rows = rows.max(1);
        let cols = cols.max(1);
        self.grid.resize(rows, cols);
        self.fin.resize(rows);
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        if !self.caps.styling_enabled {
            return;
        }
        self.reverse_used = false;
        if self.burst_open {
            self.end_burst();
        }
        self.flush_all(out);
        let pending = self.seg.take_pending();
        out.extend_from_slice(&pending);
    }
}
