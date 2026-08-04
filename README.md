# salcc — semantic attention layer for terminal AI agents

Syntax highlighting colors syntax. salcc colors **meaning**.

AI coding agents produce a lot of prose, and it all looks the same: the
crucial "this will drop your production table" renders in exactly the style
of "let me look at that file." The question you actually have while an agent
streams output at you is *"what should I pay attention to first?"* — and
today the only way to answer it is to read everything, linearly, even though
decades of reading research say nobody does that. salcc is a transparent PTY
proxy (`salcc claude`) that re-styles agent output as it streams so the
answer is visible at a glance: actions you must take, destructive risks,
runnable commands, hedged claims, and first mentions each get a distinct,
evidence-chosen visual treatment — and the boilerplate around them is dimmed
so the signal actually pops.

It changes *presentation only*: not one character of the agent's output is
altered, your keystrokes are never buffered or inspected, and with
`NO_COLOR` (or `--no-style`, or inside vim/htop) it is a bit-perfect pipe.
It wraps any agent CLI, needs no plugin API, and survives every upstream
release because it never touches the agent's code.

## What it looks like

```text
⏺ I checked the migration that failed last night. The root cause is a
  stale advisory lock left behind by the crashed runner — the schema      · boilerplate: dimmed
  itself is fine.

  ◆ The lock lives in pg_advisory_locks, key 7421.                        ◆ NOVELTY: first mention

▶ You need to rotate the DATABASE_URL credential before re-running        ▶ ACTION: user must do
  the deploy — the old one leaked into the failed job's logs.

❯ psql -c "SELECT pg_advisory_unlock_all();"                              ❯ COMMAND: runnable

⚠ `DROP TABLE sessions_backup` is irreversible — verify the backup        ⚠ RISK: never LLM-vetoed;
  restored cleanly before running it.                                       critical span in reverse

  This should also fix the retry loop, though I haven't verified          ~ UNCERTAINTY: hedges are
  the timeout path.                                                         dimmed + undercurled
```

Luminance carries importance (bright/normal/dim), hue carries category
(Okabe-Ito, color-blind-safe), and every hue is paired with a redundant
gutter glyph so no meaning depends on color alone.

## Status

**Working alpha.** Milestones M0–M6 are complete: transparent PTY
passthrough, shadow-grid finalization, heuristic emission styling, async
Haiku scoring with visible-region repaint, cascade merge with user-defined
dimensions, the cost metrics dashboard (`salcc stats --serve`), and the
self-annotation scorer (now the default: the wrapped agent marks its own
spans in a trailing annotation line that salcc parses and conceals — zero
API calls, zero added latency; ADR 0005) are all live and covered by the
test suite (conformance fixtures, attrs-only styling checks, PTY
integration). Deferred by decision: an Ollama/local scorer backend and
codex/gemini transcript profiles. Interfaces and the config schema may
still change before 0.2.

## Quickstart

```sh
# nix
nix run github:mvrozanti/salcc -- claude

# cargo
git clone https://github.com/mvrozanti/salcc && cd salcc
cargo install --path .
salcc claude
```

`salcc <any-command>` wraps any CLI; the `claude` profile additionally tails
the Claude Code transcript for clean text to score. Escape hatches:
`NO_COLOR=1` or `salcc --no-style <cmd>` for bit-perfect passthrough.

## Seamless launch

Typing `salcc claude` gets old. Run this once:

```sh
salcc link
```

It installs a PATH shim — a symlink named `claude` (in `$XDG_BIN_HOME` or
`~/.local/bin`) that *is* salcc. From then on you type `claude` exactly as
before and transparently get the styling layer, in **every shell and every
script**, with no rc file to edit. `salcc unlink` removes it. `salcc link
codex gemini` (or an `agents:` list in the config) shims more agents.

It's recursion-safe by construction: salcc resolves the real agent by
skipping its own symlink, spawns it by absolute path (bypassing the shim),
and exports `SALCC_ACTIVE` so an agent that spawns a subshell can never nest
proxies. It refuses to clobber a non-salcc file, and `salcc doctor` shows
each agent's shim status. If your bin dir isn't ahead of the real binary on
`PATH`, `link` says so.

<details><summary>Prefer a shell function instead of a PATH shim?</summary>

`eval "$(salcc init zsh)"` (or `bash`, or `salcc init fish | source`) defines
an equivalent wrapper function in your rc. Same guarantees; use it if you
don't want a symlink in your bin dir.
</details>

