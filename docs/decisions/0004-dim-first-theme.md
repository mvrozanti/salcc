# 0004 — Dim-first default theme

Status: accepted

## Context

Two ways to make important text pop: decorate the important spans, or dim
the unimportant ones. The evidence is asymmetric. Inappropriate highlighting
actively harms comprehension and metacognitive accuracy, and warnings do not
fix it (Silvers & Kreiner 1997; Gier et al. 2009). Distractor heterogeneity
destroys pre-attentive pop-out (Duncan & Humphreys 1989) — a colorful
background buries every signal, while dimming it restores homogeneity.
Emphasis is zero-sum (von Restorff isolation: Fowler & Barker 1974; Hunt
1995).

## Decision

Default theme is dim-first: boilerplate gets SGR 2 de-emphasis; highlights
are sparse (density budget 0.12); luminance encodes importance (3 steps),
hue encodes category (Okabe-Ito, ≤5); UNCERTAINTY is rendered as
de-emphasis (dim/undercurl), never as a highlight.

## Consequences

The dominant failure mode becomes graceful: wrongly-dimmed text is still
readable, while a wrongly-highlighted span would steal attention. Highlight
facets therefore carry precision floors (CLAUDE.md invariant 7), and the
styler must enforce the density budget by dropping lowest-weight spans.
Changing these defaults requires documented cause in docs/research.md
(invariant 9).
