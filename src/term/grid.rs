//! Shadow vt100 grid: the single source of truth for what is on screen.
//!
//! Composition: `ShadowGrid` owns both the `vt100::Parser` and the raw-vte
//! `Telemetry` pass and feeds every child byte to both inside `process()`,
//! guaranteeing the two passes can never observe different byte streams.
//! Consumers reach the event stream through `telemetry_mut().take_events()`.
//!
//! Scroll accounting: vt100 exposes no scroll callback, so absolute row ids
//! are derived from `Telemetry::scrolled_total()`, whose contract is to mirror
//! vt100 0.16.2's scrollback-push semantics exactly (full-region primary
//! scrolls only; alt-screen, DECSTBM-region scrolls and RI push nothing).
//! `scroll_base()` is that counter; `abs_row = scroll_base + viewport_row`.
//! The conformance suite cross-checks the counter against vt100's actual
//! scrollback length on every fixture.
//!
//! Generation tracking: after each `process()` call every visible row's text
//! (plus its wrap flag) is hashed and compared against the last hash recorded
//! for that absolute row; any change bumps the row's generation. This
//! over-counts (a row streaming in bumps once per chunk) but can never miss a
//! rewrite, which is the direction that matters: `Finalizer::is_still_valid`
//! uses generations to invalidate stale overlays. Rows that scroll off stop
//! being hashed and their generation freezes, which is sound because vt100
//! scrollback rows are immutable. While the alt screen is active the primary
//! grid is frozen and hashing is skipped, so alt-screen redraws never touch
//! primary-row generations.
//!
//! `row_text` for scrolled-off rows works by temporarily setting the vt100
//! scrollback view offset and restoring it to zero before returning; rows
//! older than `SCROLLBACK_CAPACITY` (or discarded by RIS) return `None`.
//! Row marks are kept for every absolute row ever seen (~48 bytes/row);
//! pruning below the emission horizon is deferred to M2.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::term::telemetry::Telemetry;
use crate::types::AbsRow;

const SCROLLBACK_CAPACITY: usize = 10_000;

#[derive(Debug, Clone, Copy)]
struct RowMark {
    hash: u64,
    generation: u64,
}

pub struct ShadowGrid {
    parser: vt100::Parser,
    telemetry: Telemetry,
    row_marks: HashMap<u64, RowMark>,
}

impl std::fmt::Debug for ShadowGrid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowGrid")
            .field("telemetry", &self.telemetry)
            .field("tracked_rows", &self.row_marks.len())
            .finish_non_exhaustive()
    }
}

impl ShadowGrid {
    pub fn new(rows: u16, cols: u16) -> Self {
        let mut grid = Self {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_CAPACITY),
            telemetry: Telemetry::new(rows, cols),
            row_marks: HashMap::new(),
        };
        grid.refresh_row_marks();
        grid
    }

    pub fn process(&mut self, bytes: &[u8]) {
        self.telemetry.process(bytes);
        self.parser.process(bytes);
        self.refresh_row_marks();
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        tracing::debug!(rows, cols, "shadow grid resize");
        self.telemetry.resize(rows, cols);
        self.parser.screen_mut().set_size(rows, cols);
        self.refresh_row_marks();
    }

    pub fn scroll_base(&self) -> u64 {
        self.telemetry.scrolled_total()
    }

    pub fn abs_row_of_visible(&self, visible_row: u16) -> AbsRow {
        AbsRow(self.scroll_base() + u64::from(visible_row))
    }

    pub fn visible_row_of(&self, row: AbsRow) -> Option<u16> {
        let offset = row.0.checked_sub(self.scroll_base())?;
        let (rows, _) = self.parser.screen().size();
        u16::try_from(offset).ok().filter(|r| *r < rows)
    }

    pub fn row_text(&mut self, row: AbsRow) -> Option<String> {
        if self.parser.screen().alternate_screen() {
            return None;
        }
        let (_, cols) = self.parser.screen().size();
        if let Some(visible) = self.visible_row_of(row) {
            return self.parser.screen().rows(0, cols).nth(usize::from(visible));
        }
        let back = usize::try_from(self.scroll_base().checked_sub(row.0)?).ok()?;
        let screen = self.parser.screen_mut();
        screen.set_scrollback(usize::MAX);
        let available = screen.scrollback();
        let text = if back <= available {
            screen.set_scrollback(back);
            self.parser.screen().rows(0, cols).next()
        } else {
            None
        };
        self.parser.screen_mut().set_scrollback(0);
        text
    }

    pub fn row_generation(&self, row: AbsRow) -> u64 {
        self.row_marks.get(&row.0).map_or(0, |mark| mark.generation)
    }

    pub fn cursor(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    pub fn alt_screen_active(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    pub fn telemetry_mut(&mut self) -> &mut Telemetry {
        &mut self.telemetry
    }

    fn refresh_row_marks(&mut self) {
        if self.parser.screen().alternate_screen() {
            return;
        }
        let base = self.scroll_base();
        let (_, cols) = self.parser.screen().size();
        let screen = self.parser.screen();
        for (idx, text) in (0u16..).zip(screen.rows(0, cols)) {
            let mut hasher = DefaultHasher::new();
            text.hash(&mut hasher);
            screen.row_wrapped(idx).hash(&mut hasher);
            let hash = hasher.finish();
            match self.row_marks.entry(base + u64::from(idx)) {
                Entry::Occupied(mut entry) => {
                    let mark = entry.get_mut();
                    if mark.hash != hash {
                        mark.hash = hash;
                        mark.generation += 1;
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(RowMark {
                        hash,
                        generation: 0,
                    });
                }
            }
        }
    }
}