Either way, nothing about how you invoke the agent changes — `NO_COLOR`,
`--no-style`, pipes, and non-interactive use all still pass through
untouched.

```sh
salcc doctor                 # terminal caps, config parse status, key
                             # resolution, transcript discovery, metrics
salcc doctor --verify        # + PTY round-trip byte-identity self-check
salcc completions zsh        # shell completions (zsh/bash/fish) to stdout
salcc link                   # install PATH shims — type `claude` as usual (see below)
salcc unlink                 # remove them
```

`salcc doctor` is where config problems surface: YAML parse errors print
verbatim there (the proxy itself silently falls back to defaults, because
it owns your terminal).

## Configuration

`~/.config/salcc/config.yaml` (SIGHUP reloads):

```yaml
scorer:
  name: self                 # self (default) | anthropic | heuristic
  inject_instruction: true   # self: pass the annotation instruction to the
                             # child via --append-system-prompt when supported
  model: claude-haiku-4-5    # anthropic only
  api_key_env: ANTHROPIC_API_KEY
  max_session_usd: 0.50      # hard stop when exceeded
  min_block_words: 20        # shorter blocks are never sent to the LLM
agents: [claude]             # which CLIs `salcc init` wraps transparently
hold_ms: 150
density_budget: 0.12
facets:                      # per-facet toggles, all default true
  novelty: true
theme:
  action:      { color: "#0072B2", glyph: "▶" }
  risk:        { color: "#D55E00", glyph: "⚠" }
  command:     { color: "#009E73", glyph: "❯" }
  novelty:     { color: "#CC79A7", glyph: "◆" }
dimensions:                  # your own facets
  - name: security
    keywords: [password, token, credential]
    color: "#E69F00"
    glyph: "◈"
    llm_hint: "credential handling and secret exposure"
llm_can_veto:                # which heuristic facets a zero-weight LLM
  uncertainty: true          # span may suppress; risk is never vetoable
```

## Cost transparency

The default scorer (`self`) costs nothing: the wrapped agent annotates its
own output in a trailing machine-consumed line that salcc parses and
conceals on screen — no API calls, no added latency. The opt-in
`anthropic` scorer sends completed response blocks to `claude-haiku-4-5`
(~$0.002 per scored response at current pricing; short blocks are skipped
and nearby blocks coalesce). Because a tool that quietly spends your API
key is obnoxious, every call is metered:

- a per-session USD budget with a hard stop (default in config above);
- `salcc stats` prints totals: USD per session/day, tokens in/out, cost per
  response, spans per dollar, cache-hit rate;
- `salcc stats --serve` serves the same data as a local-only dashboard
  (127.0.0.1:7877), reconciled against the API's own usage fields.

The instant heuristic scorer costs nothing and runs even when the LLM
scorer is disabled or over budget.

## The science

Every default is downstream of a finding — details and full context in
[`docs/research.md`](docs/research.md):

1. **Dim-first**: wrongly-dimmed text degrades gracefully;
   wrongly-highlighted text steals attention, and bad highlighting
   measurably harms comprehension (Silvers & Kreiner 1997; Gier et al.
   2009; Duncan & Humphreys 1989).
2. **Luminance = importance, hue = category** — never hue ramps for
   ordered data (Borland & Taylor 2007; Mackinlay 1986).
3. **≤4 emphasis conventions, ≤2 simultaneous channels** — conjunction
   search isn't pre-attentive and working memory holds 4±1 chunks
   (Treisman & Gelade 1980; Healey & Enns 2012; Cowan 2001).
4. **~12% density budget** — emphasis is zero-sum; over-highlighting
   destroys the effect (Fowler & Barker 1974; Hunt 1995).
5. **Okabe-Ito palette + redundant glyphs** — ~8% of men have a
   color-vision deficiency; no meaning rides on color alone (Okabe & Ito;
   Ware).
6. **Never blink** — animation captures attention and hurts search when
   sustained (Yantis & Jonides 1984; Hong, Thong & Tam 2004; WCAG
   2.2.2/2.3.1).
7. **Reliability floor 0.70, misses are the dangerous class** — below ~0.70
   an imperfect cue is worse than none, and missed alerts silently destroy
   reliance, so RISK runs recall-first and can never be vetoed (Wickens &
   Dixon 2007; Meyer 2004; Dixon, Wickens & McCarley 2007).
