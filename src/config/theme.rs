//! Facet theme: hue carries category (Okabe-Ito, CVD-safe), luminance
//! carries importance, glyphs mark the gutter. Evidence-locked defaults
//! (docs/research.md, ADR 0004): Action #0072B2 ▶, Risk #D55E00 ⚠,
//! Command #009E73 ❯, Novelty #CC79A7 ◆, Uncertainty hueless (dim +
//! undercurl is de-emphasis, never a highlight). User dimensions get their
//! configured hue or underline-only. Glyphs must be width-1 or the gutter
//! swap (the sole attrs-only exception) is refused at load time.

use std::collections::HashMap;

use smol_str::SmolStr;
use unicode_width::UnicodeWidthChar;

use crate::types::Facet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn parse_hex(text: &str) -> Option<Self> {
        let hex = text.trim().trim_start_matches('#');
        if hex.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Self { r, g, b })
    }

    pub fn to_256(self) -> u8 {
        let scale = |v: u8| u8::try_from((u16::from(v) * 5 + 127) / 255).unwrap_or(5);
        16 + 36 * scale(self.r) + 6 * scale(self.g) + scale(self.b)
    }

    pub fn to_16(self) -> u8 {
        let bright = u16::from(self.r) + u16::from(self.g) + u16::from(self.b) > 510;
        let mut code = 30 + u8::from(self.r > 127) + 2 * u8::from(self.g > 127) + 4 * u8::from(self.b > 127);
        if code == 30 {
            code = 30 + u8::from(self.r > 63) + 2 * u8::from(self.g > 63) + 4 * u8::from(self.b > 63);
        }
        if bright { code + 60 } else { code }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FacetTheme {
    pub color: Option<Rgb>,
    pub glyph: Option<char>,
}

impl FacetTheme {
    fn new(color: Rgb, glyph: char) -> Self {
        Self {
            color: Some(color),
            glyph: Some(glyph),
        }
    }

    pub fn gutter_glyph(&self) -> Option<char> {
        self.glyph.filter(|g| g.width() == Some(1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub action: FacetTheme,
    pub risk: FacetTheme,
    pub command: FacetTheme,
    pub novelty: FacetTheme,
    pub uncertainty: FacetTheme,
    pub user: HashMap<SmolStr, FacetTheme>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            action: FacetTheme::new(Rgb::new(0x00, 0x72, 0xB2), '▶'),
            risk: FacetTheme::new(Rgb::new(0xD5, 0x5E, 0x00), '⚠'),
            command: FacetTheme::new(Rgb::new(0x00, 0x9E, 0x73), '❯'),
            novelty: FacetTheme::new(Rgb::new(0xCC, 0x79, 0xA7), '◆'),
            uncertainty: FacetTheme::default(),
            user: HashMap::new(),
        }
    }
}

impl Theme {
    pub fn for_facet(&self, facet: &Facet) -> Option<&FacetTheme> {
        match facet {
            Facet::Action => Some(&self.action),
            Facet::Risk => Some(&self.risk),
            Facet::Command => Some(&self.command),
            Facet::Novelty => Some(&self.novelty),
            Facet::Uncertainty => Some(&self.uncertainty),
            Facet::Dim => None,
            Facet::User(name) => self.user.get(name),
        }
    }
}
