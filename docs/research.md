# salcc research report

The evidence base behind salcc's design. Sections 1–5 establish motivation,
literature, prior art, novelty, and rejected alternatives; section 6 states
the derived design rules (locked into CLAUDE.md invariant 9 and the default
theme) with their supporting citations; sections 7–9 cover the scoring
framework, risks, and roadmap. Open-access PDFs for many citations are
vendored in `docs/papers/` (see `docs/papers/fetch.sh`).

## 1. Motivation & thesis

Terminal AI agents produce a lot of prose. A Claude Code session emits
explanations, tool preambles, hedges, commands, warnings, and action items in
one visually uniform stream — markdown-rendered, syntax-highlighted where
code appears, but semantically flat. The reader's real question is rarely
"what does this say?"; it is "what should I pay attention to first?" Today
the only way to answer it is linear reading, and readers do not read
linearly: roughly 79% of users scan rather than read, covering ~20–28% of
the words (Nielsen). Scanning without anchors degrades to the inefficient
F-pattern; good formatting induces the efficient layer-cake pattern
(Pernice 2019, NN/g).

**Thesis:** syntax highlighting colors syntax; salcc colors *meaning*. A
transparent PTY proxy re-styles LLM agent output so semantically important
spans — actions the user must take, destructive risks, runnable commands,
hedged claims, first mentions — are pre-attentively visible, turning agent
output into a scannable surface without changing a single character of it.

Two lines of evidence say this should work, and one says it must be done
carefully:

- Highlighting provided by a competent *other* improves memory and
  comprehension (d = 0.44 vs self-generated; Ponce, Mayer & Méndez 2022),
  and signaling helps most exactly when the reader lacks prior knowledge of
  where to look (Alpizar et al. 2020) — which is precisely the situation of
  a developer triaging unfamiliar agent output.
- Content-blind emphasis does not work: Bionic Reading's mechanical bolding
  shows null or negative results across four independent studies (§3), and
  logprob-colored tokens showed no benefit in a controlled study
  (Vasconcelos et al.). The information content of the emphasis is what
  matters — which is why salcc scores semantics, not statistics.
- Bad highlighting actively harms comprehension and warnings do not fix it
  (Silvers & Kreiner 1997; Gier et al. 2009) — hence salcc's precision
  floors, density budget, and fail-safe span dropping.

## 2. Literature review

### 2.1 Pre-attentive processing & visual channels

