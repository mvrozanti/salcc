//! Finalization detector: decides which printed rows are scrollback-bound
//! (safe to style exactly once at emission) versus inside the child's actively
//! repainted region. Correctness linchpin — a false ScrollbackBound is the
//! dangerous direction, so every heuristic here errs toward ActiveRegion or
//! Ambiguous. Zero false-finals across the conformance corpus is the bar.
//!
//! Event-driven: the io loop ingests `Telemetry::take_events()` after every
//! processed chunk and calls `note_burst_end()` on quiet gaps. From the event
//! stream the finalizer accumulates its own `scroll_base` (sum of full-region
//! Scroll events, mirroring the grid's counter), alt-screen and sync-update
//! flags, and dynamic-region evidence.
//!
//! Classification:
//! - Alt screen active: everything is ActiveRegion (bit-perfect passthrough).
//! - `abs_row < scroll_base`: ScrollbackBound unconditionally — vt100
//!   scrollback rows are immutable, so this can never be a false final.
//! - Visible rows: ScrollbackBound only above the dynamic-region top,
//!   ActiveRegion at or below it, Ambiguous when evidence is missing or
//!   suspect (sync update in progress, recent disruption).
//!
//! Dynamic-region top (`dyn_top_abs`) deviates from the naive "min explicit
//! cursor-target row of the last completed burst" in three deliberate,
//! strictly-more-conservative ways:
//! - Minima are tracked in absolute coordinates captured at event time (the
//!   telemetry contract), so scrolls later in a burst cannot raise them.
//! - The min is taken over the last TWO completed bursts, hedging against a
//!   one-burst dip in the child's repaint height (Ink frames fluctuate).
//! - The running burst's minimum can lower dyn_top mid-burst but never counts
//!   as sole evidence, because an unfinished burst may still target higher.
//! Evidence rows come from CursorTarget, EraseLine, and EraseDisplay-below
//! events.
//!
//! Disruptions (region scrolls, reverse scrolls, IL/DL shifts, whole/above
//! erases, RIS) rewrite visible rows in ways row evidence does not model, so
//! they clear all evidence and suppress visible-row finalization until a
//! clean burst completes after the disrupted one. Scrolled rows stay final
//! throughout.

use crate::term::grid::ShadowGrid;
use crate::term::telemetry::TermEvent;
use crate::types::{AbsRow, Finality};

#[derive(Debug)]
pub struct Finalizer {
    rows: u16,
    scroll_base: u64,
    alt_screen: bool,
    sync_active: bool,
    cur_min_abs: Option<u64>,
    last_min_abs: Option<u64>,
    prev_min_abs: Option<u64>,
    cur_disrupted: bool,
    last_disrupted: bool,
}

impl Finalizer {
    pub fn new(rows: u16) -> Self {
        Self {
            rows,
            scroll_base: 0,
            alt_screen: false,
            sync_active: false,
            cur_min_abs: None,
            last_min_abs: None,
            prev_min_abs: None,
            cur_disrupted: false,
            last_disrupted: false,
        }
    }

    pub fn resize(&mut self, rows: u16) {
        self.rows = rows;
    }

    pub fn ingest(&mut self, events: impl IntoIterator<Item = TermEvent>) {
        for event in events {
            match event {
                TermEvent::CursorTarget { abs_row, .. } => self.note_evidence(abs_row),
                TermEvent::EraseLine { at_abs, .. } => self.note_evidence(at_abs),
                TermEvent::EraseDisplay { mode: 0, at_abs } => self.note_evidence(at_abs),
                TermEvent::EraseDisplay { mode, at_abs } => {
                    tracing::debug!(mode, at_abs, "disruptive erase; suppressing finalization");
                    self.note_disruption();
                }
                TermEvent::Scroll { lines } => self.scroll_base += lines,
                TermEvent::RegionScroll { .. }
                | TermEvent::ReverseScroll { .. }
                | TermEvent::LinesShifted { .. } => {
                    tracing::debug!(?event, "line shift; suppressing finalization");
                    self.note_disruption();
                }
                TermEvent::AltScreen { active } => self.alt_screen = active,
                TermEvent::SyncUpdate { active } => self.sync_active = active,
                TermEvent::Reset => {
                    tracing::debug!("terminal reset; suppressing finalization");
                    self.note_disruption();
                }
            }
        }
    }

    pub fn note_burst_end(&mut self) {
        self.prev_min_abs = self.last_min_abs;
        self.last_min_abs = self.cur_min_abs.take();
        self.last_disrupted = self.cur_disrupted;
        self.cur_disrupted = false;
        tracing::trace!(
            last_min_abs = ?self.last_min_abs,
            prev_min_abs = ?self.prev_min_abs,
            last_disrupted = self.last_disrupted,
            scroll_base = self.scroll_base,
            "burst ended"
        );
    }

    pub fn classify(&self, row: AbsRow) -> Finality {
        if self.alt_screen {
            return Finality::ActiveRegion;
        }
        if row.0 < self.scroll_base {
            return Finality::ScrollbackBound;
        }
        if self.sync_active || self.cur_disrupted || self.last_disrupted {
            return Finality::Ambiguous;
        }
        let Some(dyn_top) = self.dyn_top_abs() else {
            return Finality::Ambiguous;
        };
        if row.0 < dyn_top {
            return Finality::ScrollbackBound;
        }
        if row.0 < self.scroll_base + u64::from(self.rows) {
            return Finality::ActiveRegion;
        }
        Finality::Ambiguous
    }

    pub fn is_still_valid(
        &self,
        grid: &ShadowGrid,
        row: AbsRow,
        generation_at_style_time: u64,
    ) -> bool {
        grid.row_generation(row) == generation_at_style_time
    }

    pub fn dyn_top_abs(&self) -> Option<u64> {
        let completed = [self.last_min_abs, self.prev_min_abs]
            .into_iter()
            .flatten()
            .min()?;
        Some(self.cur_min_abs.map_or(completed, |cur| cur.min(completed)))
    }

    pub fn scroll_base(&self) -> u64 {
        self.scroll_base
    }

    pub fn alt_screen_active(&self) -> bool {
        self.alt_screen
    }

    fn note_evidence(&mut self, abs_row: u64) {
        self.cur_min_abs = Some(self.cur_min_abs.map_or(abs_row, |m| m.min(abs_row)));
    }

    fn note_disruption(&mut self) {
        self.cur_disrupted = true;
        self.cur_min_abs = None;
        self.last_min_abs = None;
        self.prev_min_abs = None;
    }
}
