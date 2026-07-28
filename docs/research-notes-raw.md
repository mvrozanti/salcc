# Raw research notes (2026-07-28)

Source material for docs/research.md and README references. Gathered by three
research agents (Claude Code extensibility, prior art, cognitive science)
during project inception. Keep until research.md fully supersedes it.

## 1. Claude Code extensibility (why salcc is a PTY proxy)

- No hook fires on assistant message render. Full hook list (v2.1.211+):
  SessionStart/End, Setup, UserPromptSubmit, Stop, StopFailure, Notification,
  PreToolUse, PostToolUse, FileChanged, SubagentStart/Stop. Hooks can block
  input or inject `systemMessage` banners — never modify displayed assistant
  text.
- Output styles = system prompt only. Markdown→ANSI renderer is hardcoded.
  Plugins contribute skills/agents/hooks/MCP/LSP/themes — zero rendering APIs
  (themes = UI color tokens, not prose).
- Feature requests for a rendering/theming plugin API closed as duplicates:
  anthropics/claude-code#42189, #1302, #22367, #25366, #29166, #8504.
- Viable doors: (a) PTY/pipe filter, (b) custom UI over
  `claude -p --output-format stream-json --include-partial-messages`
  (token-level stream events), (c) Agent SDK custom TUI, (d) cli.js binary
  patching à la tweakcc (fragile, breaks per release).
- Transcripts: `~/.claude/projects/<cwd with / → ->/<session>.jsonl`, one JSON
  object per line, assistant turns one line per content block
  (thinking/text/tool_use) sharing message.id; `isSidechain: true` =
  subagent. Format undocumented/unstable → tolerant parsing.
- Statusline: separate bottom row, script-driven, auxiliary channel only.
- Docs: https://code.claude.com/docs/en/hooks.md, plugins-reference.md,
  output-styles.md, statusline.md, agent-sdk/streaming-output.md, headless.md.

## 2. Prior art

**Closest overall: AI2 Scim / Semantic Reader** — faceted highlights
(objective/novelty/method/result, one color each) for skimming scientific
papers; density user-configurable; deployed across 521k papers.
IUI 2023: https://dl.acm.org/doi/fullHtml/10.1145/3581641.3584034 ; scale-up:
https://dl.acm.org/doi/full/10.1145/3665648 ; design lessons: density control
essential, users distrust missing highlights, facet legend needed. Browser
PDFs, batch, not conversational, not terminal.

**The key negative result: Vasconcelos et al., "Generation Probabilities Are
Not Enough"** (Microsoft Research; TOCHI/CHI'25) —
https://arxiv.org/abs/2302.07248 — highlighting lowest-logprob tokens in AI
code completions: no benefit over none. Highlighting tokens by predicted
*human edit probability*: faster completion, targeted edits, preferred.
Moral: score reader-relevant semantics, not model internals.