8. **An LLM highlighter is the evidence-aligned kind** — other-provided
   highlighting beats self-generated (d = 0.44), and signaling helps most
   when you don't already know where to look (Ponce, Mayer & Méndez 2022;
   Alpizar et al. 2020) — while content-blind emphasis (Bionic Reading,
   logprob coloring) shows null results (Vasconcelos et al. 2025; Acta
   Psychologica 2024).

Honest caveat: the strongest known limitation of any imperfect highlighter
is automation bias — cued readers attend less to *uncued* content
(Parasuraman & Manzey 2010; Warden et al. 2023). salcc mitigates this
structurally (dim-first, precision floors, RISK recall) but cannot
eliminate it. Highlighting is a triage aid, not a substitute for reading
the parts that matter.

## Contributing

The main developer of this codebase is an LLM; the repo is structured for
that. Start with [`CLAUDE.md`](CLAUDE.md) (module map, hard invariants,
commands), then [`docs/architecture.md`](docs/architecture.md) before
touching `term/` or `emit/`, and [`docs/decisions/`](docs/decisions/) for
settled choices. Evidence disputes go through
[`docs/research.md`](docs/research.md) — theme defaults only change with
documented cause. Open-access papers are vendored under
[`docs/papers/`](docs/papers/) (`fetch.sh` downloads them).

## References

Open-access PDFs marked ◦ are vendored in `docs/papers/`.

### Pre-attentive processing, visual search & color

- Treisman, A. M., & Gelade, G. (1980). A feature-integration theory of
  attention. *Cognitive Psychology, 12*(1), 97–136.
  https://doi.org/10.1016/0010-0285(80)90005-5
- Healey, C. G., & Enns, J. T. (2012). Attention and visual memory in
  visualization and computer graphics. *IEEE TVCG, 18*(7). Author copy:
  https://www.csc2.ncsu.edu/faculty/healey/PP/
- Wolfe, J. M. (2021). Guided Search 6.0: An updated model of visual
  search. *Psychonomic Bulletin & Review, 28*.
  https://link.springer.com/article/10.3758/s13423-020-01859-9
- Callaghan, T. C. (1984; 1989). Dimensional interaction of hue and
  brightness in preattentive field segregation; Interference and dominance
  in texture segregation. *Perception & Psychophysics*.
- Duncan, J., & Humphreys, G. W. (1989). Visual search and stimulus
  similarity. *Psychological Review, 96*(3), 433–458.
  https://doi.org/10.1037/0033-295X.96.3.433
- Yantis, S., & Jonides, J. (1984). Abrupt visual onsets and selective
  attention. *JEP: Human Perception and Performance, 10*(5).
- Cleveland, W. S., & McGill, R. (1984). Graphical perception. *JASA,
  79*(387), 531–554. https://doi.org/10.1080/01621459.1984.10478080
- Heer, J., & Bostock, M. (2010). Crowdsourcing graphical perception.
  *CHI 2010*. https://doi.org/10.1145/1753326.1753357
- Mackinlay, J. (1986). Automating the design of graphical presentations
  of relational information. *ACM TOG, 5*(2).
  https://doi.org/10.1145/22949.22950
- Borland, D., & Taylor, R. M. (2007). Rainbow color map (still)
  considered harmful. *IEEE CG&A, 27*(2).
- Ware, C. *Information Visualization: Perception for Design.* Morgan
  Kaufmann.
- ◦ Healey, C. G. (1996). Choosing effective colours for data
  visualization. *Proc. IEEE Visualization '96*.
  https://vis.cs.brown.edu/docs/pdf/Healey-1996-CEC.pdf
- Okabe, M., & Ito, K. Color Universal Design — the Okabe-Ito palette.
  https://jfly.uni-koeln.de/color/
- W3C. WCAG 2.1, success criteria 2.2.2 (Pause, Stop, Hide) and 2.3.1
  (Three Flashes). https://www.w3.org/TR/WCAG21/
- Hong, W., Thong, J. Y. L., & Tam, K. Y. (2004). Does animation attract
  online users' attention? *Information Systems Research, 15*(1).

### Highlighting, signaling & reading

- Dunlosky, J., Rawson, K. A., Marsh, E. J., Nathan, M. J., & Willingham,
  D. T. (2013). Improving students' learning with effective learning
  techniques. *Psychological Science in the Public Interest, 14*(1).
  https://doi.org/10.1177/1529100612453266
- Ponce, H. R., Mayer, R. E., & Méndez, E. E. (2022). Effects of
  learner-generated and instructor-provided highlighting on learning from
  text: a meta-analysis. *Educational Psychology Review, 34*, 989–1024.
  https://link.springer.com/article/10.1007/s10648-021-09654-1
