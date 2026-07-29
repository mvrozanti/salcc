//! User-defined semantic dimensions from config.yaml. M2 scope: keyword
//! lists compiled into one ascii-case-insensitive aho-corasick automaton
//! with word-boundary post-checks, producing `Facet::User(name)` spans at
//! emission time. `llm_hint` is carried but unused until M3 injects it into
//! the LLM prompt. Empty keyword lists compile to nothing; user spans are
//! emphasis and therefore compete inside the density budget like any other.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use smol_str::SmolStr;

use crate::config::theme::Rgb;
use crate::types::{Facet, SourceId, SpanScore};

const USER_DIMENSION_WEIGHT: f32 = 0.7;

#[derive(Debug, Clone, Default)]
pub struct UserDimension {
    pub name: SmolStr,
    pub keywords: Vec<String>,
    pub color: Option<Rgb>,
    pub llm_hint: Option<String>,
}

#[derive(Debug)]
pub struct DimensionRules {
    ac: AhoCorasick,
    names: Vec<SmolStr>,
}

impl DimensionRules {
    pub fn compile(dimensions: &[UserDimension]) -> Option<Self> {
        let mut patterns = Vec::new();
        let mut names = Vec::new();
        for dim in dimensions {
            for keyword in &dim.keywords {
                if keyword.is_empty() {
                    continue;
                }
                patterns.push(keyword.clone());
                names.push(dim.name.clone());
            }
        }
        if patterns.is_empty() {
            return None;
        }
        let ac = AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::LeftmostLongest)
            .build(&patterns)
            .ok()?;
        Some(Self { ac, names })
    }

    pub fn spans(&self, clean: &str) -> Vec<SpanScore> {
        self.ac
            .find_iter(clean)
            .filter(|m| word_bounded(clean, m.start(), m.end()))
            .map(|m| {
                SpanScore::new(
                    m.start()..m.end(),
                    Facet::User(self.names[m.pattern().as_usize()].clone()),
                    USER_DIMENSION_WEIGHT,
                    SourceId::Heuristic,
                )
            })
            .collect()
    }
}

pub fn word_bounded(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.is_some_and(|c| c.is_alphanumeric()) && !after.is_some_and(|c| c.is_alphanumeric())
}
