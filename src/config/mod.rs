//! Configuration: ~/.config/salcc/config.yaml (XDG), reloaded on SIGHUP.
//!
//! Covers scorer selection (default: anthropic/claude-haiku-4-5), hold_ms,
//! density_budget, per-facet toggles, theme overrides (Okabe-Ito defaults),
//! user-defined semantic dimensions (keywords -> aho-corasick rules + llm_hint
//! injected into the LLM prompt), per-command profiles (claude: transcript
//! tail + gutters; "*": grid-text scoring only).
//!
//! Evidence-locked defaults (do not change without reading docs/research.md):
//! dim-first on, density_budget 0.12, no blink ever, hues carry category
//! only, luminance carries importance.

pub mod dimensions;
pub mod theme;

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;
use smol_str::SmolStr;

use dimensions::UserDimension;
use theme::{FacetTheme, Rgb, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FacetToggles {
    pub action: bool,
    pub risk: bool,
    pub command: bool,
    pub uncertainty: bool,
    pub dim: bool,
    pub novelty: bool,
}

impl Default for FacetToggles {
    fn default() -> Self {
        Self {
            action: true,
            risk: true,
            command: true,
            uncertainty: true,
            dim: true,
            novelty: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub hold_ms: u64,
    pub density_budget: f32,
    pub facets: FacetToggles,
    pub theme: Theme,
    pub dimensions: Vec<UserDimension>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hold_ms: 150,
            density_budget: 0.12,
            facets: FacetToggles::default(),
            theme: Theme::default(),
            dimensions: Vec::new(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_yaml(&text).unwrap_or_else(|err| {
                tracing::warn!(%err, ?path, "config parse failed; using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn path() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("salcc/config.yaml"))
    }

    pub fn from_yaml(text: &str) -> anyhow::Result<Self> {
        let raw: RawConfig = serde_norway::from_str(text)?;
        Ok(Self::from_raw(raw))
    }

    fn from_raw(raw: RawConfig) -> Self {
        let mut config = Self::default();
        if let Some(hold_ms) = raw.hold_ms {
            config.hold_ms = hold_ms;
        }
        if let Some(budget) = raw.density_budget {
            config.density_budget = budget.clamp(0.0, 1.0);
        }
        if let Some(facets) = raw.facets {
            let toggles = &mut config.facets;
            let defaults = FacetToggles::default();
            toggles.action = facets.action.unwrap_or(defaults.action);
            toggles.risk = facets.risk.unwrap_or(defaults.risk);
            toggles.command = facets.command.unwrap_or(defaults.command);
            toggles.uncertainty = facets.uncertainty.unwrap_or(defaults.uncertainty);
            toggles.dim = facets.dim.unwrap_or(defaults.dim);
            toggles.novelty = facets.novelty.unwrap_or(defaults.novelty);
        }
        if let Some(theme) = raw.theme {
            let slots: [(&mut FacetTheme, Option<RawFacetTheme>); 5] = [
                (&mut config.theme.action, theme.action),
                (&mut config.theme.risk, theme.risk),
                (&mut config.theme.command, theme.command),
                (&mut config.theme.novelty, theme.novelty),
                (&mut config.theme.uncertainty, theme.uncertainty),
            ];
            for (slot, raw_facet) in slots {
                if let Some(raw_facet) = raw_facet {
                    apply_raw_facet(slot, raw_facet);
                }
            }
        }
        for raw_dim in raw.dimensions.unwrap_or_default() {
            let name = SmolStr::from(raw_dim.name);
            let color = raw_dim.color.as_deref().and_then(Rgb::parse_hex);
            config.theme.user.insert(
                name.clone(),
                FacetTheme {
                    color,
                    glyph: first_char(raw_dim.glyph.as_deref()),
                },
            );
            config.dimensions.push(UserDimension {
                name,
                keywords: raw_dim.keywords,
                color,
                llm_hint: raw_dim.llm_hint,
            });
        }
        config
    }
}

fn apply_raw_facet(slot: &mut FacetTheme, raw: RawFacetTheme) {
    if let Some(color) = raw.color.as_deref() {
        slot.color = Rgb::parse_hex(color);
    }
    if let Some(glyph) = raw.glyph.as_deref() {
        slot.glyph = first_char(Some(glyph));
    }
}

fn first_char(text: Option<&str>) -> Option<char> {
    text.and_then(|t| t.chars().next())
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    hold_ms: Option<u64>,
    density_budget: Option<f32>,
    facets: Option<RawFacetToggles>,
    theme: Option<RawTheme>,
    dimensions: Option<Vec<RawDimension>>,
    #[serde(default)]
    #[allow(dead_code)]
    scorer: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    profiles: Option<HashMap<String, serde_norway::Value>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFacetToggles {
    action: Option<bool>,
    risk: Option<bool>,
    command: Option<bool>,
    uncertainty: Option<bool>,
    dim: Option<bool>,
    novelty: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTheme {
    action: Option<RawFacetTheme>,
    risk: Option<RawFacetTheme>,
    command: Option<RawFacetTheme>,
    novelty: Option<RawFacetTheme>,
    uncertainty: Option<RawFacetTheme>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFacetTheme {
    color: Option<String>,
    glyph: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDimension {
    name: String,
    #[serde(default)]
    keywords: Vec<String>,
    color: Option<String>,
    glyph: Option<String>,
    llm_hint: Option<String>,
}
