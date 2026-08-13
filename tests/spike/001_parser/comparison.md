# Parser toolchain spike — comparison (issue #1)

Four Rust parsing approaches implemented against the shared grammar subset in
`grammar/sqlite-subset.ebnf` (CREATE TABLE, INSERT, SELECT, UPDATE, DELETE,
plus the shared expression grammar), each tested against the same
`fixtures/valid.sql` (30 statements, all must parse) and `fixtures/invalid.sql`
(20 statements, all must be rejected). All four passed 30/30 and 20/20 with
no panics.

Source: `001_lemon-rs/`, `002_pomelo/`, `003_lalrpop/`, `004_pest/`. Each is a
standalone crate — no shared workspace, no code shared between variants
beyond the grammar/fixtures they were all pointed at.

## Results

| | lemon-rs | pomelo | lalrpop | pest |
|---|---|---|---|---|
| Paradigm | LALR(1) (Lemon, vendored C tool) | LALR(1) (Lemon-inspired proc-macro) | LR(1) (own DSL, build.rs codegen) | PEG (no separate lexer) |
| valid.sql | 30/30 | 30/30 | 30/30 | 30/30 |
| invalid.sql | 20/20 | 20/20 | 20/20 | 20/20 |
| Grammar conflicts | 0 | 0 | 0 | n/a (PEG has no conflict concept) |
| Perf (release, µs/statement) | ~1.3–1.4 | ~1.8–2.2 | ~9.3\* | ~4.1 |
| Runtime deps | 0 (vendors lemon.c + lempar.rs) | 0 | 2 (lalrpop-util + generated regex) | ~10 (all permissive) |
| License | Unlicense / public domain | MIT OR Apache-2.0 | MIT OR Apache-2.0 | MIT OR Apache-2.0 |
| Precedence declared how | `%left`/`%right` on flat `expr` | `[Prec]` markers on flat `expr` | Layered nonterminals (no native precedence op) | Layered nonterminals (no `PrattParser` used) |
| Error message quality | token + byte offset only, no expected-set | token + byte offset; `ExpectedTokens` list (sometimes empty); degrades to generic "unexpected EOF" for ~40% of truncated inputs | token + position; `expected: Vec<String>` available but not in default `Display` | line/col + source excerpt + caret + expected-set, all free from `Display`; degrades under backtracking (doesn't always name the real problem, e.g. an unclosed paren) |

\* lalrpop's built-in lexer costs ~1ms to construct — must be cached (e.g. `OnceLock`) across parses. Cached-vs-uncached is a ~100x difference and the number above is cached. This is a real footgun for a naive integration and worth flagging: an easy-to-miss one-time setup cost that skews naive benchmarks by two orders of magnitude.

## Cross-cutting findings (apply to any toolchain choice, not just one variant)

1. **Chained comparisons are ambiguous in the shared EBNF as written.** The
   grammar text (`comparison-expr ::= additive-expr [ comparison-op
   additive-expr ]`) reads as non-associative, but the precedence-ladder
   comment says to mirror `parse.y`'s `%left` declarations exactly, which
   makes chains like `1 = 2 = 3` legal (left-associative). pomelo followed
   the comment and accepts it; this should be reconciled in the grammar file
   before it's used as anything other than a spike reference — pick one and
   say so explicitly, since right now the prose and the precedence rule
   disagree.
2. **`SELECT count(*)` is out of scope for all four**, per the EBNF's
   `function-call` rule (no bare `*` argument). Confirmed as a grammar gap,
   not a toolchain difference — every variant agrees, so it's not
   discriminating between tools, just a known subset limitation to fix
   before extending past the spike.
3. **SQLite's `%fallback ID` (keywords double as identifiers) was not
   attempted by any variant** — all four hard-reserve the subset's ~30
   keywords. This is fine for the spike's fixtures, but the four toolchains
   are **not equally capable of adding it back**: LALR tools (lemon-rs,
   pomelo, lalrpop) can special-case a fallback token class relatively
   directly; pest's ordered-choice/PEG model would need real per-keyword
   lookahead logic to reproduce the same behavior without becoming
   ambiguous. This is a genuine long-term cost specific to the PEG choice,
   not just a fixture gap.
4. **Debug vs. release matters enormously and identically across all four**
   (roughly 10x). Every perf number above is release-only; make sure future
   comparisons state the profile.

## Ergonomics summary (condensed from each variant's full report)

- **lemon-rs**: Highest fidelity to real SQLite (`parse.y` ports almost
  verbatim, 1:1 precedence declarations), fastest, and public-domain
  licensed — but by far the roughest on-ramp: the vendored `lempar.rs`
  template has an undocumented, reverse-engineered stack-access contract,
  requires an undiscoverable `-m` flag and an oddly-named `NDEBUG` feature to
  even compile, and Lemon scans action blocks as C — meaning a Rust
  lifetime's `'` can silently break grammar parsing. Semantic-value type
  errors surface as runtime panics (`unreachable!()`), not compile errors.
  ~80 lines of hand-written, unowned glue code is now a permanent
  maintenance liability of this path.
- **pomelo**: Nearly the same fidelity/precedence story as lemon-rs (flat
  `expr` with `[Prec]` markers, near 1:1 with `parse.y`), zero runtime
  dependencies, no build.rs/C toolchain needed, and the best fidelity-to-
  effort ratio of the four. Real weaknesses: no way to inspect the
  generated tables or get a conflict report (a larger grammar could hit an
  invisible shift/reduce conflict), and ~40% of the truncated-input fixtures
  get a generic "unexpected end of input" instead of a real error, because
  the macro short-circuits to `%parse_fail` at end-of-input and skips
  `%syntax_error` entirely — a real production concern for a SQL engine's
  error UX.
- **lalrpop**: Best-in-class compile-time diagnostics (ambiguity/conflict
  errors come with file:line:col spans and ASCII parse-tree drawings of the
  competing interpretations) and the most ergonomic grammar DSL (typed
  rules, named bindings, built-in `T*`/`T?`/user macros eliminate most list/
  optional boilerplate). Costs: no native precedence declarations (every
  level of the ladder needs its own hand-written nonterminal — fine for this
  subset, would mean ~15 nonterminals for SQL's full operator table), a
  17.8k-line generated file from 292 grammar lines, and the built-in lexer's
  ~1ms one-time construction cost is an easy trap for naive benchmarking.
- **pest**: No separate lexer needed at all — one grammar file handles
  lexing and parsing, and it compiled clean on the first try with zero
  conflicts to debug (PEG has no such concept). Best default error messages
  (line/col + source excerpt + caret + expected-set, free from `Display`).
  Real costs: ordered choice is a silent-failure hazard (rule ordering
  determines correctness with no compiler warning if you get it wrong — the
  spike hit this directly with `function_call` needing to precede
  `column_ref`), keyword word-boundary handling required routing around a
  whitespace/atomic-rule interaction, and error quality degrades under
  backtracking (doesn't always identify the real cause, e.g. an unclosed
  paren). Adding `%fallback`-equivalent keyword behavior later would be
  structurally harder here than in any LALR/LR variant (finding #3 above).

## Recommendation

**Carry forward `pomelo`.** It gets nearly all of lemon-rs's strategic value
— grammar rules and precedence declarations that transliterate close to 1:1
from SQLite's actual `parse.y`, which matters a great deal for a
binary-compatible reimplementation where "port parse.y" beats "re-derive a
grammar" — without lemon-rs's maintenance liability: no vendored C tool, no
undocumented template-internals to reverse-engineer, no C-lexing-Rust-
action-blocks footgun, zero runtime dependencies, and ordinary Rust compile
errors instead of runtime `unreachable!()` panics. Its two known weaknesses
(no visibility into LALR conflicts on a larger grammar; degraded error
messages on end-of-input) are real but tractable — the second is a known,
documented macro limitation that a thin wrapper around tokenization can
likely work around (e.g. detecting EOF before invoking the parser and
producing a better message ourselves), and the first is a one-time risk to
watch for once the grammar grows past this spike's subset, not a structural
dead end.

lalrpop is the strongest runner-up and the safer choice if compile-time
grammar diagnostics (not runtime discovery of conflicts) are judged more
valuable than fidelity to `parse.y` — worth revisiting if the pomelo path
hits a conflict it can't diagnose once the grammar grows toward the real
~200 production rules. lemon-rs remains the fallback if strict parse.y
fidelity turns out to matter more than the maintenance cost once we're
further in — the spike proves it *can* be made to work, just at a real
ongoing cost. pest is not recommended for the main grammar: the
ordered-choice hazard and the structural difficulty of ever reproducing
`%fallback ID` are real risks for a binary-compatible SQL engine, though it
remains attractive for narrower, definitely-PEG-shaped problems (e.g. a
future standalone tokenizer or config-file parser elsewhere in the project).

## Spend

Issue #1 estimate: medium. Actual: 4 parallel subagents building/testing
independent crates plus this synthesis — roughly matched the estimate,
skewed slightly over by lemon-rs's build.rs/lempar.rs integration taking
longer than the other three combined (expected, given it was flagged as the
highest-risk variant going in).