**Token-statistical colorers** (all statistical, not semantic): GLTR
http://gltr.io/ (rank-colored tokens, raised human AI-detection 54%→72%);
OpenAI logprobs cookbook coloring; ELI5 logprob explain; HoloViz logprob
coloring. Interpretability: BertViz, Ecco, LIT, LIME/SHAP (researcher tools,
input attribution, browser-bound; known instability + stopword highlighting
failures: https://arxiv.org/pdf/2605.28060).

**Bionic Reading = the content-blind control condition, refuted**: Readwise
n=2,074 no significant speed gain
(https://blog.readwise.io/does-bionic-reading-actually-work/); Acta
Psychologica 2024 "No, Bionic Reading does not work"
(https://www.sciencedirect.com/science/article/pii/S0001691824001811); SAGE
2025 eye-tracking: no benefit, comprehension dropped on easy text
(https://journals.sagepub.com/doi/10.1177/21582440251376158); Springer 2025
boldface study (https://link.springer.com/article/10.3758/s13414-025-03067-w).
Mechanical emphasis without information content does not help.

**IDE precedent — de-emphasis works**: JetBrains/ReSharper dead-code gray-out,
Focus Mode plugin (https://github.com/mallowigi/focus-mode), neodim. LSP
semantic tokens classify kind, never importance. Semantic-vs-syntactic essay:
https://tomassetti.me/syntactic-vs-semantic-highlighting/.

**Terminal rule-based colorizers** (regex, not meaning): grc, ccze, ChromaTerm
(https://pypi.org/project/chromaterm/ — architectural template: transparent
colorizing PTY wrapper), lnav (https://lnav.org/features — most semantic:
log-level colors, stable identifier colors). Streaming markdown renderers:
Streamdown (https://github.com/day50-dev/Streamdown), md2term,
https://willmcgugan.github.io/streaming-markdown/, rich streaming PR
simonw/llm#571 — solve incremental-render flicker.

**LLM TUIs** (aichat, elia, oterm, gptme, oatmeal, aider, opencode, Crush):
uniform markdown + syntax colors; themes only; none do importance emphasis.

**Claude Code ecosystem**: tweakcc (https://github.com/Piebald-AI/tweakcc)
patches cli.js for themes/spinners/input-highlighting — input box only.
claude-powerline, claude-hud, claude-code-log, cc.nvim: chrome around the
response, never inside it. Nothing does semantic response styling.

**Prose-issue highlighting**: Hemingway Editor (readability defects, author-
facing). **Browser AI importance highlighters exist for articles**: AI Article
Highlighter, Highlight AI, SummarLight (single facet, web-only).
**Highlight-recommendation ML**: SO-answer highlighting CNN/BERT,
precision 0.50–0.72, poor long tail (https://arxiv.org/pdf/2401.01472).
**Adjacent restructuring research**: Graphologue (UIST'23, inline annotation
during generation → diagrams), WaitGPT, Sensecape.
**Adaptive typography**: Adobe/Readability Consortium — personalized fonts
~21% speed gains; personalization pays, orthogonal axis.

**Novelty claim**: nothing combines (1) LLM-scored (2) multi-facet semantic
importance (3) conversational agent output (4) terminal-native. Prior systems
have at most two.

## 3. Cognitive science → design rules

Findings (citations):
- Pre-attentive pop-out: single-feature targets found <250ms, flat in
  distractor count; conjunctions require serial search. Treisman & Gelade
  1980 (Cognitive Psychology); Healey & Enns 2012 (IEEE TVCG,
  https://www.csc2.ncsu.edu/faculty/healey/PP/); Wolfe Guided Search 6.0 2021
  (https://link.springer.com/article/10.3758/s13423-020-01859-9).
- Channel interference asymmetric: luminance > hue > shape (Callaghan
  1984/89). Distractor heterogeneity kills pop-out (Duncan & Humphreys 1989,
  Psychological Review) → dimming the background restores it.
- Channel rankings: Cleveland & McGill 1984 (JASA; replicated Heer & Bostock
  2010 CHI); Mackinlay 1986 (ACM TOG): hue high for nominal, luminance for
  ordinal. Rainbow for ordered data refuted: Borland & Taylor 2007 (IEEE
  CG&A).
- Color codes: ≤6 comfortable / 12 max nominal (Ware); ~7 max rapid-search
  colors (Healey 1996). CVD ~8% males → Okabe-Ito palette, redundant coding.
- Highlighting: low-utility for learning (Dunlosky et al. 2013 PSPI), BUT
  expert/other-provided highlighting improved memory AND comprehension
  d = 0.44 vs self-generated (Ponce, Mayer & Méndez 2022, Educ Psych Review,
  https://link.springer.com/article/10.1007/s10648-021-09654-1). Signaling
  principle: d ≈ 0.38–0.53, strongest for low-prior-knowledge readers
  (Alpizar et al. 2020 ETR&D; Schneider et al. 2018 Educ Research Review).
- Bad highlighting actively harms and warnings don't fix it (Silvers &
  Kreiner 1997; Gier et al. 2009 — also degrades metacognitive accuracy).
  Emphasis is zero-sum: von Restorff isolation (Fowler & Barker 1974;
  Hunt 1995).
- Scanning: 79% scan, ~20-28% of words read (Nielsen); F-pattern is the
  no-anchors fallback; layer-cake (headings) is the efficient pattern good
  formatting induces (Pernice 2019 NN/g). Information scent: Pirolli & Card
  1999 (Psychological Review) — highlights must be representative proxies of
  their block. Programmers read non-linearly, experts jump anchors (Crosby &
  Stelovsky 1990; Busjahn et al. 2015 ICPC).
- Working memory 4±1 chunks (Cowan 2001 BBS) → ≤4 learned emphasis
  conventions. Split-attention: semantics must be guessable in place, no
  legends (Ayres & Sweller).
- Automation bias: imperfect cueing reduces detection of uncued targets below
  no-cue baseline (Parasuraman & Manzey 2010; Wickens et al. 2015; Galletta
  et al. 2005 CACM spellchecker study). Reliability crossover ~0.70: below
  it, automation worse than none (Wickens & Dixon 2007). False alarms erode
  compliance (cry-wolf: Sorkin 1988; Wickens et al. 2009); misses erode
  reliance silently (Meyer 2004; Dixon, Wickens & McCarley 2007) → for a
  highlighter, misses are the dangerous class; keep near-1.0 recall only for
  a narrow critical class (RISK), high precision everywhere.
- Motion/blink: strongest capture, toxic sustained (Yantis & Jonides 1984;
  Hong, Thong & Tam 2004 ISR: animation hurt search; WCAG 2.2.2/2.3.1).
- Dark terminals: near-black bg + off-white fg, avoid pure-white-on-black
  halation; dim text ≥3:1 only if genuinely skippable; saturated pure blue
  illegible on black. WCAG AA 4.5:1 for readable text.
- Syntax-highlighting efficacy is itself mixed (Sarkar 2015 PPIG n=10
  positive; Hannebauer et al. 2018 n≈390 null) — selectivity, not color,
  carries benefit.

Derived rules (locked into salcc defaults): dim-first; luminance=importance
(3 steps), hue=category (Okabe-Ito, ≤5), glyph redundancy; ≤4 conventions,
≤2 simultaneous channels; density budget ~12%; never blink, reverse video =
single most critical RISK; no legends required to parse; precision floors
0.85/rule, 0.70 system; UNCERTAINTY = de-emphasis not highlight.

## 4. Open-access PDFs for docs/papers/

Downloadable (license-safe): arXiv 2302.07248 (Vasconcelos), arXiv
1906.04043 (GLTR), arXiv 2401.01472 (SO highlighting), arXiv 2303.14300
(imperfect AR cueing), Busjahn ICPC 2015 (author copy,
https://researchonline.gcu.ac.uk/ws/files/24953094/ICPC2015_authors_version.pdf),
Sarkar PPIG 2015 (https://ppig.org/files/2015-PPIG-26th-Sarkar1.pdf), Gier
EJ926361 (ERIC: https://files.eric.ed.gov/fulltext/EJ926361.pdf), Healey
1996 CEC (https://vis.cs.brown.edu/docs/pdf/Healey-1996-CEC.pdf), Cowan 2001
(philpapers), NSW CESE CLT review. Paywalled (cite only): Dunlosky 2013,
Ponce 2022, Alpizar 2020, Schneider 2018, Wickens & Dixon 2007, Treisman &
Gelade 1980, Duncan & Humphreys 1989, Scim IUI 2023 (ACM).
