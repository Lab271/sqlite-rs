---
domain: parser
version: 0.1.0
status: draft
date: 2026-08-13
---

# 002 — Parser

The sqlite-rs parser transforms SQL text into an Abstract Syntax Tree (AST). This spec defines the tokenizer, grammar, and parser generator strategy.

**Grammar source of truth:** [`.openspec/grammar/sqlite.ebnf`](../../grammar/sqlite.ebnf) — a structural EBNF re-derivation of SQLite's [`parse.y`](https://github.com/sqlite/sqlite/blob/version-3.53.4/src/parse.y) (pinned 3.53.4; SQLite publishes no EBNF — see [lang.html](https://www.sqlite.org/lang.html) and [syntaxdiagrams.html](https://www.sqlite.org/syntaxdiagrams.html)), V-block-annotated per rule and drift-checked against parse.y by `make grammar-drift` (`tools/grammar_drift.py`).

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

SQLite uses a Lemon-generated LALR(1) parser. The grammar is defined in `parse.y` (~3,500 lines) and processed by Lemon (~6,000 lines) to produce `parse.c`. We have three options:

1. **Use lemon-rs** — Rust port of Lemon with SQLite grammar already ported
2. **Use lalrpop** — Rust-native LALR(1) generator, rewrite grammar
3. **Hand-write** — Recursive descent like rustc

**Decision:** Start with **lemon-rs** for maximum compatibility, evaluate migration to lalrpop if maintenance burden is high.

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

| Tool | Type | SQLite grammar? | Recommendation |
|------|------|-----------------|----------------|
| **lemon-rs** | Lemon port | Yes (already ported) | **Use this** |
| **pomelo** | Lemon as proc-macro | Partial | Evaluate later |
| **lalrpop** | LALR(1) native | Must rewrite | Cleaner but work |
| **pest** | PEG | Must rewrite | Different paradigm |
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

```rust
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    pub value: Option<String>,  // For identifiers, strings, numbers
}

pub struct Span {
    pub start: usize,   // Byte offset
    pub end: usize,
    pub line: u32,
    pub column: u32,
}

pub enum TokenKind {
    // Keywords
    Select, From, Where, Insert, Update, Delete, Create, Drop,
    Table, Index, View, Trigger, // ... ~140 more
    
    // Literals
    Integer(i64),
    Float(f64),
    String,
    Blob,
    Null,
    
    // Identifiers
    Identifier,
    QuotedIdentifier,
    
    // Operators
    Plus, Minus, Star, Slash, Percent,
    Eq, Ne, Lt, Gt, Le, Ge,
    And, Or, Not,
    
    // Punctuation
    LParen, RParen, Comma, Semicolon, Dot,
    
    // Parameters
    Param,          // ?
    ParamNum(u32),  // ?123
    ParamName,      // :name, @name, $name
    
    // Special
    Eof,
    Error,
}
```

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

```rust
pub enum Stmt {
    Select(SelectStmt),
    Insert(InsertStmt),
    Update(UpdateStmt),
    Delete(DeleteStmt),
    CreateTable(CreateTableStmt),
    CreateIndex(CreateIndexStmt),
    // ...
}

pub struct SelectStmt {
    pub with: Option<WithClause>,
    pub body: SelectBody,
    pub order_by: Option<Vec<OrderingTerm>>,
    pub limit: Option<Limit>,
}

pub enum SelectBody {
    Select(SelectCore),
    Compound {
        op: CompoundOp,
        left: Box<SelectBody>,
        right: Box<SelectBody>,
    },
}

pub struct SelectCore {
    pub distinct: Distinct,
    pub columns: Vec<ResultColumn>,
    pub from: Option<FromClause>,
    pub where_clause: Option<Expr>,
    pub group_by: Option<GroupBy>,
    pub having: Option<Expr>,
    pub window: Option<Vec<WindowDef>>,
}

pub enum Expr {
    Literal(Literal),
    Column(ColumnRef),
    Unary { op: UnaryOp, operand: Box<Expr> },
    Binary { op: BinaryOp, left: Box<Expr>, right: Box<Expr> },
    Between { expr: Box<Expr>, low: Box<Expr>, high: Box<Expr>, not: bool },
    In { expr: Box<Expr>, values: InValues, not: bool },
    Like { expr: Box<Expr>, pattern: Box<Expr>, escape: Option<Box<Expr>>, not: bool },
    Case { operand: Option<Box<Expr>>, cases: Vec<WhenClause>, else_expr: Option<Box<Expr>> },
    Cast { expr: Box<Expr>, type_name: TypeName },
    FunctionCall(FunctionCall),
    Subquery(Box<SelectStmt>),
    Exists(Box<SelectStmt>),
    // ...
}
```

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

