//! Second cheap vte pass over the exact child byte stream, observing raw
//! events vt100 hides: explicit cursor targets, scroll signals, erases,
//! alt-screen and sync-update mode flips, and live SGR state.
//!
//! Contract: cursor/scroll modeling mirrors vt100 0.16.2's semantics exactly
//! (not xterm's), because the shadow grid IS vt100 — `scrolled_total()` must
//! equal the number of rows vt100 pushed into primary-screen scrollback.
//! Mirrored quirks: HVP (CSI f) does not move the cursor (vt100 ignores it;
//! we record it as burst-min evidence only), DECAWM is ignored (vt100 always
//! autowraps), RI/SD never push scrollback (reported as ReverseScroll).
//!
//! Bursts are time-domain: telemetry sees bytes, not gaps, so the caller
//! signals `end_burst()` on quiet gaps. Burst minima are tracked in absolute
//! row coordinates captured at event time, so scrolls occurring later in the
//! same burst cannot skew the dynamic-region top downward in history.

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermEvent {
    CursorTarget { row: u16, col: u16, abs_row: u64 },
    Scroll { lines: u64 },
    RegionScroll { top_abs: u64, lines: u16 },
    ReverseScroll { lines: u16 },
    LinesShifted { at_abs: u64 },
    EraseDisplay { mode: u16, at_abs: u64 },
    EraseLine { mode: u16, at_abs: u64 },
    AltScreen { active: bool },
    SyncUpdate { active: bool },
    Reset,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SgrState {
    pub fg: Option<Vec<u16>>,
    pub bg: Option<Vec<u16>>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
}

impl SgrState {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn restore_sequence(&self) -> Vec<u8> {
        let mut parts: Vec<String> = vec!["0".to_owned()];
        let flags: [(bool, &str); 8] = [
            (self.bold, "1"),
            (self.dim, "2"),
            (self.italic, "3"),
            (self.underline, "4"),
            (self.blink, "5"),
            (self.inverse, "7"),
            (self.hidden, "8"),
            (self.strikethrough, "9"),
        ];
        for (on, code) in flags {
            if on {
                parts.push(code.to_owned());
            }
        }
        for color in [&self.fg, &self.bg].into_iter().flatten() {
            parts.push(
                color
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(";"),
            );
        }
        format!("\x1b[{}m", parts.join(";")).into_bytes()
    }
}

#[derive(Debug, Clone, Copy)]
struct GridModel {
    row: u16,
    col: u16,
    saved_row: u16,
    saved_col: u16,
    scroll_top: u16,
    scroll_bottom: u16,
    origin_mode: bool,
    saved_origin_mode: bool,
}

impl GridModel {
    fn new(rows: u16) -> Self {
        Self {
            row: 0,
            col: 0,
            saved_row: 0,
            saved_col: 0,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            origin_mode: false,
            saved_origin_mode: false,
        }
    }

    fn in_scroll_region(&self) -> bool {
        self.row >= self.scroll_top && self.row <= self.scroll_bottom
    }

    fn full_region(&self, rows: u16) -> bool {
        self.scroll_top == 0 && self.scroll_bottom == rows.saturating_sub(1)
    }

    fn row_clamp_top(&mut self, limit_to_scroll_region: bool) -> u16 {
        if limit_to_scroll_region && self.row < self.scroll_top {
            let lines = self.scroll_top - self.row;
            self.row = self.scroll_top;
            lines
        } else {
            0
        }
    }

    fn row_clamp_bottom(&mut self, limit_to_scroll_region: bool, rows: u16) -> u16 {
        let bottom = if limit_to_scroll_region {
            self.scroll_bottom
        } else {
            rows.saturating_sub(1)
        };
        if self.row > bottom {
            let lines = self.row - bottom;
            self.row = bottom;
            lines
        } else {
            0
        }
    }

    fn col_clamp(&mut self, cols: u16) {
        self.col = self.col.min(cols.saturating_sub(1));
    }
}

#[derive(Debug)]
struct TelemetryState {
    rows: u16,
    cols: u16,
    primary: GridModel,
    alternate: GridModel,
    alt_screen: bool,
    sync_active: bool,
    scrolled_total: u64,
    events: Vec<TermEvent>,
    sgr: SgrState,
    saved_sgr: SgrState,
    burst_min_row: Option<u16>,
    burst_min_abs: Option<u64>,
    last_burst_min_row: Option<u16>,
    last_burst_min_abs: Option<u64>,
    reset_seen: bool,
}

impl TelemetryState {
    fn new(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            primary: GridModel::new(rows),
            alternate: GridModel::new(rows),
            alt_screen: false,
            sync_active: false,
            scrolled_total: 0,
            events: Vec::new(),
            sgr: SgrState::default(),
            saved_sgr: SgrState::default(),
            burst_min_row: None,
            burst_min_abs: None,
            last_burst_min_row: None,
            last_burst_min_abs: None,
            reset_seen: false,
        }
    }

    fn gm(&mut self) -> &mut GridModel {
        if self.alt_screen {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    fn gm_ref(&self) -> &GridModel {
        if self.alt_screen {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn scroll_up(&mut self, count: u16) {
        let rows = self.rows;
        let scroll_top = self.gm_ref().scroll_top;
        let full = self.gm_ref().full_region(rows);
        let count = count.min(rows.saturating_sub(scroll_top));
        if count == 0 || self.alt_screen {
            return;
        }
        if full {
            self.scrolled_total += u64::from(count);
            self.events.push(TermEvent::Scroll {
                lines: u64::from(count),
            });
        } else {
            self.events.push(TermEvent::RegionScroll {
                top_abs: self.scrolled_total + u64::from(scroll_top),
                lines: count,
            });
        }
    }

    fn scroll_down(&mut self, count: u16) {
        if count > 0 && !self.alt_screen {
            self.events.push(TermEvent::ReverseScroll { lines: count });
        }
    }

    fn row_inc_scroll(&mut self, count: u16) {
        let rows = self.rows;
        let g = self.gm();
        let in_region = g.in_scroll_region();
        g.row = g.row.saturating_add(count);
        let lines = g.row_clamp_bottom(in_region, rows);
        if in_region {
            self.scroll_up(lines);
        }
    }

    fn row_inc_clamp(&mut self, count: u16) {
        let rows = self.rows;
        let g = self.gm();
        let in_region = g.in_scroll_region();
        g.row = g.row.saturating_add(count);
        g.row_clamp_bottom(in_region, rows);
    }

    fn row_dec_scroll(&mut self, count: u16) {
        let g = self.gm();
        let in_region = g.in_scroll_region();
        let extra_lines = count.saturating_sub(g.row);
        g.row = g.row.saturating_sub(count);
        let lines = g.row_clamp_top(in_region);
        self.scroll_down(lines + extra_lines);
    }

    fn row_dec_clamp(&mut self, count: u16) {
        let g = self.gm();
        let in_region = g.in_scroll_region();
        g.row = g.row.saturating_sub(count);
        g.row_clamp_top(in_region);
    }

    fn set_pos(&mut self, row: u16, col: u16) {
        let rows = self.rows;
        let cols = self.cols;
        let g = self.gm();
        g.row = if g.origin_mode {
            row.saturating_add(g.scroll_top)
        } else {
            row
        };
        g.col = col;
        let origin = g.origin_mode;
        g.row_clamp_top(origin);
        g.row_clamp_bottom(origin, rows);
        g.col_clamp(cols);
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let rows = self.rows;
        let g = self.gm();
        let bottom = bottom.min(rows.saturating_sub(1));
        if top < bottom {
            g.scroll_top = top;
            g.scroll_bottom = bottom;
        } else {
            g.scroll_top = 0;
            g.scroll_bottom = rows.saturating_sub(1);
        }
        g.row = g.scroll_top;
        g.col = 0;
    }

    fn note_explicit_target(&mut self) {
        if self.alt_screen {
            return;
        }
        let row = self.gm_ref().row;
        let col = self.gm_ref().col;
        let abs_row = self.scrolled_total + u64::from(row);
        self.events
            .push(TermEvent::CursorTarget { row, col, abs_row });
        self.note_row_evidence(row, abs_row);
    }

    fn note_row_evidence(&mut self, row: u16, abs_row: u64) {
        self.burst_min_row = Some(self.burst_min_row.map_or(row, |m| m.min(row)));
        self.burst_min_abs = Some(self.burst_min_abs.map_or(abs_row, |m| m.min(abs_row)));
    }

    fn cursor_abs(&self) -> u64 {
        self.scrolled_total + u64::from(self.gm_ref().row)
    }

    fn erase_mode(params: &Params) -> u16 {
        params.iter().next().map_or(0, |p| *p.first().unwrap_or(&0))
    }

    fn push_erase_display(&mut self, params: &Params) {
        if !self.alt_screen {
            let at_abs = self.cursor_abs();
            self.events.push(TermEvent::EraseDisplay {
                mode: Self::erase_mode(params),
                at_abs,
            });
        }
    }

    fn push_erase_line(&mut self, params: &Params) {
        if !self.alt_screen {
            let at_abs = self.cursor_abs();
            self.events.push(TermEvent::EraseLine {
                mode: Self::erase_mode(params),
                at_abs,
            });
        }
    }

    fn save_cursor(&mut self) {
        let saved = self.sgr.clone();
        let g = self.gm();
        g.saved_row = g.row;
        g.saved_col = g.col;
        g.saved_origin_mode = g.origin_mode;
        self.saved_sgr = saved;
    }

    fn restore_cursor(&mut self) {
        let restored = self.saved_sgr.clone();
        let g = self.gm();
        g.row = g.saved_row;
        g.col = g.saved_col;
        g.origin_mode = g.saved_origin_mode;
        self.sgr = restored;
    }

    fn reset(&mut self) {
        self.primary = GridModel::new(self.rows);
        self.alternate = GridModel::new(self.rows);
        self.alt_screen = false;
        self.sync_active = false;
        self.sgr = SgrState::default();
        self.saved_sgr = SgrState::default();
        self.reset_seen = true;
        self.events.push(TermEvent::Reset);
    }

    fn set_alt_screen(&mut self, active: bool) {
        if self.alt_screen != active {
            self.alt_screen = active;
            self.events.push(TermEvent::AltScreen { active });
        }
    }

    fn decset_decrst(&mut self, params: &Params, set: bool) {
        let modes: Vec<u16> = params.iter().filter_map(|p| p.first().copied()).collect();
        for mode in modes {
            match (mode, set) {
                (47, _) => self.set_alt_screen(set),
                (1049, true) => {
                    self.save_cursor();
                    self.alternate = GridModel::new(self.rows);
                    self.set_alt_screen(true);
                }
                (1049, false) => {
                    self.set_alt_screen(false);
                    self.restore_cursor();
                }
                (6, _) => {
                    self.gm().origin_mode = set;
                    self.set_pos(0, 0);
                }
                (2026, _) => {
                    if self.sync_active != set {
                        self.sync_active = set;
                        self.events.push(TermEvent::SyncUpdate { active: set });
                    }
                }
                _ => {}
            }
        }
    }

    fn sgr_update(&mut self, params: &Params) {
        if params.is_empty() {
            self.sgr = SgrState::default();
            return;
        }
        let mut iter = params.iter();
        while let Some(param) = iter.next() {
            match param {
                [] | [0] => self.sgr = SgrState::default(),
                [1] => self.sgr.bold = true,
                [2] => self.sgr.dim = true,
                [3] => self.sgr.italic = true,
                [4] => self.sgr.underline = true,
                [4, style, ..] => self.sgr.underline = *style != 0,
                [5 | 6] => self.sgr.blink = true,
                [7] => self.sgr.inverse = true,
                [8] => self.sgr.hidden = true,
                [9] => self.sgr.strikethrough = true,
                [21] => self.sgr.underline = true,
                [22] => {
                    self.sgr.bold = false;
                    self.sgr.dim = false;
                }
                [23] => self.sgr.italic = false,
                [24] => self.sgr.underline = false,
                [25] => self.sgr.blink = false,
                [27] => self.sgr.inverse = false,
                [28] => self.sgr.hidden = false,
                [29] => self.sgr.strikethrough = false,
                [n] if (30..=37).contains(n) || (90..=97).contains(n) => {
                    self.sgr.fg = Some(vec![*n]);
                }
                [38, rest @ ..] if !rest.is_empty() => {
                    self.sgr.fg = Some(param.to_vec());
                }
                [38] => match Self::extended_color(&mut iter, 38) {
                    Some(color) => self.sgr.fg = Some(color),
                    None => return,
                },
                [39] => self.sgr.fg = None,
                [n] if (40..=47).contains(n) || (100..=107).contains(n) => {
                    self.sgr.bg = Some(vec![*n]);
                }
                [48, rest @ ..] if !rest.is_empty() => {
                    self.sgr.bg = Some(param.to_vec());
                }
                [48] => match Self::extended_color(&mut iter, 48) {
                    Some(color) => self.sgr.bg = Some(color),
                    None => return,
                },
                [49] => self.sgr.bg = None,
                _ => {}
            }
        }
    }

    fn extended_color(iter: &mut vte::ParamsIter<'_>, base: u16) -> Option<Vec<u16>> {
        match iter.next()? {
            [2] => {
                let r = *iter.next()?.first()?;
                let g = *iter.next()?.first()?;
                let b = *iter.next()?.first()?;
                Some(vec![base, 2, r, g, b])
            }
            [5] => {
                let i = *iter.next()?.first()?;
                Some(vec![base, 5, i])
            }
            _ => None,
        }
    }
}

fn param1(params: &Params, default: u16) -> u16 {
    let first = params.iter().next().map_or(0, |p| *p.first().unwrap_or(&0));
    if first == 0 { default } else { first }
}

fn param2(params: &Params, default1: u16, default2: u16) -> (u16, u16) {
    let mut iter = params.iter();
    let first = iter.next().map_or(0, |p| *p.first().unwrap_or(&0));
    let second = iter.next().map_or(0, |p| *p.first().unwrap_or(&0));
    (
        if first == 0 { default1 } else { first },
        if second == 0 { default2 } else { second },
    )
}

impl Perform for TelemetryState {
    fn print(&mut self, c: char) {
        if c == '\u{fffd}' || ('\u{80}'..'\u{a0}').contains(&c) {
            return;
        }
        let width = match c.width() {
            None => {
                if u32::from(c) < 256 {
                    return;
                }
                1
            }
            Some(w) => w,
        };
        let Ok(width) = u16::try_from(width) else {
            return;
        };
        if width == 0 {
            return;
        }
        let cols = self.cols;
        if self.gm_ref().col > cols.saturating_sub(width) {
            self.gm().col = 0;
            self.row_inc_scroll(1);
        }
        let g = self.gm();
        g.col = g.col.saturating_add(width);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            8 => {
                let g = self.gm();
                g.col = g.col.saturating_sub(1);
            }
            9 => {
                let cols = self.cols;
                let g = self.gm();
                g.col -= g.col % 8;
                g.col = g.col.saturating_add(8);
                g.col_clamp(cols);
            }
            10..=12 => self.row_inc_scroll(1),
            13 => self.gm().col = 0,
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'M' => {
                self.row_dec_scroll(1);
                self.note_explicit_target();
            }
            b'c' => self.reset(),
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        match intermediates.first() {
            None => match action {
                'A' => {
                    self.row_dec_clamp(param1(params, 1));
                    self.note_explicit_target();
                }
                'B' => {
                    self.row_inc_clamp(param1(params, 1));
                    self.note_explicit_target();
                }
                'C' => {
                    let cols = self.cols;
                    let g = self.gm();
                    g.col = g.col.saturating_add(param1(params, 1));
                    g.col_clamp(cols);
                }
                'D' => {
                    let n = param1(params, 1);
                    let g = self.gm();
                    g.col = g.col.saturating_sub(n);
                }
                'E' => {
                    self.gm().col = 0;
                    self.row_inc_clamp(param1(params, 1));
                    self.note_explicit_target();
                }
                'F' => {
                    self.gm().col = 0;
                    self.row_dec_clamp(param1(params, 1));
                    self.note_explicit_target();
                }
                'G' => {
                    let cols = self.cols;
                    let n = param1(params, 1);
                    let g = self.gm();
                    g.col = n.saturating_sub(1);
                    g.col_clamp(cols);
                }
                'H' => {
                    let (row, col) = param2(params, 1, 1);
                    self.set_pos(row - 1, col - 1);
                    self.note_explicit_target();
                }
                'f' => {
                    let (row, _col) = param2(params, 1, 1);
                    let g = self.gm_ref();
                    let target = if g.origin_mode {
                        (row - 1).saturating_add(g.scroll_top)
                    } else {
                        row - 1
                    }
                    .min(self.rows.saturating_sub(1));
                    if !self.alt_screen {
                        let abs = self.scrolled_total + u64::from(target);
                        self.note_row_evidence(target, abs);
                    }
                }
                'J' => self.push_erase_display(params),
                'K' => self.push_erase_line(params),
                'L' | 'M' => {
                    if !self.alt_screen {
                        let at_abs = self.cursor_abs();
                        self.events.push(TermEvent::LinesShifted { at_abs });
                    }
                }
                'S' => self.scroll_up(param1(params, 1)),
                'T' => self.scroll_down(param1(params, 1)),
                'd' => {
                    let rows = self.rows;
                    let n = param1(params, 1);
                    let g = self.gm();
                    g.row = n.saturating_sub(1).min(rows.saturating_sub(1));
                    self.note_explicit_target();
                }
                'm' => self.sgr_update(params),
                'r' => {
                    let (top, bottom) = param2(params, 1, self.rows);
                    self.set_scroll_region(top - 1, bottom - 1);
                }
                _ => {}
            },
            Some(b'?') => match action {
                'h' => self.decset_decrst(params, true),
                'l' => self.decset_decrst(params, false),
                'J' => self.push_erase_display(params),
                'K' => self.push_erase_line(params),
                _ => {}
            },
            _ => {}
        }
    }
}

pub struct Telemetry {
    parser: vte::Parser,
    state: TelemetryState,
}

impl std::fmt::Debug for Telemetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Telemetry").field("state", &self.state).finish_non_exhaustive()
    }
}

impl Telemetry {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vte::Parser::new(),
            state: TelemetryState::new(rows, cols),
        }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        let Self { parser, state } = self;
        parser.advance(state, bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let old_rows = self.state.rows;
        for g in [&mut self.state.primary, &mut self.state.alternate] {
            if g.scroll_bottom == old_rows.saturating_sub(1) || g.scroll_bottom >= rows {
                g.scroll_bottom = rows.saturating_sub(1);
            }
            if g.scroll_bottom < g.scroll_top {
                g.scroll_top = 0;
            }
            g.row = g.row.min(rows.saturating_sub(1));
            g.col = g.col.min(cols.saturating_sub(1));
            g.saved_row = g.saved_row.min(rows.saturating_sub(1));
            g.saved_col = g.saved_col.min(cols.saturating_sub(1));
        }
        self.state.rows = rows;
        self.state.cols = cols;
    }

    pub fn take_events(&mut self) -> Vec<TermEvent> {
        std::mem::take(&mut self.state.events)
    }

    pub fn alt_screen_active(&self) -> bool {
        self.state.alt_screen
    }

    pub fn sync_update_active(&self) -> bool {
        self.state.sync_active
    }

    pub fn live_sgr(&self) -> &SgrState {
        &self.state.sgr
    }

    pub fn min_cursor_row_this_burst(&self) -> Option<u16> {
        self.state.burst_min_row
    }

    pub fn min_abs_cursor_row_this_burst(&self) -> Option<u64> {
        self.state.burst_min_abs
    }

    pub fn last_burst_min_cursor_row(&self) -> Option<u16> {
        self.state.last_burst_min_row
    }

    pub fn last_burst_min_abs_cursor_row(&self) -> Option<u64> {
        self.state.last_burst_min_abs
    }

    pub fn end_burst(&mut self) {
        self.state.last_burst_min_row = self.state.burst_min_row.take();
        self.state.last_burst_min_abs = self.state.burst_min_abs.take();
    }

    pub fn scrolled_total(&self) -> u64 {
        self.state.scrolled_total
    }

    pub fn reset_seen(&self) -> bool {
        self.state.reset_seen
    }

    pub fn cursor(&self) -> (u16, u16) {
        let g = self.state.gm_ref();
        (g.row, g.col)
    }
}