Feature-integration theory established that single-feature targets (a
unique color, a unique orientation) are found in under ~250ms with search
time flat in distractor count, while feature *conjunctions* require serial
search (Treisman & Gelade 1980, Cognitive Psychology). Healey & Enns'
survey of attention and visual memory in visualization (2012, IEEE TVCG;
author copy at https://www.csc2.ncsu.edu/faculty/healey/PP/) and Wolfe's
Guided Search 6.0 (2021, Psychonomic Bulletin & Review;
https://link.springer.com/article/10.3758/s13423-020-01859-9) carry this
forward: guidance to targets is driven by a small set of channels, and
stacking channels does not stack speed.

Two asymmetries matter for a terminal theme. Channel interference is
asymmetric — luminance differences interfere with hue judgments more than
the reverse, and both dominate shape (Callaghan 1984; 1989). And distractor
heterogeneity kills pop-out: a target that pops against a homogeneous
background disappears against a varied one (Duncan & Humphreys 1989,
Psychological Review). A terminal full of decorated text has no pop-out
left; *dimming the background* restores homogeneity and gives it back.

### 2.2 Channel rankings & color limits

Cleveland & McGill's graphical-perception experiments (1984, JASA;
replicated at scale by Heer & Bostock 2010, CHI) and Mackinlay's automatic
design work (1986, ACM TOG) rank encodings per data type: hue is effective
for *nominal* (categorical) data, luminance for *ordinal* data. Rainbow
colormaps for ordered data are refuted outright (Borland & Taylor 2007,
IEEE CG&A). Practical limits: ≤6 comfortable / 12 maximum distinguishable
nominal colors (Ware, Information Visualization), and ~7 colors maximum for
rapid search (Healey 1996, Proc. IEEE Visualization '96;
https://vis.cs.brown.edu/docs/pdf/Healey-1996-CEC.pdf). ~8% of males have a
color-vision deficiency, motivating the Okabe-Ito CVD-safe palette plus
redundant non-color coding.

### 2.3 Highlighting & signaling

Self-generated highlighting is a low-utility study technique (Dunlosky et
al. 2013, Psychological Science in the Public Interest). But highlighting
provided by an expert *other* is different: it improves both memory and
comprehension, d = 0.44 over self-generated (Ponce, Mayer & Méndez 2022,
Educational Psychology Review;
https://link.springer.com/article/10.1007/s10648-021-09654-1). The broader
signaling principle — visually cueing the important parts of learning
material — shows d ≈ 0.38–0.53 across meta-analyses, strongest for
low-prior-knowledge readers (Alpizar et al. 2020, ETR&D; Schneider et al.
2018, Educational Research Review).

The negative branch is equally load-bearing: pre-existing *inappropriate*
highlighting actively harms comprehension, and warning readers about it
does not remove the harm (Silvers & Kreiner 1997); it also degrades
metacognitive accuracy — readers become miscalibrated about what they know
(Gier et al. 2009, Journal of General Psychology;
https://files.eric.ed.gov/fulltext/EJ926361.pdf). Emphasis is zero-sum: the
von Restorff isolation effect only exists when isolation is rare (Fowler &
Barker 1974; Hunt 1995). Highlight everything and nothing is highlighted.

### 2.4 Visual search, scanning, reading code

Users scan: 79% of web readers scan rather than read, covering ~20–28% of
words (Nielsen); the F-pattern is the no-anchors fallback while layer-cake
scanning of well-signposted content is the efficient pattern (Pernice 2019,
NN/g). Information-foraging theory adds a constraint on *what* to
highlight: highlights act as information scent, so each must be a
representative proxy for its block, or the reader is misled about where the
value is (Pirolli & Card 1999, Psychological Review). Programmers
specifically read non-linearly, and experts jump between anchor points
rather than reading top-to-bottom (Crosby & Stelovsky 1990; Busjahn et al.
2015, ICPC — eye-tracking evidence;
https://researchonline.gcu.ac.uk/ws/files/24953094/ICPC2015_authors_version.pdf).

Notably, the efficacy of classic *syntax* highlighting is itself mixed — a
small positive study (Sarkar 2015, PPIG;
https://ppig.org/files/2015-PPIG-26th-Sarkar1.pdf) against a large null
(Hannebauer et al. 2018, n≈390). The reading: selectivity, not color per
se, carries the benefit. Coloring everything by kind is decoration;
selectively marking what matters is signal.

### 2.5 Working memory & cognitive load

Working memory holds 4±1 chunks (Cowan 2001, Behavioral and Brain
Sciences), so a reader can hold at most about four learned emphasis
conventions while doing something else — the real task is never "decode the
highlighting." The split-attention effect (Ayres & Sweller; surveyed in the
NSW CESE cognitive load review, 2017) forbids designs that require a
legend: the meaning of every mark must be guessable in place.

### 2.6 Color on dark terminals

Dark terminals want near-black backgrounds with off-white foregrounds —
pure white on pure black causes halation. Dim (SGR 2) text should keep
≥3:1 contrast and be reserved for genuinely skippable content; WCAG AA
requires 4.5:1 for text meant to be read. Saturated pure blue is illegible
on black. These constraints shape the theme's three luminance steps.

### 2.7 Trust, automation bias, and imperfect cueing

A highlighter is an imperfect automated cue, and that literature is
sobering. Imperfect cueing reduces detection of *uncued* targets below the
no-cue baseline — attention follows the cue and abandons the rest
(Parasuraman & Manzey 2010; Wickens et al. 2015; the same effect appears in
AR head-mounted cueing: Warden et al. 2023, arXiv:2303.14300). Galletta et
al.'s spellchecker study (2005, CACM) showed skilled users performing
*worse* with an imperfect checker than without. There is a reliability
crossover near 0.70: below it, automation support is worse than none
(Wickens & Dixon 2007, Theoretical Issues in Ergonomics Science).

The two failure classes decay trust differently. False alarms erode
*compliance* visibly — the cry-wolf effect (Sorkin 1988; Wickens et al.
2009). Misses erode *reliance* silently — users keep trusting a cue that
skips things (Meyer 2004; Dixon, Wickens & McCarley 2007). For a
highlighter, misses are the dangerous class: an unmarked destructive
command inherits false confidence from every correctly-marked one. Hence:
near-1.0 recall for a narrow critical class (RISK), high precision
everywhere else.

Finally, motion and blink: abrupt onset and motion are the strongest
attention captors (Yantis & Jonides 1984) and are toxic when sustained —
animation measurably hurt search performance (Hong, Thong & Tam 2004, ISR),
and WCAG 2.2.2/2.3.1 prohibit uncontrollable blinking. salcc never blinks.

## 3. Prior art survey

| System | Scored by | Facets | Medium | Conversational | Terminal |
|---|---|---|---|---|---|
| AI2 Scim / Semantic Reader | LLM/ML | 4 (objective/novelty/method/result) | browser PDF | no | no |
| Vasconcelos et al. edit-prob highlighting | ML (edit probability) | 1 | IDE plugin | no | no |
| GLTR / logprob colorers | token statistics | 1 | browser | no | no |
| Bionic Reading | none (mechanical) | 1 | e-reader/browser | no | no |
| IDE de-emphasis (dead code gray-out) | static analysis | 1 | IDE | no | no |
| grc / ccze / ChromaTerm / lnav | regex rules | n (syntactic) | terminal | no | yes |
| LLM TUIs (aichat, aider, opencode, Crush…) | none | 0 | terminal | yes | yes |
| tweakcc / claude-powerline / claude-hud | none | 0 | terminal chrome | yes | yes |
| Hemingway Editor | readability rules | ~5 (defect types) | editor | no | no |
| Browser AI article highlighters | LLM | 1 | browser | no | no |
| **salcc** | **LLM (semantic)** | **5 + user-defined** | **terminal** | **yes** | **yes** |

**AI2 Scim / Semantic Reader** is the closest system overall: faceted
highlights (objective, novelty, method, result — one color each) supporting
skimming of scientific papers, deployed across 521k papers (Fok et al.,
IUI 2023: https://dl.acm.org/doi/fullHtml/10.1145/3581641.3584034;
scale-up: https://dl.acm.org/doi/full/10.1145/3665648). Its design lessons
transfer directly: density control is essential, users distrust *missing*
highlights, and a facet legend was needed (salcc instead makes facets
guessable in place — §2.5). But Scim is browser PDFs, batch-processed, not
conversational, not terminal.

**The key negative result** is Vasconcelos et al., "Generation
Probabilities Are Not Enough" (Microsoft Research; TOCHI, presented
CHI'25; https://arxiv.org/abs/2302.07248). Highlighting the
lowest-logprob tokens in AI code completions produced *no benefit over no
highlighting at all*. Highlighting tokens by predicted *human edit
probability* — a reader-relevant semantic — produced faster task
completion, more targeted edits, and user preference. The moral salcc is
built on: score reader-relevant semantics, not model internals.

**Token-statistical colorers** are all statistics, not semantics: GLTR
(http://gltr.io/; Gehrmann, Strobelt & Rush, ACL 2019 demo) colored tokens
by rank and raised human AI-detection from 54% to 72% — a forensic tool,
not a reading aid; the OpenAI logprobs cookbook coloring, ELI5, and
HoloViz logprob coloring are the same idea. Interpretability tooling
(BertViz, Ecco, LIT, LIME/SHAP) is researcher-facing, input-attribution,
browser-bound, with known instability and stopword-highlighting failures
(https://arxiv.org/pdf/2605.28060).

**Bionic Reading is the content-blind control condition, refuted.**
Mechanical bolding of word onsets shows: no significant speed gain in
Readwise's n=2,074 test
(https://blog.readwise.io/does-bionic-reading-actually-work/); "No, Bionic
Reading does not work" (Acta Psychologica 2024;
https://www.sciencedirect.com/science/article/pii/S0001691824001811); no
benefit and *reduced* comprehension on easy text in a 2025 eye-tracking
study (SAGE Open;
https://journals.sagepub.com/doi/10.1177/21582440251376158); and a null
boldface result (Attention, Perception & Psychophysics 2025;
https://link.springer.com/article/10.3758/s13414-025-03067-w). Emphasis
without information content does not help — the emphasis must *know
something*.

**IDE precedent — de-emphasis works.** JetBrains/ReSharper gray out dead
code; Focus Mode plugins dim everything but the current scope
(https://github.com/mallowigi/focus-mode); neodim does the same for
Neovim diagnostics. LSP semantic tokens classify *kind*, never
*importance* — the gap salcc fills. On the semantic-vs-syntactic
distinction: https://tomassetti.me/syntactic-vs-semantic-highlighting/.

**Terminal rule-based colorizers** prove the architecture but not the
semantics: grc and ccze colorize command output by regex; ChromaTerm
(https://pypi.org/project/chromaterm/) is the architectural template — a
transparent colorizing PTY wrapper; lnav (https://lnav.org/features) is
the most semantic of the family with log-level colors and stable
identifier colors. All are pattern-matching, meaning-blind. Streaming
markdown renderers — Streamdown
(https://github.com/day50-dev/Streamdown), md2term, Will McGugan's
streaming-markdown work
(https://willmcgugan.github.io/streaming-markdown/), the rich streaming PR
(simonw/llm#571) — solve incremental-render flicker, an adjacent problem
salcc's hold-buffer design learns from.

**LLM TUIs** (aichat, elia, oterm, gptme, oatmeal, aider, opencode,
Crush) all render uniform markdown with syntax colors and theme support;
none does importance emphasis.

**Claude Code ecosystem**: tweakcc
(https://github.com/Piebald-AI/tweakcc) patches cli.js for themes,
spinners, and input-box syntax highlighting — input only, breaks per
release. claude-powerline, claude-hud, claude-code-log, and cc.nvim build
chrome *around* the response. Nothing styles the response semantically.
Feature requests for a rendering/theming plugin API were closed as
duplicates (anthropics/claude-code#42189, #1302, #22367, #25366, #29166,
#8504); the hook system (SessionStart/End, Setup, UserPromptSubmit, Stop,
StopFailure, Notification, PreToolUse, PostToolUse, FileChanged,
SubagentStart/Stop as of v2.1.211) can block input or inject
`systemMessage` banners but never modify displayed assistant text; output
styles change only the system prompt; the markdown→ANSI renderer is
hardcoded; plugins contribute skills/agents/hooks/MCP/LSP/themes with zero
rendering APIs. Docs: https://code.claude.com/docs/en/hooks.md (plus
plugins-reference.md, output-styles.md, statusline.md,
agent-sdk/streaming-output.md, headless.md).

**Other adjacent work**: Hemingway Editor highlights prose *defects*
(author-facing readability, not reader-facing importance). Browser AI
article highlighters exist (AI Article Highlighter, Highlight AI,
SummarLight) — single facet, web-only. ML highlight recommendation for
Stack Overflow answers reached precision 0.50–0.72 with a poor long tail
(Ahmed et al. 2024; https://arxiv.org/pdf/2401.01472) — a caution about
purely-learned rules, and part of why salcc uses precision-annotated
heuristics plus a sparse LLM prompt instead. Restructuring research
(Graphologue, UIST'23; WaitGPT; Sensecape) transforms LLM output into
diagrams and canvases — a heavier intervention on a different axis.
Adaptive typography (Adobe/Readability Consortium) shows ~21% speed gains
from personalized fonts — personalization pays, orthogonal axis.

## 4. What's genuinely novel

No prior system combines all four of:

1. **LLM-scored** — importance judged semantically, not by regex, token
   statistics, or mechanical position;
2. **multi-facet** — ACTION / RISK / COMMAND / UNCERTAINTY / NOVELTY as
   distinct visual categories, not one undifferentiated "important";
3. **conversational agent output** — streaming, incremental, interleaved
   with tool calls, rather than batch documents;
4. **terminal-native** — inside the user's existing terminal and agent CLI,
   not a browser, IDE pane, or replacement frontend.

Scim has (1)(2); Vasconcelos's edit-probability condition has (1);
ChromaTerm/lnav have (4); LLM TUIs have (3)(4) with zero emphasis. Prior
systems have at most two of the four.

## 5. Critique & rejected alternatives

**Companion pane** (a second terminal pane or window summarizing/annotating
the main one) — rejected. It splits attention between two surfaces, exactly
the split-attention effect the signaling literature warns costs working
memory (Ayres & Sweller); it also abandons the core premise that the answer
to "where do I look?" should be *in the text itself* (in-place signaling is
what carries the d ≈ 0.4 effects in §2.3, and highlights must sit on the
content they are scent for — Pirolli & Card 1999).

**stream-json custom frontend** (`claude -p --output-format stream-json
--include-partial-messages` gives token-level stream events) — rejected as
the primary integration. It means reimplementing the entire Claude Code
UI — input handling, tool approval flows, statusline, slash commands — and
chasing every upstream feature forever. The same argument applies to an
Agent SDK custom TUI. Kept as a documented door for a future dedicated
frontend; the PTY proxy needs none of it (ADR 0001).

**cli.js binary patching** (tweakcc-style) — rejected. tweakcc's own
history shows the cost: patches target minified internals and break on
release cadence. tweakcc also only reaches the input box, not response
rendering. Kept as an explicitly deferred spike (§9), since it is the only
path to true in-app rendering.

**Anti-pattern list** — things salcc will not do, each with its refuting
evidence:

- **Dense highlighting.** Inappropriate/over-highlighting harms
  comprehension and metacognition, warnings don't fix it (Silvers &
  Kreiner 1997; Gier et al. 2009); emphasis is zero-sum (Fowler & Barker
  1974; Hunt 1995). → density budget 0.12, drop lowest-weight spans.
- **Hue ramps for importance.** Rainbow-for-ordered-data is refuted
  (Borland & Taylor 2007); hue is for nominal categories, luminance for
  ordinal intensity (Mackinlay 1986; Cleveland & McGill 1984). → luminance
  = importance, hue = category.
- **Blink/animation.** Strongest capture, toxic sustained (Yantis &
  Jonides 1984; Hong, Thong & Tam 2004); WCAG 2.2.2/2.3.1. → never blink;
  reverse video reserved for the single most critical visible RISK span.
- **Channel stacking / many conventions.** Conjunction search is not
  pre-attentive (Treisman & Gelade 1980; Healey & Enns 2012; Wolfe 2021);
  working memory holds 4±1 chunks (Cowan 2001); >~7 colors defeat rapid
  search (Healey 1996; Ware). → ≤4 conventions on screen, ≤2 simultaneous
  channels per span, ≤5 hues.

## 6. Derived design rules (locked)

These are the evidence-based defaults baked into the theme and enforced as
CLAUDE.md invariants. Changing any of them requires documented cause here.

1. **Dim-first**: de-emphasize boilerplate (SGR 2) rather than decorate
   everything important. Wrongly-dimmed degrades gracefully;
   wrongly-highlighted steals attention. (Duncan & Humphreys 1989 —
   background homogeneity restores pop-out; Silvers & Kreiner 1997; Gier
   et al. 2009; IDE de-emphasis precedent.)
2. **Luminance = ordinal importance** (bright/normal/dim, 3 levels). **Hue
   = category only.** Never hue ramps for importance. (Borland & Taylor
   2007; Mackinlay 1986; Cleveland & McGill 1984; Callaghan 1984/89.)
3. **≤4 emphasis conventions on screen; ≤2 orthogonal channels** —
   conjunction search isn't pre-attentive. (Treisman & Gelade 1980; Healey
   & Enns 2012; Cowan 2001.)
4. **Density budget ~12%**: if more of a block is emphasized, drop
   lowest-weight spans. (Silvers & Kreiner 1997 — bad/dense highlighting
   actively harms, warnings don't fix it; von Restorff isolation: Fowler &
   Barker 1974; Hunt 1995.)
5. **Okabe-Ito CVD-safe hues**, each paired with a redundant channel
   (gutter glyph/weight). (Okabe & Ito palette; ~8% male CVD prevalence;
   Ware's redundant-coding guidance.)
6. **Never blink**; reverse video reserved for the single most critical
   RISK span visible. (WCAG 2.2.2/2.3.1; Hong, Thong & Tam 2004; Yantis &
   Jonides 1984.)
7. **Reliability floor ~0.70**: below it, highlighting is worse than
   nothing (Wickens & Dixon 2007). Heuristic rules ship with precision
   annotations (<0.85 → dim-only/disabled); the Haiku prompt enforces
   sparse marking (precision over recall). Misses are the dangerous
   failure class for RISK → near-1.0 recall for that narrow critical
   class, high precision everywhere else. (Meyer 2004; Dixon, Wickens &
   McCarley 2007; Sorkin 1988; Wickens et al. 2009; Parasuraman & Manzey
   2010.)
8. **Facets**: ACTION ▶ (user must do), RISK ⚠
   (destructive/irreversible/security), COMMAND ❯ (runnable), UNCERTAINTY
   (hedged claims → dim/grey undercurl, i.e. de-emphasis, not highlight),
   NOVELTY ◆ (first mention of entity/decision). No legend required to
   parse — meanings guessable in place. (Ayres & Sweller split-attention;
   Scim's facet-legend lesson inverted.)
9. **LLM-as-highlighter is evidence-aligned**: other-provided highlighting
   outperforms self-generated (Ponce, Mayer & Méndez 2022, d = 0.44), and
   signaling helps most when the reader lacks prior knowledge of where to
   look (Alpizar et al. 2020) — exactly agent-output triage.

## 7. Scoring framework

```rust
trait Scorer { fn score(&self, req: ScoreRequest) -> BoxFuture<Result<Vec<SpanScore>>>; }
struct SpanScore { range: Range<usize>, facet: Facet, weight: f32, rationale: Option<String>, source: SourceId }
enum Facet { Action, Risk, Command, Uncertainty, Novelty, User(SmolStr) }
```

- **Heuristic scorer** (instant, synchronous at emission): fenced code and
  `$`-prompt/known-binary lines → COMMAND; `rm -rf`/`--force`/`DROP`/
  `reset --hard`/`sudo` → RISK; imperatives addressed to the user →
  ACTION; hedge phrases → UNCERTAINTY; first-seen paths/identifiers
  against the session entity set → NOVELTY; tool preambles and "Let me…"
  → DIM. Every rule carries a measured precision annotation; CI fails any
  rule below 0.85 on the precision corpus.
- **Anthropic scorer** (default): raw POST `/v1/messages` with
  `model: claude-haiku-4-5` and a structured-output JSON schema
  (`output_config.format`). Spans come back as
  `{quote, occurrence?, facet, weight}` — **span-as-verbatim-quote
  sidesteps offset hallucination** (ADR 0003); the locator resolves quotes
  to byte ranges, and ambiguous or absent quotes are dropped (fail-safe).
  The system prompt enforces sparsity: mark only what must not be missed,
  ≤1 mark per ~40 words, an empty list is a valid answer, precision over
  recall. Cost ~$0.002/response; per-session USD budget with hard stop;
  blocks under 20 words are skipped; blocks arriving within 500ms
  coalesce. API key via `api_key_env` or `api_key_cmd`.
- **Cascade merge**: per overlapping region, max weight per facet. The LLM
  may veto heuristic spans only where `llm_can_veto` (default true for
  UNCERTAINTY, **never for RISK** — rule 7's asymmetric-recall
  requirement made structural).
- **User-defined dimensions**: config-declared facets
  (`security: {keywords: […], llm_hint: "…", facet: risk, weight: 0.9}`)
  compile to aho-corasick rules and inject into the LLM prompt.
- **Reliability accounting**: every Haiku call logs
  `{ts, session, block_id, model, input_tokens, output_tokens, usd,
  spans_returned, cache_hit}` to `$XDG_STATE_HOME/salcc/metrics.jsonl`,
  feeding the budget and `salcc stats [--serve]`.
- **Ollama backend is deferred** but the trait and cascade are designed so
  it lands purely additively (gpu-lock gated when it does).

## 8. Risks & limitations

- **Automation bias / the missed-unhighlighted-text problem.** The
  deepest limitation: imperfect cueing suppresses attention to uncued
  content below the no-cue baseline (Parasuraman & Manzey 2010; Warden et
  al. 2023), and misses erode reliance silently (Meyer 2004). A RISK
  highlighter that misses one `rm -rf` may be worse than none.
  Mitigations: heuristic RISK rules are recall-tuned and can never be
  vetoed; dim-first keeps unmarked text readable rather than invisible;
  the system-level 0.70 reliability floor is a shipping gate, not an
  aspiration. This risk cannot be engineered to zero — it is documented in
  the README so users calibrate.
- **Scorer cost and latency.** Haiku adds ~$0.002/response and 0.5–1.5s
  RTT. Mitigations: per-session budget with hard stop, block coalescing,
  min-block-length skip, cost dashboard for transparency. The heuristic
  layer means the system degrades to useful-instant rather than
  broken-slow.
- **Scrollback physics.** Text that scrolls out before Haiku returns keeps
  heuristic-only styling forever; ANSI cannot repaint scrollback. This is
  a hard physical limit of the medium, accepted in the architecture
  (docs/architecture.md) rather than fought.
- **Format drift.** Claude Code's Ink output and the transcript JSONL are
  both undocumented and unstable. Mitigations: the grid-physics approach
  doesn't depend on exact escape sequences; fixtures are re-recordable;
  serde is tolerant (only `type=="assistant"` and
  `message.content[].text` are load-bearing); everything degrades to
  bit-perfect passthrough (`salcc doctor --verify`).
- **Wrong styling.** Mitigated structurally: attrs-only invariant,
  fail-safe span dropping, precision floors, child-rewrite-wins overlay
  invalidation.

## 9. Roadmap

- **M0 — transparent passthrough**: PTY spawn, raw mode + guaranteed
  termios restore, resize/signal/exit-code forwarding. Accept: byte-
  identical `salcc cat big.bin` (sha256); vim/htop indistinguishable incl.
  resize storms + 1MiB bracketed paste; p99 chunk latency <1ms.
- **M1 — shadow grid + finalization**: vt100+vte, absolute row ids,
  finalization boundary; `salcc record`/`salcc replay` fixture tooling.
  Accept: replayed real-CC casts → proxy grid == reference grid; zero
  false-finals across fixtures.
- **M2 — heuristic emission styling + config + theme**: segmenter, hold
  buffer, styler, SGR merge, density budget, capability detection. Accept:
  grid text identical styled vs `--no-style`; `NO_COLOR` == M0 bytes; live
  CC session shows dim-first + facets.
- **M3 — transcript tail + Haiku + repaint**: tail poller, tolerant JSONL
  model, Anthropic scorer, quote locator, overlay store, repaint
  scheduler. Accept: mapper ≥95% exact-cell accuracy; Haiku spans repaint
  ≤2.5s after block completion; repaints never hit non-final/scrolled-out
  rows; cost meter matches API usage fields.
- **M4 — cascade + user dimensions + cost web UI**: merge/veto policy,
  YAML dimensions, SIGHUP reload, metrics JSONL, `salcc stats [--serve]`.
  Accept: cascade changes attrs only; new keyword dimension styles without
  restart; dashboard totals reconcile exactly with the metrics log.
- **M5 — polish**: density auto-degrade, `salcc doctor`, README + docs,
  completions.
- **Deferred**: Ollama scorer backend (gpu-lock gated; `Scorer` trait
  already pluggable), codex/gemini agent profiles (the proxy is
  agent-agnostic by design; profile packs land later), cli.js patch spike
  for true in-app rendering.
