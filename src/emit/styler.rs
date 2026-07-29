//! Emission-time SGR merge. Takes a held line's raw bytes (printable text +
//! child SGR + trailing CR/LF only), recovers the clean text and the child's
//! SGR state at every position, scores the clean text, and re-emits the line
//! with facet styling layered on top. Hard rules enforced here:
//! - Output differs from input in SGR attributes only; the sole text change
//!   is the audited gutter swap (2-space pad -> width-1 glyph + space).
//! - Luminance is ours (dim for de-emphasis, bold for weight >= 0.7); hue
//!   applies only where the child left default fg, except Risk always wins.
//! - Undercurl (4:3, 58;2;r;g;b) only when caps allow, else underline.
//! - Density budget: emphasis chars capped at density_budget of the line's
//!   visible chars, dropping lowest-weight spans first. Two floors: Risk
//!   spans are exempt (invariant 7: near-1.0 risk recall, never vetoed) and
//!   the single highest-weight emphasis span always survives — a lone
//!   isolated emphasis is exactly the von Restorff pattern the budget
//!   exists to protect, and 12% of one 80-column line is shorter than any
//!   phrase-level span, so a literal per-line cap would zero out every
//!   facet the block-level evidence (docs/research.md) argues for.
//! - Reverse video: at most one Risk span (weight >= 0.9) per flush batch.
//! - Every styled region exits by restoring the child's exact SGR state, so
//!   the line always ends in the child's live attributes.
//! Any line the extractor cannot model losslessly (interior CR, stray
//! control bytes, invalid UTF-8) is refused: `None` means emit raw.
//!
//! The facet SGR vocabulary (`facet_sgr_params`, `fg_params`,
//! `underline_param`, `gutter_glyph_slot`) is exposed as free functions so
//! term/repaint.rs layers the exact same styling rules — one vocabulary,
//! two channels.

use std::ops::Range;

use crate::config::theme::{Rgb, Theme};
use crate::term::caps::{Caps, ColorDepth};
use crate::term::telemetry::SgrState;
use crate::types::{Facet, SpanScore};

pub struct Styler {
    theme: Theme,
    caps: Caps,
    density_budget: f32,
}

pub struct StyledLine {
    pub bytes: Vec<u8>,
    pub used_reverse: bool,
}

struct Extract {
    clean: String,
    events: Vec<SgrEvent>,
    terminator: Vec<u8>,
}

struct SgrEvent {
    clean_ofs: usize,
    raw: Vec<u8>,
    params_start: usize,
}

impl Styler {
    pub fn new(theme: Theme, caps: Caps, density_budget: f32) -> Self {
        Self {
            theme,
            caps,
            density_budget,
        }
    }

    pub fn style_line(
        &self,
        raw: &[u8],
        sgr_before: &SgrState,
        allow_reverse: bool,
        score: impl FnOnce(&str) -> Vec<SpanScore>,
    ) -> Option<StyledLine> {
        if !self.caps.styling_enabled {
            return None;
        }
        let extract = extract(raw)?;
        if extract.clean.trim().is_empty() {
            return None;
        }
        let mut spans = self.usable_spans(score(&extract.clean), &extract.clean);
        self.enforce_density(&mut spans, &extract.clean);
        if spans.is_empty() {
            return None;
        }
        let reverse_idx = if allow_reverse {
            best_reverse_candidate(&spans)
        } else {
            None
        };
        let glyph = self.gutter_glyph(&spans, &extract.clean);
        Some(self.render(&extract, sgr_before, &spans, reverse_idx, glyph))
    }