- Alpizar, D., Adesope, O. O., & Wong, R. M. (2020). A meta-analysis of
  signaling principle in multimedia learning environments. *Educational
  Technology Research and Development, 68*.
  https://doi.org/10.1007/s11423-020-09748-7
- Schneider, S., Beege, M., Nebel, S., & Rey, G. D. (2018). A
  meta-analysis of how signaling affects learning with media.
  *Educational Research Review, 23*, 1–24.
  https://doi.org/10.1016/j.edurev.2017.11.001
- Silvers, V. L., & Kreiner, D. S. (1997). The effects of pre-existing
  inappropriate highlighting on reading comprehension.
- ◦ Gier, V. S., Kreiner, D. S., & Natz-Gonzalez, A. (2009). Harmful
  effects of preexisting inappropriate highlighting on reading
  comprehension and metacognitive accuracy. *Journal of General
  Psychology, 136*(3). https://files.eric.ed.gov/fulltext/EJ926361.pdf
- Fowler, R. L., & Barker, A. S. (1974). Effectiveness of highlighting for
  retention of text material. *Journal of Applied Psychology, 59*(3).
- Hunt, R. R. (1995). The subtlety of distinctiveness: What von Restorff
  really did. *Psychonomic Bulletin & Review, 2*.
- Nielsen, J. (1997). How users read on the web. NN/g.
  https://www.nngroup.com/articles/how-users-read-on-the-web/
- Pernice, K. (2019). Text scanning patterns: eyetracking evidence. NN/g.
  https://www.nngroup.com/articles/text-scanning-patterns-eyetracking/
- Pirolli, P., & Card, S. (1999). Information foraging. *Psychological
  Review, 106*(4), 643–675.
- Crosby, M. E., & Stelovsky, J. (1990). How do we read algorithms? A
  case study. *IEEE Computer, 23*(1).
- ◦ Busjahn, T., Bednarik, R., Begel, A., Crosby, M., Paterson, J. H.,
  Schulte, C., Sharif, B., & Tamm, S. (2015). Eye movements in code
  reading: relaxing the linear order. *ICPC 2015*.
  https://researchonline.gcu.ac.uk/ws/files/24953094/ICPC2015_authors_version.pdf
- ◦ Sarkar, A. (2015). The impact of syntax colouring on program
  comprehension. *PPIG 2015*.
  https://ppig.org/files/2015-PPIG-26th-Sarkar1.pdf
- Hannebauer, C., Hesenius, M., & Gruhn, V. (2018). Does syntax
  highlighting help programming novices? *Empirical Software
  Engineering, 23*. (n≈390, null result.)
- Cowan, N. (2001). The magical number 4 in short-term memory.
  *Behavioral and Brain Sciences, 24*(1), 87–114.
  https://doi.org/10.1017/S0140525X01003922
- Ayres, P., & Sweller, J. The split-attention principle in multimedia
  learning. In *The Cambridge Handbook of Multimedia Learning.*
- ◦ Centre for Education Statistics and Evaluation (2017). Cognitive load
  theory: research that teachers really need to understand. NSW
  Department of Education.
  https://education.nsw.gov.au/content/dam/main-education/about-us/educational-data/cese/2017-cognitive-load-theory.pdf
- Adobe / The Readability Consortium — personalized-typography studies
  (~21% reading-speed gains).

### Trust, automation bias & imperfect cueing

- Parasuraman, R., & Manzey, D. H. (2010). Complacency and bias in human
  use of automation. *Human Factors, 52*(3).
- Wickens, C. D., & Dixon, S. R. (2007). The benefits of imperfect
  diagnostic automation: a synthesis of the literature. *Theoretical
  Issues in Ergonomics Science, 8*(3).
  https://doi.org/10.1080/14639220500370105
- Wickens, C. D., et al. (2009; 2015). False-alerting and imperfect-alert
  syntheses.
- ◦ Warden, A. C., Wickens, C. D., Rehberg, D., Ortega, F. R., & Clegg,
  B. A. (2023). Fast, accurate, but sometimes too-compelling support:
  imperfectly automated cues in an AR head-mounted display.
  arXiv:2303.14300. https://arxiv.org/abs/2303.14300
- Galletta, D. F., Durcikova, A., Everard, A., & Jones, B. M. (2005).
  Does spell-checking software need a warning label? *CACM, 48*(7).
- Sorkin, R. D. (1988). Why are people turning off our alarms? *Journal
  of the Acoustical Society of America, 84*(3).
