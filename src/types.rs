//! Core shared types for salcc. Every module speaks these; changing them is an
//! interface change and requires updating docs/architecture.md.
//!
//! Invariants encoded here:
//! - `SpanScore.range` is a byte range into the CLEAN text it was scored
//!   against (transcript markdown or reconstructed grid text) — never into
//!   rendered/wrapped terminal output. The map/ module owns the translation.
//! - `weight` is always clamped to 0.0..=1.0 at construction.
//! - `Facet::Dim` is de-emphasis: it never counts against the density budget.

use std::collections::HashSet;
use std::future::Future;
use std::ops::Range;
use std::pin::Pin;

use smol_str::SmolStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Facet {
    Action,
    Risk,
    Command,
    Uncertainty,
    Novelty,
    Dim,
    User(SmolStr),
}

impl Facet {
    pub fn is_emphasis(&self) -> bool {
        !matches!(self, Facet::Dim | Facet::Uncertainty)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceId {
    Heuristic,
    Anthropic,
    Ollama,
}

#[derive(Debug, Clone)]
pub struct SpanScore {
    pub range: Range<usize>,
    pub facet: Facet,
    pub weight: f32,
    pub rationale: Option<String>,
    pub source: SourceId,
}

impl SpanScore {
    pub fn new(range: Range<usize>, facet: Facet, weight: f32, source: SourceId) -> Self {
        Self {
            range,
            facet,
            weight: weight.clamp(0.0, 1.0),
            rationale: None,
            source,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatencyClass {
    Instant,
    Async,
}

#[derive(Debug, Default, Clone)]
pub struct ConvoContext {
    pub session_id: String,
    pub recent_block_summaries: Vec<String>,
    pub seen_entities: HashSet<String>,
}

#[derive(Debug)]
pub struct ScoreRequest<'a> {
    pub text: &'a str,
    pub ctx: &'a ConvoContext,
}

pub type ScoreFuture<'a> =
    Pin<Box<dyn Future<Output = anyhow::Result<Vec<SpanScore>>> + Send + 'a>>;

pub trait Scorer: Send + Sync {
    fn name(&self) -> &'static str;
    fn latency_class(&self) -> LatencyClass;
    fn score<'a>(&'a self, req: ScoreRequest<'a>) -> ScoreFuture<'a>;
}

/// Absolute line id: lines ever printed by the child, counted from session
/// start. Survives scrolling; `viewport_row = abs_row - scroll_base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AbsRow(pub u64);

/// Verdict of the finalization detector for a printed line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finality {
    /// Scrolled or above the child's dynamic repaint region: style once, safe.
    ScrollbackBound,
    /// Inside the child's actively repainted region: pass through untouched.
    ActiveRegion,
    /// Not yet decidable: hold until deadline, then emit raw if still unknown.
    Ambiguous,
}