    fn usable_spans(&self, mut spans: Vec<SpanScore>, clean: &str) -> Vec<SpanScore> {
        spans.retain(|s| {
            s.range.end <= clean.len()
                && clean.is_char_boundary(s.range.start)
                && clean.is_char_boundary(s.range.end)
                && s.range.start < s.range.end
        });
        spans.sort_by(|a, b| b.weight.total_cmp(&a.weight).then(a.range.start.cmp(&b.range.start)));
        let mut kept: Vec<SpanScore> = Vec::new();
        for span in spans {
            if kept.iter().all(|k| !ranges_overlap(&k.range, &span.range)) {
                kept.push(span);
            }
        }
        kept.sort_by_key(|s| s.range.start);
        kept
    }

    fn enforce_density(&self, spans: &mut Vec<SpanScore>, clean: &str) {
        let visible = clean.chars().count().max(1);
        let budget = self.density_budget * visible as f32;
        loop {
            let emphasis: Vec<usize> = spans
                .iter()
                .enumerate()
                .filter(|(_, s)| s.facet.is_emphasis())
                .map(|(i, _)| i)
                .collect();
            let emphasized: usize = emphasis
                .iter()
                .map(|&i| clean[spans[i].range.clone()].chars().count())
                .sum();
            if emphasized as f32 <= budget || emphasis.len() <= 1 {
                return;
            }
            let victim = emphasis
                .iter()
                .copied()
                .filter(|&i| spans[i].facet != Facet::Risk)
                .min_by(|&a, &b| spans[a].weight.total_cmp(&spans[b].weight));
            match victim {
                Some(i) => {
                    spans.remove(i);
                }
                None => return,
            }
        }
    }

    fn gutter_glyph(&self, spans: &[SpanScore], clean: &str) -> Option<(char, Facet)> {
        if !gutter_glyph_slot(clean) {
            return None;
        }
        let dominant = spans
            .iter()
            .filter(|s| s.facet.is_emphasis())
            .max_by(|a, b| a.weight.total_cmp(&b.weight))?;
        let glyph = self.theme.for_facet(&dominant.facet)?.gutter_glyph()?;
        Some((glyph, dominant.facet.clone()))
    }

    fn render(
        &self,
        extract: &Extract,
        sgr_before: &SgrState,
        spans: &[SpanScore],
        reverse_idx: Option<usize>,
        glyph: Option<(char, Facet)>,
    ) -> StyledLine {
        let clean = extract.clean.as_bytes();
        let mut out = Vec::with_capacity(clean.len() * 2);
        let mut state = sgr_before.clone();
        let mut event_idx = 0;
        let mut span_idx = 0;
        let mut active_end: Option<usize> = None;
        let mut used_reverse = false;
        for ofs in 0..=clean.len() {
            if active_end == Some(ofs) {
                out.extend_from_slice(&state.restore_sequence());
                active_end = None;
            }
            while let Some(event) = extract.events.get(event_idx).filter(|e| e.clean_ofs == ofs) {
                out.extend_from_slice(&event.raw);
                apply_sgr_params(&mut state, &event.raw[event.params_start..event.raw.len() - 1]);
                event_idx += 1;
            }
            if let Some(span) = spans.get(span_idx).filter(|s| s.range.start == ofs) {
                let reverse = reverse_idx == Some(span_idx);
                used_reverse |= reverse;
                out.extend_from_slice(&self.enter_sequence(span, &state, reverse));
                active_end = Some(span.range.end);
                span_idx += 1;
            }
            if ofs < clean.len() {
                match &glyph {
                    Some((g, facet)) if ofs < 2 => {
                        if ofs == 0 {
                            self.emit_glyph(&mut out, *g, facet, &state);
                        } else {
                            out.push(b' ');
                        }
                    }
                    _ => out.push(clean[ofs]),
                }
            }
        }
        out.extend_from_slice(&extract.terminator);
        StyledLine {
            bytes: out,
            used_reverse,
        }
    }

