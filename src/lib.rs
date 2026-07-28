//! salcc — Semantic Attention Layer for Claude Code.
//!
//! Transparent PTY proxy that re-styles LLM agent terminal output so
//! semantically important spans (actions, risks, commands, uncertainty,
//! novelty) are visually emphasized. Read CLAUDE.md and docs/architecture.md
//! before modifying term/ or emit/.

pub mod cli;
pub mod config;
pub mod emit;
pub mod map;
pub mod proxy;
pub mod score;
pub mod stats;
pub mod tail;
pub mod term;
pub mod types;
