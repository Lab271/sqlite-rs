---
domain: parser
version: 0.1.0
status: draft
date: 2026-08-13
---

# 002 — Parser

The sqlite-rs parser transforms SQL text into an Abstract Syntax Tree (AST). This spec defines the tokenizer, grammar, and parser generator strategy.

**Grammar source of truth:** [`.openspec/grammar/sqlite.ebnf`](../../grammar/sqlite.ebnf) — a structural EBNF re-derivation of SQLite's [`parse.y`](https://github.com/sqlite/sqlite/blob/version-3.53.4/src/parse.y) (pinned 3.53.4; SQLite publishes no EBNF — see [lang.html](https://www.sqlite.org/lang.html) and [syntaxdiagrams.html](https://www.sqlite.org/syntaxdiagrams.html)), V-block-annotated per rule and drift-checked against parse.y by `make check-grammar-drift` (`tools/grammar_drift.py`).

## Tier Position

The full parser is **Tier 1** in the tier model ([plan.md](../../plan.md#core-definition--drop-order)). It is deliberately **not** part of the Tier 0 READ CORE: reading existing databases uses a *minimal DDL reader* that extracts table/column names and types from `sqlite_master` DDL text without a full grammar. This keeps the never-droppable core free of the ~200-production grammar.

Grammar productions land tier-by-tier:

| Tier | Grammar scope |
|------|---------------|
| Tier 0 | None (minimal DDL reader, separate component) |
| Tier 1 | Tokenizer complete; SELECT core + expressions (~40 productions) |
| Tier 2 | DML + core DDL + transactions (~100 cumulative) |
| Tier 3 | Joins/subqueries/CTEs, triggers, windows, virtual tables (~200+, in drop order) |

Consequence for the tokenizer: it is built **once, completely** (all ~140 keywords) at Tier 1 — keyword recognition is cheap and retrofitting it is not.

## Philosophy

SQLite uses a Lemon-generated LALR(1) parser. The grammar is defined in `parse.y` (~3,500 lines) and processed by Lemon (~6,000 lines) to produce `parse.c`. Spike 001 (issue #1, `tests/spike/001_parser/comparison.md`) built and benchmarked four real variants against a shared subset grammar:

1. **lemon-rs** — Rust port of Lemon with SQLite grammar already ported
2. **pomelo** — Lemon-as-a-proc-macro, LALR(1), zero runtime deps
3. **lalrpop** — Rust-native LALR(1) generator, rewrite grammar
4. **pest** — PEG, not LALR

**Decision:** **pomelo** — near-1:1 transliteration of `parse.y`'s precedence/rules, ordinary Rust compile errors instead of lemon-rs's runtime `unreachable!()` panics, no runtime dependency. lalrpop is the fallback if compile-time diagnostics matter more than parse.y fidelity; pest is not recommended for the main grammar (ordered-choice hazard, can't cleanly reproduce `%fallback ID`). Spike 006 (issue #57, `tests/spike/006_grammar_slice/FINDINGS.md`) confirmed the V2 SELECT-core subset slices out of pomelo's grammar cleanly and grows to V3 by addition only.

## SQLite Grammar Statistics

From SQLite 3.53:

| Metric | Count |
|--------|-------|
| **Terminals** | ~140 |
| **Nonterminals** | ~90 |
| **Production rules** | ~200 |
| **Tokenizer keywords** | ~140 |
| **Grammar file** | ~3,500 lines |

## Lemon Parser Generator

### What Lemon Is

Lemon is an LALR(1) parser generator created by D. Richard Hipp for SQLite. Key differences from yacc/bison:

| Feature | Yacc/Bison | Lemon |
|---------|------------|-------|
| Parser calls tokenizer | Yes | No — **tokenizer calls parser** |
| Global variables | Yes | **No** — thread-safe |
| Memory management | Manual | Destructor callbacks |
| Error recovery | yyerrok | Cleaner mechanism |
| Reentrant | No | **Yes** |

### Lemon Components

| File | Lines | Purpose |
|------|-------|---------|
| `lemon.c` | 6,075 | Parser generator tool |
| `lempar.c` | ~800 | Template for generated parser |
| `parse.y` | ~3,500 | SQLite SQL grammar |
| `parse.c` | ~8,000 | Generated parser (output) |

### Lemon Rule Syntax

```
stmt ::= SELECT select_core(S) orderby_opt(O) limit_opt(L). {
    S->pOrderBy = O;
    S->pLimit = L;
    sqlite3Select(pParse, S);
}
```

- `::=` separates LHS from RHS
- Parenthesized names `(S)` bind semantic values
- Braces contain C action code
- Terminals are UPPERCASE
- Nonterminals are lowercase

### Rust Alternatives

The landscape surveyed before the Decision above, which selected
**pomelo**; this table records what was compared, not a recommendation.

| Tool | Type | SQLite grammar? | Notes |
|------|------|-----------------|-------|
| **lemon-rs** | Lemon port | Yes (already ported) | Runtime `unreachable!()` panics on grammar error |
| **pomelo** | Lemon as proc-macro | Partial | Selected — see Decision above |
| **lalrpop** | LALR(1) native | Must rewrite | Fallback if compile-time diagnostics outweigh parse.y fidelity |
| **pest** | PEG | Must rewrite | Ordered-choice hazard; can't cleanly reproduce `%fallback ID` |
| **nom** | Combinators | Must hand-write | For tokenizer only |
| **tree-sitter** | Incremental | Community grammar | For tooling, not DB |

## Tokenizer

The tokenizer (lexer) converts SQL text into a stream of tokens.

### Token Categories

| Category | Examples |
|----------|----------|
| **Keywords** | `SELECT`, `FROM`, `WHERE`, `INSERT`, `CREATE`, `DROP` |
| **Identifiers** | `table_name`, `column`, `"quoted identifier"`, `[bracketed]` |
| **Literals** | `42`, `3.14`, `'string'`, `X'BLOB'`, `NULL` |
| **Operators** | `+`, `-`, `*`, `/`, `%`, `=`, `<>`, `<`, `>`, `<=`, `>=` |
| **Punctuation** | `(`, `)`, `,`, `;`, `.` |
| **Special** | `?`, `?NNN`, `:name`, `@name`, `$name` (parameters) |

### SQLite Keywords (~140)

```
ABORT ACTION ADD AFTER ALL ALTER ALWAYS ANALYZE AND AS ASC ATTACH
AUTOINCREMENT BEFORE BEGIN BETWEEN BY CASCADE CASE CAST CHECK COLLATE
COLUMN COMMIT CONFLICT CONSTRAINT CREATE CROSS CURRENT CURRENT_DATE
CURRENT_TIME CURRENT_TIMESTAMP DATABASE DEFAULT DEFERRABLE DEFERRED
DELETE DESC DETACH DISTINCT DO DROP EACH ELSE END ESCAPE EXCEPT
EXCLUDE EXCLUSIVE EXISTS EXPLAIN FAIL FILTER FIRST FOLLOWING FOR
FOREIGN FROM FULL GENERATED GLOB GROUP GROUPS HAVING IF IGNORE
IMMEDIATE IN INDEX INDEXED INITIALLY INNER INSERT INSTEAD INTERSECT
INTO IS ISNULL JOIN KEY LAST LEFT LIKE LIMIT MATCH NATURAL NO NOT
NOTHING NOTNULL NULL NULLS OF OFFSET ON OR ORDER OTHERS OUTER OVER
PARTITION PLAN PRAGMA PRECEDING PRIMARY QUERY RAISE RANGE RECURSIVE
REFERENCES REGEXP REINDEX RELEASE RENAME REPLACE RESTRICT RIGHT
ROLLBACK ROW ROWS SAVEPOINT SELECT SET TABLE TEMP TEMPORARY THEN TIES
TO TRANSACTION TRIGGER UNBOUNDED UNION UNIQUE UPDATE USING VACUUM
VALUES VIEW VIRTUAL WHEN WHERE WINDOW WITH WITHOUT
```

### Tokenizer Implementation

Real definitions live in `src/parser/tokenizer.rs`; this is the shape, not
a copy — consult the source for the full variant list and its doc comments.

```rust
pub struct Span {
    pub line: u32,    // 1-based
    pub column: u32,  // 1-based
    pub offset: u32,  // byte offset of first character
    pub len: u32,     // length in bytes
}

pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

pub enum Param {
    Anonymous,        // ?
    Numbered(u32),    // ?NNN
    Colon(String),    // :name
    At(String),       // @name
    Dollar(String),   // $name
}

pub enum TokenKind {
    // Literals
    Integer(i64),
    Float(f64),
    String(String),      // quotes stripped, escapes resolved
    Blob(Box<Vec<u8>>),  // X'...' decoded to raw bytes
    Null,
    True,
    False,

    Identifier(String),  // unquoted or quoted
    Keyword(Keyword),    // one enum for all reserved words
    Param(Box<Param>),

    // Punctuation / operators
    Star, Comma, Semicolon, LParen, RParen, Dot,
    Plus, Minus, Slash, Percent,
    Eq, Ne, Lt, Le, Gt, Ge,
    Concat, Arrow, ArrowArrow,
    BitAnd, BitOr, BitNot, Shl, Shr,

    Error(String),  // malformed input, with a human-readable reason
    Eof,
}
```

Three properties of this shape are load-bearing:

- **A token carries its own value.** There is no separate `value` field —
  `Integer(i64)`, `String(String)`, `Identifier(String)` and friends hold
  the scanned value in the variant, so a token is self-describing.
- **Keywords are one variant, not ~146.** `Keyword(Keyword)` wraps a
  separate `Keyword` enum rather than giving each reserved word its own
  `TokenKind`, which keeps `TokenKind` small and lets keyword recognition
  stay a single table lookup. `NULL`/`TRUE`/`FALSE` are the exceptions —
  they are literals, so they get their own variants.
- **`Blob` and `Param` are boxed.** Both are rare, and `Vec<u8>`/`Param`
  are wide enough that leaving them inline would roughly double every
  token's size to match the widest variant. `test_token_kind_size`
  enforces the resulting bound.

## Grammar Overview

### Top-Level Statements

| Statement | Production |
|-----------|------------|
| `SELECT` | `select_stmt ::= SELECT ...` |
| `INSERT` | `insert_stmt ::= INSERT INTO ...` |
| `UPDATE` | `update_stmt ::= UPDATE ...` |
| `DELETE` | `delete_stmt ::= DELETE FROM ...` |
| `CREATE TABLE` | `create_table_stmt ::= CREATE TABLE ...` |
| `CREATE INDEX` | `create_index_stmt ::= CREATE INDEX ...` |
| `CREATE VIEW` | `create_view_stmt ::= CREATE VIEW ...` |
| `CREATE TRIGGER` | `create_trigger_stmt ::= CREATE TRIGGER ...` |
| `DROP` | `drop_stmt ::= DROP (TABLE|INDEX|VIEW|TRIGGER) ...` |
| `ALTER TABLE` | `alter_table_stmt ::= ALTER TABLE ...` |
| `PRAGMA` | `pragma_stmt ::= PRAGMA ...` |
| `BEGIN/COMMIT/ROLLBACK` | Transaction control |
| `ATTACH/DETACH` | Database attachment |
| `VACUUM` | Database compaction |
| `REINDEX` | Index rebuild |
| `ANALYZE` | Statistics collection |
| `EXPLAIN` | Query plan display |

### SELECT Grammar (simplified)

```
select_stmt
    ::= with_clause? select_core compound_op* orderby_opt limit_opt

select_core
    ::= SELECT distinct_opt result_columns
        FROM join_source
        where_opt
        group_by_opt
        having_opt
        window_clause?

compound_op
    ::= UNION ALL?
    |   INTERSECT
    |   EXCEPT

result_columns
    ::= result_column (',' result_column)*

result_column
    ::= '*'
    |   table_name '.' '*'
    |   expr (AS? alias)?

join_source
    ::= table_or_subquery (join_op table_or_subquery join_constraint?)*

join_op
    ::= ','
    |   NATURAL? (LEFT OUTER? | RIGHT OUTER? | FULL OUTER? | INNER | CROSS)? JOIN

table_or_subquery
    ::= table_name (AS? alias)? indexed_opt
    |   '(' select_stmt ')' (AS? alias)?
    |   '(' join_source ')'
```

### Expression Grammar (simplified)

```
expr
    ::= literal
    |   column_ref
    |   unary_op expr
    |   expr binary_op expr
    |   expr IS NOT? NULL
    |   expr NOT? BETWEEN expr AND expr
    |   expr NOT? IN '(' (expr_list | select_stmt) ')'
    |   expr NOT? LIKE expr (ESCAPE expr)?
    |   CASE expr? (WHEN expr THEN expr)+ (ELSE expr)? END
    |   CAST '(' expr AS type_name ')'
    |   function_call
    |   '(' expr ')'
    |   '(' select_stmt ')'
    |   EXISTS '(' select_stmt ')'

binary_op
    ::= '||' | '*' | '/' | '%' | '+' | '-'
    |   '<<' | '>>' | '&' | '|'
    |   '<' | '<=' | '>' | '>='
    |   '=' | '==' | '!=' | '<>'
    |   IS | IS NOT | IN | LIKE | GLOB | MATCH | REGEXP
    |   AND | OR

unary_op
    ::= '-' | '+' | '~' | NOT
```

## AST Data Structures

Real definitions live in `src/parser/ast.rs`, which is heavily
doc-commented; this is the shape, not a copy.

```rust
// No unifying `Stmt` enum: each statement is its own top-level struct,
// and dispatch happens on the leading keyword in `codegen::dispatch`.
pub struct Select {
    pub with_clause: Option<WithClause>,
    pub distinct: Option<Distinctness>,
    pub columns: Vec<ResultColumn>,
    pub from: Option<FromClause>,
    pub where_clause: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub compound: Vec<CompoundSelect>,   // UNION [ALL] arms
    pub order_by: Vec<OrderingTerm>,     // applies to the whole compound
    pub limit: Option<Limit>,            // applies to the whole compound
    pub span: Span,
}

// Sibling statement structs: Insert, Update, Delete, CreateTable,
// CreateIndex, DropTable, CreateView, DropView, DropIndex, Begin,
// Commit, Rollback, Explain, Analyze, Pragma.

pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

pub enum ExprKind {
    Literal(Literal),
    Param(ParamKind),
    Column { table: Option<String>, catalog: Option<String>, name: String },
    FunctionCall { .. },
    Unary { .. },
    Binary { .. },
    Is { .. },
    IsNull { .. },
    Between { .. },
    In { .. },
    Like { .. },
    Case { .. },
    Cast { .. },
    Collate { .. },
    Paren(Box<Expr>),
    Subquery(Box<Select>),
    Exists { .. },
    InSubquery { .. },
    InSubqueryMulti { .. },
}
```

Three shape decisions worth stating, because they differ from the obvious
design:

- **There is no `Stmt` enum.** Each statement kind is a flat top-level
  struct. Nothing needs to match over "any statement" — `compile_statement`
  dispatches on leading keywords (`codegen::dispatch::leading_keywords`)
  and calls the specific parser, so a wrapping enum would only add a layer
  to unwrap.
- **`Select` is flat, with no `SelectBody`/`SelectCore` layering.** A
  compound `SELECT` is the first arm's own core plus a `compound: Vec<..>`
  of further arms, rather than a recursive left/right tree. `order_by` and
  `limit` sit on the `Select` itself because SQLite's grammar only lets the
  outermost `select-stmt` carry them.
- **`Expr` is a struct wrapping `ExprKind`, not an enum.** Every expression
  carries a `span` regardless of kind, so the span lives on the struct
  instead of being repeated into every variant.

## Requirements

### Requirement 1: Tokenizer [MUST]

The tokenizer MUST convert SQL text into a stream of tokens. Each token MUST carry source location for error reporting.

**Implementation:** `src/parser/tokenizer.rs`

#### Scenario: Tokenize SELECT

- GIVEN `SELECT a, b FROM t WHERE x > 10`
- WHEN tokenized
- THEN tokens: `[Select, Identifier("a"), Comma, Identifier("b"), From, Identifier("t"), Where, Identifier("x"), Gt, Integer(10)]`

**Tests:** `src/parser/tokenizer.rs::test_tokenize_select`

#### Scenario: Tokenize string literals

- GIVEN `'hello''world'` (SQLite escaping)
- WHEN tokenized
- THEN one String token with value `hello'world`

**Tests:** `src/parser/tokenizer.rs::test_tokenize_string_literal_escaping`

#### Scenario: Tokenize blob literal

- GIVEN `X'48454C4C4F'`
- WHEN tokenized
- THEN one Blob token with bytes `[72, 69, 76, 76, 79]` ("HELLO")

**Tests:** `src/parser/tokenizer.rs::test_tokenize_blob_literal`

#### Scenario: Tokenize parameters

- GIVEN `?, ?1, :name, @var, $param`
- WHEN tokenized
- THEN five Param tokens with appropriate kinds

**Tests:** `src/parser/tokenizer.rs::test_tokenize_parameters`

### Requirement 2: Grammar Compatibility [MUST]

The parser MUST accept all SQL that SQLite accepts, and reject all SQL that SQLite rejects.

**Implementation:** `src/parser/grammar.rs` (hand-written recursive descent covering SELECT core, DML, core DDL, transactions, PRAGMA and ANALYZE; a pomelo-generated grammar per spike 006 is future work — Requirement 6)

#### Scenario: Accept valid SELECT

- GIVEN `SELECT * FROM t`
- WHEN parsed
- THEN parse succeeds with SelectStmt AST

**Tests:** `tests/unit/parser.rs::test_accept_select_star`

#### Scenario: Accept CTE

- GIVEN `WITH cte AS (SELECT 1) SELECT * FROM cte`
- WHEN parsed
- THEN parse succeeds with WithClause in SelectStmt

**Tests:** `tests/unit/parser.rs::test_with_clause_single_cte`, `tests/unit/parser.rs::test_with_clause_multiple_ctes`, `tests/unit/parser.rs::test_with_clause_cte_with_column_list`, `tests/unit/parser.rs::test_with_clause_cte_referenced_in_from`, `tests/unit/parser.rs::test_with_recursive_is_unsupported`, `tests/unit/parser.rs::test_with_clause_printer_roundtrip`

#### Scenario: Accept compound SELECT (UNION / UNION ALL)

- GIVEN `SELECT a FROM t1 UNION SELECT b FROM t2` and `SELECT a FROM t1 UNION ALL SELECT b FROM t2`, including chained (`A UNION B UNION C`) and mixed (`A UNION B UNION ALL C`) arms
- WHEN parsed
- THEN parse succeeds with each arm as a `CompoundSelect` (tagged `CompoundOp::Union`/`CompoundOp::UnionAll`) appended to `Select::compound`; `INTERSECT`/`EXCEPT` remain `Unsupported` (deferred to V7), both at the top level and inside an `EXISTS`/`IN (...)`/scalar subquery

**Tests:** `tests/unit/parser.rs::test_accept_union_all`, `tests/unit/parser.rs::test_accept_multiple_union_all_arms`, `tests/unit/parser.rs::test_accept_union_all_with_trailing_order_by_limit`, `tests/unit/parser.rs::test_accept_union`, `tests/unit/parser.rs::test_accept_multiple_union_arms`, `tests/unit/parser.rs::test_accept_mixed_union_and_union_all_arms`, `tests/unit/parser.rs::test_union_inside_subquery_parses`, `tests/unit/parser.rs::test_unsupported_compound_select`, `tests/unit/parser.rs::test_compound_select_inside_subquery_is_unsupported_not_invalid`, `tests/unit/parser.rs::test_multi_column_in_rejects_compound_subquery`, `tests/corpus/parser_oracle_test.rs::parser_matches_oracle_three_way_outcome`

#### Scenario: Window function is a deliberate Unsupported, not Invalid

- GIVEN `SELECT row_number() OVER (ORDER BY x) FROM t` (this scenario's original title, "Accept window function," described a not-yet-built V9 capability and is stale — window functions are deferred, not accepted, per `tests/tiers/tier3.rs`'s drop-order 4)
- WHEN parsed
- THEN parse returns `Unsupported` (not `Invalid`) naming the window-function construct — the same not-yet-implemented-but-recognized pattern as compound SELECT's deferred `INTERSECT`/`EXCEPT`

**Tests:** `tests/unit/parser.rs::test_unsupported_window_function`

#### Scenario: Reject trailing comma

- GIVEN `SELECT a, b, FROM t` (invalid trailing comma)
- WHEN parsed
- THEN parse fails with error pointing to `FROM`

**Tests:** `tests/unit/parser.rs::test_error_on_missing_columns`, `tests/corpus/parser_oracle_test.rs::parser_matches_oracle_three_way_outcome`

#### Scenario: SQL text corpus labels match real SQLite

- GIVEN the three-way labeled corpus at `tests/corpus/sql/{valid_in_subset,valid_out_of_subset,invalid}/*.sql` (#2), covering the V2 SELECT-core subset plus representative V3/V4+ statements and malformed SQL
- WHEN each statement runs against the pinned oracle
- THEN `valid_in_subset` and `valid_out_of_subset` statements succeed and `invalid` statements are rejected — validating the corpus's labels ahead of the real parser (#61), which will replace the oracle check with sqlite-rs's own parser

**Tests:** `tests/corpus/sql_corpus_test.rs::valid_in_subset_statements_parse_in_real_sqlite`, `tests/corpus/sql_corpus_test.rs::valid_out_of_subset_statements_parse_in_real_sqlite`, `tests/corpus/sql_corpus_test.rs::invalid_statements_are_rejected_by_real_sqlite`

#### Scenario: Accept CREATE/DROP TABLE and CREATE/DROP INDEX

- GIVEN `CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT NOT NULL DEFAULT 'x', CHECK (a > 0)) STRICT`, `DROP TABLE IF EXISTS t`, `CREATE UNIQUE INDEX IF NOT EXISTS i ON t (a, b DESC) WHERE a > 0`, and `DROP INDEX IF EXISTS i`
- WHEN parsed
- THEN parse succeeds with `CreateTable`/`DropTable`/`CreateIndex`/`DropIndex` AST; `REFERENCES`/`FOREIGN KEY` (deferred to V8), `CREATE VIRTUAL TABLE`, `CREATE TEMP TABLE`, and `ON CONFLICT` resolution clauses are `Unsupported`, not `Invalid`

**Tests:** `tests/unit/ddl_parser.rs::test_accept_create_table_basic`, `tests/unit/ddl_parser.rs::test_accept_create_index_basic`, `tests/unit/ddl_parser.rs::test_accept_drop_table`, `tests/unit/ddl_parser.rs::test_accept_drop_index`, `tests/unit/ddl_parser.rs::test_unsupported_create_table_references`

#### Extraction process (#70)

The hand-curated corpus above is complemented by SQL extracted from the two
external suites SQLite itself is validated against. `tools/extract_sql_corpus.py`
performs the extraction; `make extract-sql-corpus` regenerates it offline.

- **Sources.** sqllogictest (`gregrahn/sqllogictest` mirror, pinned by commit
  SHA — sqlite.org's Fossil tarball endpoint serves an HTML anti-robot page
  with a `200` status and is not fetchable by tooling) and SQLite's own TCL
  suite (`test/*.test` at the tag matching `[package.metadata.oracle]`).
- **Vendoring.** Upstream is 110 MB and 13 MB respectively and is not
  committed. A curated subset of source `.test` files is vendored verbatim
  under `tests/corpus/sql/vendor/` with provenance recorded in its README, and
  the committed extraction is generated from that subset — so a clean checkout
  reproduces it byte-identically with no network access.
- **Parsing.** sqllogictest's `statement ok|error` and `query <types> <sort>`
  blocks; TCL's `do_execsql_test` / `do_catchsql_test` / bare `execsql` brace
  blocks. Statements whose SQL is built by TCL interpolation (`$var`, `[cmd]`,
  `%s`) are skipped and counted — resolving them needs a TCL interpreter.
- **Labels are honoured.** `statement error`, `do_catchsql_test` and
  `catch`-wrapped `execsql` blocks hold deliberately-invalid SQL and are
  excluded from the valid corpus rather than mislabeled into it.
- **Representativeness over volume.** The generated sqllogictest files differ
  only in literal values, so each statement is reduced to a shape key
  (literals normalized away) and capped per distinct shape. A cap of N
  therefore buys N structurally different statements. Every dropped statement
  is counted and reported by category; nothing is silently truncated.
- **Category yield is uneven by design.** sqllogictest is a query suite whose
  DML is incidental setup, so INSERT/UPDATE/DELETE/DDL diversity comes
  predominantly from the TCL suite. `update` and `delete` fall short of the
  ~1000-per-category target because the corpora do not contain that many
  structurally distinct statements at a vendorable size.

#### Scenario: Extracted corpus tokenizes without error

- GIVEN the extracted corpus at `tests/corpus/sql/{select,insert,update,delete,ddl}/*.sql`, every statement of which real SQLite accepted in the suite it came from
- WHEN each statement is tokenized by sqlite-rs
- THEN no statement produces `TokenKind::Error` — the tokenizer is total over SQL real SQLite lexes

**Tests:** `tests/corpus/extracted_sql_test.rs::every_extracted_tcl_statement_tokenizes_without_error`, `tests/corpus/extracted_sql_test.rs::every_extracted_sqllogictest_statement_tokenizes_without_error`, `tests/corpus/extracted_sql_test.rs::extracted_corpus_is_present_tcl`, `tests/corpus/extracted_sql_test.rs::extracted_corpus_is_present_sqllogictest`

#### Scenario: Extracted SELECT is never misreported as invalid

- GIVEN the extracted SELECT corpus, which is valid SQL by construction
- WHEN each statement is parsed by `parse_select`
- THEN `Accepted` and `Unsupported` are both acceptable (the V2 grammar is a deliberate slice) but `Invalid` is not, since it asserts valid SQL is malformed
- AND the count of such misclassifications is held to a documented baseline that may only decrease — currently non-zero for subquery-in-FROM, `IN <table-name>`, schema-qualified names, `HAVING` without `GROUP BY`, `->`/`->>`, `NOT INDEXED` and bare `VALUES` (tracked by #110)

**Tests:** `tests/corpus/extracted_sql_test.rs::no_extracted_select_is_reported_invalid`

### Requirement 3: AST Completeness [MUST]

The AST MUST represent all SQLite SQL constructs without loss of information.

**Implementation:** `src/parser/ast.rs`, `src/parser/printer.rs` (roundtrip)

#### Scenario: Preserve column aliases

- GIVEN `SELECT a AS alias`
- WHEN parsed and unparsed
- THEN output MUST include `AS alias`

**Tests:** `tests/unit/parser.rs::test_preserve_column_alias`

#### Scenario: Preserve parentheses for precedence

- GIVEN `SELECT (a + b) * c`
- WHEN parsed
- THEN AST MUST represent grouping (not just operator precedence)

**Tests:** `tests/unit/parser.rs::test_preserve_parens_for_precedence`, `tests/unit/parser.rs::test_roundtrip_fixpoint`

#### Scenario: CREATE/DROP TABLE/INDEX round-trip

- GIVEN a parsed `CreateTable`, `CreateIndex`, `DropTable`, or `DropIndex`
- WHEN printed via `Display` and reparsed
- THEN the reparsed AST MUST equal the original

**Tests:** `tests/unit/ddl_parser.rs::test_printer_roundtrip_create_table`, `tests/unit/ddl_parser.rs::test_printer_roundtrip_create_table_without_rowid`, `tests/unit/ddl_parser.rs::test_printer_roundtrip_create_index`, `tests/unit/ddl_parser.rs::test_printer_roundtrip_drop_table`, `tests/unit/ddl_parser.rs::test_printer_roundtrip_drop_index`

#### Scenario: Accept CREATE VIEW / DROP VIEW

- GIVEN `CREATE VIEW v AS SELECT ...`, `CREATE VIEW v (a, b) AS SELECT ...`,
  or `DROP VIEW [IF EXISTS] v`
- WHEN parsed
- THEN the result MUST be `ParseOutcome::Accepted` with a `CreateView`
  (name, optional explicit column list, boxed `Select` query) or
  `DropView` AST node, and printing it via `Display` and reparsing MUST
  reproduce an equal AST

**Implementation:** `src/parser/ast.rs::CreateView`, `src/parser/ast.rs::DropView`, `src/parser/grammar.rs::Parser::parse_create_view_stmt`, `src/parser/grammar.rs::Parser::parse_drop_view_stmt`

**Tests:** `tests/unit/ddl_parser.rs::test_accept_create_view_simple`, `tests/unit/ddl_parser.rs::test_accept_create_view_with_column_list`, `tests/unit/ddl_parser.rs::test_accept_create_view_if_not_exists`, `tests/unit/ddl_parser.rs::test_printer_roundtrip_create_view`, `tests/unit/ddl_parser.rs::test_accept_drop_view`, `tests/unit/ddl_parser.rs::test_accept_drop_view_if_exists`

### Requirement 4: Error Messages [SHOULD]

Parse errors SHOULD include source location and helpful context.

**Implementation:** `src/parser/error.rs`

#### Scenario: Error on unexpected token

- GIVEN `SELECT FROM t` (missing columns)
- WHEN parsed
- THEN error SHOULD say "expected column or expression, found FROM at line 1, column 8"

**Tests:** `tests/unit/parser.rs::test_error_on_missing_columns`

### Requirement 5: Minimal DDL Reader Independence [MUST]

The Tier 0 minimal DDL reader (used to decode `sqlite_master` for the READ CORE) MUST NOT depend on the full parser. It extracts table names, column names, declared types, and WITHOUT ROWID / STRICT markers from DDL text — nothing more.

**Implementation:** `src/schema/ddl_reader.rs` (not under `src/parser/`)

**Tests:** inline `#[cfg(test)]` in `src/schema/ddl_reader.rs`

#### Scenario: Read schema without the parser

- GIVEN a build with the full parser feature-gated off
- WHEN sqlite-rs opens a database and dumps its rows
- THEN schema decoding MUST still work via the minimal DDL reader

`src/schema/ddl_reader.rs` has zero `use` of any `src/parser/` item (verified). No Cargo feature flag exists yet to gate the parser out, so the feature-gated-build half of this scenario remains unverified; no automated test backs it.

#### Scenario: Tolerate unparseable DDL

- GIVEN a `sqlite_master` entry with DDL the minimal reader does not understand (e.g. `CREATE VIRTUAL TABLE ... USING fts5(...)`)
- WHEN the schema is decoded
- THEN the entry MUST degrade to raw-row access with untyped columns, not an error

**Tests:** `src/schema/ddl_reader.rs::fts5_virtual_table_is_graceful_unknown_shadow_tables_are_readable`, `src/schema/ddl_reader.rs::unparseable_non_virtual_ddl_degrades_gracefully_never_errors`

### Requirement 6: Generator Swap [MAY]

The hand-written recursive-descent parser MAY be replaced by a pomelo-generated one (the Decision above; spike 006, #57). If it is, the pomelo grammar SHOULD be a near-1:1 transliteration of SQLite's `parse.y`, and the swap MUST NOT change the AST or diagnostics contract.

**Implementation:** `src/parser/grammar.rs` (planned) — the swap would replace this file's hand-written recursive descent; not yet started.

#### Scenario: Grammar parity

- GIVEN SQLite's `parse.y`
- WHEN compared to sqlite-rs grammar (`.openspec/grammar/sqlite.ebnf`)
- THEN all production rules SHOULD have direct correspondence

## Development Strategy

### Phase 1: Tokenizer (standalone)

1. Implement tokenizer with all SQLite token types
2. Test against SQLite tokenizer output
3. Benchmark performance

### Phase 2: Parser

1. Hand-written recursive descent against the tokenizer (done)
2. Optionally swap in a pomelo-generated parser (Requirement 6)
3. Test against SQLite parser (success/failure parity)

### Phase 3: AST Builder

1. Define complete AST types
2. Wire grammar actions to AST construction
3. Test roundtrip (parse → unparse → parse)

### Phase 4: Analyzer (later spec)

1. Name resolution
2. Type inference
3. Semantic validation

## References

- [SQLite parse.y](https://github.com/sqlite/sqlite/blob/master/src/parse.y)
- [Lemon Parser Generator](https://sqlite.org/lemon.html)
- [lemon-rs](https://github.com/gwenn/lemon-rs)
- [SQLite tokenize.c](https://github.com/sqlite/sqlite/blob/master/src/tokenize.c)