    fn emit_glyph(&self, out: &mut Vec<u8>, glyph: char, facet: &Facet, state: &SgrState) {
        let color = self.theme.for_facet(facet).and_then(|t| t.color);
        let mut params: Vec<String> = vec!["1".into()];
        if let Some(color) = color {
            params.push(fg_params(&self.caps, color));
        }
        out.extend_from_slice(format!("\x1b[{}m", params.join(";")).as_bytes());
        let mut buf = [0u8; 4];
        out.extend_from_slice(glyph.encode_utf8(&mut buf).as_bytes());
        out.extend_from_slice(&state.restore_sequence());
    }

    fn enter_sequence(&self, span: &SpanScore, state: &SgrState, reverse: bool) -> Vec<u8> {
        let params = facet_sgr_params(
            &self.theme,
            &self.caps,
            &span.facet,
            span.weight,
            state.fg.is_none(),
            reverse,
        );
        format!("\x1b[{}m", params.join(";")).into_bytes()
    }
}

pub fn gutter_glyph_slot(clean: &str) -> bool {
    clean.starts_with("  ") && clean.len() > 2 && !clean[2..].starts_with(' ')
}

pub fn facet_sgr_params(
    theme: &Theme,
    caps: &Caps,
    facet: &Facet,
    weight: f32,
    child_fg_default: bool,
    reverse: bool,
) -> Vec<String> {
    let mut params: Vec<String> = Vec::new();
    match facet {
        Facet::Dim => params.push("2".into()),
        Facet::Uncertainty => {
            params.push("2".into());
            params.push(underline_param(caps));
        }
        facet => {
            if weight >= 0.7 {
                params.push("1".into());
            }
            let color = theme.for_facet(facet).and_then(|t| t.color);
            let hue_allowed = child_fg_default || *facet == Facet::Risk;
            if let Some(color) = color.filter(|_| hue_allowed) {
                params.push(fg_params(caps, color));
            }
            if let Facet::User(_) = facet {
                params.push(underline_param(caps));
                if let (Some(color), ColorDepth::TrueColor, true) =
                    (color, caps.depth, caps.undercurl)
                {
                    params.push(format!("58;2;{};{};{}", color.r, color.g, color.b));
                }
            }
            if reverse {
                params.push("7".into());
            }
        }
    }
    params
}

pub fn underline_param(caps: &Caps) -> String {
    if caps.undercurl {
        "4:3".into()
    } else {
        "4".into()
    }
}

pub fn fg_params(caps: &Caps, color: Rgb) -> String {
    match caps.depth {
        ColorDepth::TrueColor => format!("38;2;{};{};{}", color.r, color.g, color.b),
        ColorDepth::C256 => format!("38;5;{}", color.to_256()),
        ColorDepth::C16 => format!("{}", color.to_16()),
    }
}