- Meyer, J. (2004). Conceptual issues in the study of dynamic hazard
  warnings. *Human Factors, 46*(2).
- Dixon, S. R., Wickens, C. D., & McCarley, J. S. (2007). On the
  independence of compliance and reliance. *Human Factors, 49*(4).

### Prior art: systems & tools

- Fok, R., Kambhamettu, H., Soldaini, L., Bragg, J., Lo, K., Hearst, M.,
  Head, A., & Weld, D. S. (2023). Scim: intelligent skimming support for
  scientific papers. *IUI 2023*.
  https://dl.acm.org/doi/fullHtml/10.1145/3581641.3584034 — scale-up: The
  Semantic Reader Project, https://dl.acm.org/doi/full/10.1145/3665648
- ◦ Vasconcelos, H., Bansal, G., Fourney, A., Liao, Q. V., & Wortman
  Vaughan, J. (2025). Generation probabilities are not enough:
  uncertainty highlighting in AI code completions. *ACM TOCHI /
  CHI'25*. https://arxiv.org/abs/2302.07248
- ◦ Gehrmann, S., Strobelt, H., & Rush, A. M. (2019). GLTR: statistical
  detection and visualization of generated text. *ACL 2019 demo*.
  http://gltr.io/ — arXiv:1906.04043
- OpenAI logprobs cookbook (token-probability coloring).
  https://cookbook.openai.com/examples/using_logprobs
- Interpretability-tool instability & stopword-highlighting failures.
  https://arxiv.org/pdf/2605.28060
- Bionic Reading null results: Readwise (n=2,074),
  https://blog.readwise.io/does-bionic-reading-actually-work/ ; Acta
  Psychologica (2024),
  https://www.sciencedirect.com/science/article/pii/S0001691824001811 ;
  SAGE Open eye-tracking (2025),
  https://journals.sagepub.com/doi/10.1177/21582440251376158 ; boldface
  study, *Attention, Perception & Psychophysics* (2025),
  https://link.springer.com/article/10.3758/s13414-025-03067-w
- IDE de-emphasis precedent: JetBrains/ReSharper dead-code gray-out;
  Focus Mode, https://github.com/mallowigi/focus-mode ; neodim.
  Syntactic vs semantic highlighting:
  https://tomassetti.me/syntactic-vs-semantic-highlighting/
- Terminal colorizers: grc; ccze; ChromaTerm,
  https://pypi.org/project/chromaterm/ ; lnav, https://lnav.org/features
- Streaming markdown renderers: Streamdown,
  https://github.com/day50-dev/Streamdown ; md2term; Will McGugan,
  https://willmcgugan.github.io/streaming-markdown/ ; rich streaming PR,
  https://github.com/simonw/llm/pull/571
- LLM TUIs (no importance emphasis): aichat, elia, oterm, gptme, oatmeal,
  aider, opencode, Crush.
- Claude Code ecosystem: tweakcc,
  https://github.com/Piebald-AI/tweakcc ; claude-powerline; claude-hud;
  claude-code-log; cc.nvim.
- Hemingway Editor (author-facing prose defects).
  https://hemingwayapp.com/
- Browser AI article highlighters (single facet, web-only): AI Article
  Highlighter; Highlight AI; SummarLight.
- ◦ Ahmed, S. S., Wang, S., Tian, Y., Chen, T.-H., & Zhang, H. (2024).
  Studying and recommending information highlighting in Stack Overflow
  answers. arXiv:2401.01472. https://arxiv.org/pdf/2401.01472
- Restructuring research: Graphologue (*UIST 2023*); WaitGPT; Sensecape.

### Claude Code extensibility (why salcc is a PTY proxy)

- Hooks reference: https://code.claude.com/docs/en/hooks.md (also
  plugins-reference.md, output-styles.md, statusline.md,
  agent-sdk/streaming-output.md, headless.md — no rendering extension
  point exists).
- Rendering/theming plugin API feature requests, closed as duplicates:
  anthropics/claude-code issues
  [#42189](https://github.com/anthropics/claude-code/issues/42189),
  [#1302](https://github.com/anthropics/claude-code/issues/1302),
  [#22367](https://github.com/anthropics/claude-code/issues/22367),
  [#25366](https://github.com/anthropics/claude-code/issues/25366),
  [#29166](https://github.com/anthropics/claude-code/issues/29166),
  [#8504](https://github.com/anthropics/claude-code/issues/8504).

## License

TBD (will be a permissive OSI license before first release).