**Implementation:** `src/parser/grammar.rs` (planned) or `src/parser/parse.y` (if using lemon-rs)

#### Scenario: Accept valid SELECT

- GIVEN `SELECT * FROM t`
- WHEN parsed
- THEN parse succeeds with SelectStmt AST

#### Scenario: Accept CTE

- GIVEN `WITH cte AS (SELECT 1) SELECT * FROM cte`
- WHEN parsed
- THEN parse succeeds with WithClause in SelectStmt

#### Scenario: Accept window function

- GIVEN `SELECT row_number() OVER (ORDER BY x) FROM t`
- WHEN parsed
- THEN parse succeeds with window function in result column

#### Scenario: Reject trailing comma

- GIVEN `SELECT a, b, FROM t` (invalid trailing comma)
- WHEN parsed
- THEN parse fails with error pointing to `FROM`

### Requirement 3: AST Completeness [MUST]

The AST MUST represent all SQLite SQL constructs without loss of information.

**Implementation:** `src/parser/ast.rs` (planned)

#### Scenario: Preserve column aliases

- GIVEN `SELECT a AS alias`
- WHEN parsed and unparsed
- THEN output MUST include `AS alias`

#### Scenario: Preserve parentheses for precedence

- GIVEN `SELECT (a + b) * c`
- WHEN parsed
- THEN AST MUST represent grouping (not just operator precedence)

### Requirement 4: Error Messages [SHOULD]

Parse errors SHOULD include source location and helpful context.

**Implementation:** `src/parser/error.rs` (planned)

#### Scenario: Error on unexpected token

- GIVEN `SELECT FROM t` (missing columns)
- WHEN parsed
- THEN error SHOULD say "expected column or expression, found FROM at line 1, column 8"

### Requirement 5: Minimal DDL Reader Independence [MUST]

The Tier 0 minimal DDL reader (used to decode `sqlite_master` for the READ CORE) MUST NOT depend on the full parser. It extracts table names, column names, declared types, and WITHOUT ROWID / STRICT markers from DDL text — nothing more.

**Implementation:** `src/schema/ddl_reader.rs` (not under `src/parser/`)

**Tests:** inline `#[cfg(test)]` in `src/schema/ddl_reader.rs`

#### Scenario: Read schema without the parser

- GIVEN a build with the full parser feature-gated off
- WHEN sqlite-rs opens a database and dumps its rows
- THEN schema decoding MUST still work via the minimal DDL reader

Trivially true today — `src/schema/ddl_reader.rs` has zero `use` of any `src/parser/` item, and no `src/parser/` module or Cargo feature flag exists yet to gate. Re-verify with a real feature-gated build once a parser lands; no automated test backs this yet since there is nothing to gate.

#### Scenario: Tolerate unparseable DDL

- GIVEN a `sqlite_master` entry with DDL the minimal reader does not understand (e.g. `CREATE VIRTUAL TABLE ... USING fts5(...)`)
- WHEN the schema is decoded
- THEN the entry MUST degrade to raw-row access with untyped columns, not an error

**Tests:** `src/schema/ddl_reader.rs::fts5_virtual_table_is_graceful_unknown_shadow_tables_are_readable`, `src/schema/ddl_reader.rs::unparseable_non_virtual_ddl_degrades_gracefully_never_errors`

### Requirement 6: lemon-rs Integration [MAY]

If using lemon-rs, the grammar file SHOULD be derived from SQLite's `parse.y` with minimal modifications.

**Implementation:** `src/parser/parse.y` (planned)

#### Scenario: Grammar parity

- GIVEN SQLite's `parse.y`
- WHEN compared to sqlite-rs grammar
- THEN all production rules SHOULD have direct correspondence

## Development Strategy

### Phase 1: Tokenizer (standalone)

1. Implement tokenizer with all SQLite token types
2. Test against SQLite tokenizer output
3. Benchmark performance

### Phase 2: Parser (lemon-rs)

1. Port SQLite `parse.y` to lemon-rs format
2. Generate parser
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