fn ranges_overlap(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

fn best_reverse_candidate(spans: &[SpanScore]) -> Option<usize> {
    spans
        .iter()
        .enumerate()
        .filter(|(_, s)| s.facet == Facet::Risk && s.weight >= 0.9)
        .max_by(|(_, a), (_, b)| a.weight.total_cmp(&b.weight))
        .map(|(i, _)| i)
}

fn extract(raw: &[u8]) -> Option<Extract> {
    let terminator: &[u8] = if raw.ends_with(b"\r\n") {
        b"\r\n"
    } else if raw.ends_with(b"\n") {
        b"\n"
    } else {
        b""
    };
    let content = &raw[..raw.len() - terminator.len()];
    let mut clean = Vec::with_capacity(content.len());
    let mut events = Vec::new();
    let mut i = 0;
    while i < content.len() {
        let byte = content[i];
        if byte == 0x1b {
            let end = sgr_end(content, i)?;
            events.push(SgrEvent {
                clean_ofs: clean.len(),
                raw: content[i..end].to_vec(),
                params_start: 2,
            });
            i = end;
        } else if byte < 0x20 || byte == 0x7f {
            return None;
        } else {
            clean.push(byte);
            i += 1;
        }
    }
    let clean = String::from_utf8(clean).ok()?;
    Some(Extract {
        clean,
        events,
        terminator: terminator.to_vec(),
    })
}

fn sgr_end(content: &[u8], start: usize) -> Option<usize> {
    if content.get(start + 1) != Some(&b'[') {
        return None;
    }
    let mut i = start + 2;
    while i < content.len() {
        match content[i] {
            b'0'..=b'9' | b';' | b':' => i += 1,
            b'm' => return Some(i + 1),
            _ => return None,
        }
    }
    None
}

pub fn apply_sgr_params(state: &mut SgrState, params: &[u8]) {
    let text = std::str::from_utf8(params).unwrap_or("");
    let tokens: Vec<&str> = text.split(';').collect();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        if token.contains(':') {
            apply_colon_group(state, token);
            i += 1;
            continue;
        }
        let n: u16 = token.parse().unwrap_or(0);
        match n {
            0 => *state = SgrState::default(),
            1 => state.bold = true,
            2 => state.dim = true,
            3 => state.italic = true,
            4 | 21 => state.underline = true,
            5 | 6 => state.blink = true,
            7 => state.inverse = true,
            8 => state.hidden = true,
            9 => state.strikethrough = true,
            22 => {
                state.bold = false;
                state.dim = false;
            }
            23 => state.italic = false,
            24 => state.underline = false,
            25 => state.blink = false,
            27 => state.inverse = false,
            28 => state.hidden = false,
            29 => state.strikethrough = false,
            30..=37 | 90..=97 => state.fg = Some(vec![n]),
            39 => state.fg = None,
            40..=47 | 100..=107 => state.bg = Some(vec![n]),
            49 => state.bg = None,
            38 | 48 | 58 => {
                let (color, consumed) = parse_extended(&tokens[i + 1..], n);
                match (n, color) {
                    (38, Some(c)) => state.fg = Some(c),
                    (48, Some(c)) => state.bg = Some(c),
                    _ => {}
                }
                i += consumed;
            }
            _ => {}
        }
        i += 1;
    }
}

fn parse_extended(rest: &[&str], base: u16) -> (Option<Vec<u16>>, usize) {
    let mode: u16 = rest.first().and_then(|t| t.parse().ok()).unwrap_or(0);
    match mode {
        2 => {
            let rgb: Vec<u16> = rest[1..rest.len().min(4)]
                .iter()
                .map(|t| t.parse().unwrap_or(0))
                .collect();
            if rgb.len() == 3 {
                (
                    Some(vec![base, 2, rgb[0], rgb[1], rgb[2]]),
                    4,
                )
            } else {
                (None, rest.len())
            }
        }
        5 => {
            let idx: Option<u16> = rest.get(1).and_then(|t| t.parse().ok());
            (idx.map(|i| vec![base, 5, i]), 2)
        }
        _ => (None, 0),
    }
}

fn apply_colon_group(state: &mut SgrState, token: &str) {
    let parts: Vec<&str> = token.split(':').collect();
    let head: u16 = parts[0].parse().unwrap_or(0);
    match head {
        4 => {
            let style: u16 = parts.get(1).and_then(|t| t.parse().ok()).unwrap_or(1);
            state.underline = style != 0;
        }
        38 | 48 => {
            let mode: u16 = parts.get(1).and_then(|t| t.parse().ok()).unwrap_or(0);
            let color = match mode {
                2 => {
                    let nums: Vec<u16> = parts[2..]
                        .iter()
                        .filter_map(|t| if t.is_empty() { None } else { t.parse().ok() })
                        .collect();
                    (nums.len() >= 3).then(|| vec![head, 2, nums[nums.len() - 3], nums[nums.len() - 2], nums[nums.len() - 1]])
                }
                5 => parts
                    .get(2)
                    .and_then(|t| t.parse().ok())
                    .map(|i| vec![head, 5, i]),
                _ => None,
            };
            if let Some(color) = color {
                if head == 38 {
                    state.fg = Some(color);
                } else {
                    state.bg = Some(color);
                }
            }
        }
        _ => {}
    }
}
