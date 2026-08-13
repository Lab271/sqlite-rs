// Lemon grammar for the sqlite-rs parser spike (issue #1, variant 001).
//
// Implements exactly the subset in tests/spike/001_parser/grammar/sqlite-subset.ebnf:
// CREATE TABLE / INSERT / SELECT / UPDATE / DELETE plus the shared expression
// grammar. Processed at build time by third_party/lemon/lemon.c using the Rust
// driver template third_party/lemon/lempar.rs.
//
// NOTE ON CODE BLOCKS: lemon scans { ... } blocks as C code, so a single quote
// starts a character literal. Rust lifetimes must therefore appear in pairs
// (see Tok2 in src/tokenizer.rs) and no apostrophes may appear in any block,
// comments included.

%include {
use crate::ast::*;
use crate::tokenizer::Tok2;
use crate::parser::{Context, ParseError};
}

%name Parse
%token_type {Tok2<'i, 'i>}
%extra_context {ctx: Context}

%syntax_error {
    let pos = yyminor.pos;
    let text = yyminor.text;
    if self.ctx.error.is_none() {
        self.ctx.error = Some(if text.is_empty() {
            format!("syntax error: unexpected end of input at offset {pos}")
        } else {
            format!("syntax error near \"{text}\" at offset {pos}")
        });
    }
}

// ---------------------------------------------------------------------------
// Operator precedence, lowest to highest, mirroring the EBNF ladder (which in
// turn mirrors sqlite parse.y:295-309). Note CONCAT binds tighter than STAR and
// unary +/- binds tightest of all, exactly as in real SQLite.
// ---------------------------------------------------------------------------
%left OR.
%left AND.
%right NOT.
%left EQ NE LT LE GT GE.
%left PLUS MINUS.
%left STAR SLASH REM.
%left CONCAT.
%right UNARY.

// ===================== Top level =====================

program ::= stmt(S). { self.ctx.stmt = Some(S); }

%type stmt {Stmt}

// ===================== CREATE TABLE =====================

stmt(A) ::= CREATE TABLE ifnotexists(E) nm(N) LP columnlist(C) RP. {
    A = Stmt::CreateTable { if_not_exists: E, name: N, columns: C };
}

%type ifnotexists {bool}
ifnotexists(A) ::= . { A = false; }
ifnotexists(A) ::= IF NOT EXISTS. { A = true; }

%type columnlist {Vec<ColumnDef>}
columnlist(A) ::= columndef(C). { A = vec![C]; }
columnlist(A) ::= columnlist(B) COMMA columndef(C). { let mut v = B; v.push(C); A = v; }

%type columndef {ColumnDef}
// NOTE: every reference to an RHS label expands to a *move* out of the parser
// stack, so a label may only be mentioned once. Hence the let-binding for G.
columndef(A) ::= nm(N) typename_opt(T) carglist(G). {
    let g = G;
    A = ColumnDef { name: N, type_name: T, not_null: g.0, primary_key: g.1 };
}

%type typename_opt {Option<String>}
typename_opt(A) ::= . { A = None; }
typename_opt(A) ::= typename(T). { A = Some(T); }

%type typename {String}
typename(A) ::= ID(X). { A = X.text.to_string(); }
typename(A) ::= typename(B) ID(X). { let mut s = B; s.push_str(" "); s.push_str(X.text); A = s; }

// column constraints, collected as (not_null, primary_key)
%type carglist {(bool, bool)}
carglist(A) ::= . { A = (false, false); }
carglist(A) ::= carglist(B) NOT NULL. { let mut v = B; v.0 = true; A = v; }
carglist(A) ::= carglist(B) PRIMARY KEY. { let mut v = B; v.1 = true; A = v; }

// ===================== INSERT =====================

stmt(A) ::= INSERT INTO nm(N) idlist_opt(I) VALUES rowlist(R). {
    A = Stmt::Insert { table: N, columns: I, rows: R };
}

%type idlist_opt {Vec<String>}
idlist_opt(A) ::= . { A = Vec::new(); }
idlist_opt(A) ::= LP idlist(L) RP. { A = L; }

%type idlist {Vec<String>}
idlist(A) ::= nm(N). { A = vec![N]; }
idlist(A) ::= idlist(B) COMMA nm(N). { let mut v = B; v.push(N); A = v; }

%type rowlist {Vec<Vec<Expr>>}
rowlist(A) ::= LP exprlist(L) RP. { A = vec![L]; }
rowlist(A) ::= rowlist(B) COMMA LP exprlist(L) RP. { let mut v = B; v.push(L); A = v; }

// ===================== SELECT =====================

stmt(A) ::= select(S). { A = Stmt::Select(S); }

%type select {Select}
select(A) ::= SELECT distinct(D) selcollist(C) from_opt(F) where_opt(W)
              groupby_opt(G) orderby_opt(O) limit_opt(L). {
    let g = G;
    A = Select { distinct: D, columns: C, from: F, where_clause: W,
                 group_by: g.0, having: g.1, order_by: O, limit: L };
}

%type distinct {Option<bool>}
distinct(A) ::= . { A = None; }
distinct(A) ::= DISTINCT. { A = Some(true); }
distinct(A) ::= ALL. { A = Some(false); }

%type selcollist {Vec<ResultColumn>}
selcollist(A) ::= selcol(C). { A = vec![C]; }
selcollist(A) ::= selcollist(B) COMMA selcol(C). { let mut v = B; v.push(C); A = v; }

%type selcol {ResultColumn}
selcol(A) ::= STAR. { A = ResultColumn::Star; }
selcol(A) ::= expr(X). { A = ResultColumn::Expr(X, None); }
selcol(A) ::= expr(X) AS nm(N). { A = ResultColumn::Expr(X, Some(N)); }
selcol(A) ::= expr(X) nm(N). { A = ResultColumn::Expr(X, Some(N)); }

%type from_opt {Option<String>}
from_opt(A) ::= . { A = None; }
from_opt(A) ::= FROM nm(N). { A = Some(N); }

%type where_opt {Option<Expr>}
where_opt(A) ::= . { A = None; }
where_opt(A) ::= WHERE expr(X). { A = Some(X); }

%type groupby_opt {(Vec<Expr>, Option<Expr>)}
groupby_opt(A) ::= . { A = (Vec::new(), None); }
groupby_opt(A) ::= GROUP BY exprlist(L) having_opt(H). { A = (L, H); }

%type having_opt {Option<Expr>}
having_opt(A) ::= . { A = None; }
having_opt(A) ::= HAVING expr(X). { A = Some(X); }

%type orderby_opt {Vec<(Expr, bool)>}
orderby_opt(A) ::= . { A = Vec::new(); }
orderby_opt(A) ::= ORDER BY sortlist(L). { A = L; }

%type sortlist {Vec<(Expr, bool)>}
sortlist(A) ::= sortitem(I). { A = vec![I]; }
sortlist(A) ::= sortlist(B) COMMA sortitem(I). { let mut v = B; v.push(I); A = v; }

%type sortitem {(Expr, bool)}
sortitem(A) ::= expr(X) sortorder(D). { A = (X, D); }

%type sortorder {bool}
sortorder(A) ::= . { A = false; }
sortorder(A) ::= ASC. { A = false; }
sortorder(A) ::= DESC. { A = true; }

%type limit_opt {Option<(Expr, Option<Expr>)>}
limit_opt(A) ::= . { A = None; }
limit_opt(A) ::= LIMIT expr(X). { A = Some((X, None)); }
limit_opt(A) ::= LIMIT expr(X) OFFSET expr(Y). { A = Some((X, Some(Y))); }
limit_opt(A) ::= LIMIT expr(X) COMMA expr(Y). { A = Some((X, Some(Y))); }

// ===================== UPDATE =====================

stmt(A) ::= UPDATE nm(N) SET setlist(L) where_opt(W). {
    A = Stmt::Update { table: N, sets: L, where_clause: W };
}

%type setlist {Vec<(String, Expr)>}
setlist(A) ::= nm(N) EQ expr(X). { A = vec![(N, X)]; }
setlist(A) ::= setlist(B) COMMA nm(N) EQ expr(X). { let mut v = B; v.push((N, X)); A = v; }

// ===================== DELETE =====================

stmt(A) ::= DELETE FROM nm(N) where_opt(W). {
    A = Stmt::Delete { table: N, where_clause: W };
}

// ===================== Expressions =====================

%type exprlist {Vec<Expr>}
exprlist(A) ::= expr(X). { A = vec![X]; }
exprlist(A) ::= exprlist(B) COMMA expr(X). { let mut v = B; v.push(X); A = v; }

%type exprlist_opt {Vec<Expr>}
exprlist_opt(A) ::= . { A = Vec::new(); }
exprlist_opt(A) ::= exprlist(L). { A = L; }

%type expr {Expr}

expr(A) ::= ID(X). { A = Expr::Column { table: None, name: X.text.to_string() }; }
expr(A) ::= ID(T) DOT ID(X). {
    A = Expr::Column { table: Some(T.text.to_string()), name: X.text.to_string() };
}
expr(A) ::= INTEGER(X). { A = Expr::Lit(Lit::Int(X.text.parse().unwrap_or_default())); }
expr(A) ::= FLOAT(X). { A = Expr::Lit(Lit::Float(X.text.parse().unwrap_or_default())); }
expr(A) ::= STRING(X). { A = Expr::Lit(Lit::Str(unquote(X.text))); }
expr(A) ::= NULL. { A = Expr::Lit(Lit::Null); }
expr(A) ::= LP expr(X) RP. { A = Expr::Paren(Box::new(X)); }

expr(A) ::= ID(F) LP distinct_kw(D) exprlist_opt(L) RP. {
    A = Expr::Func { name: F.text.to_string(), distinct: D, args: L };
}

%type distinct_kw {bool}
distinct_kw(A) ::= . { A = false; }
distinct_kw(A) ::= DISTINCT. { A = true; }

expr(A) ::= expr(X) OR expr(Y). { A = Expr::Binary { op: BinOp::Or, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) AND expr(Y). { A = Expr::Binary { op: BinOp::And, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) EQ expr(Y). { A = Expr::Binary { op: BinOp::Eq, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) NE expr(Y). { A = Expr::Binary { op: BinOp::Ne, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) LT expr(Y). { A = Expr::Binary { op: BinOp::Lt, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) LE expr(Y). { A = Expr::Binary { op: BinOp::Le, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) GT expr(Y). { A = Expr::Binary { op: BinOp::Gt, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) GE expr(Y). { A = Expr::Binary { op: BinOp::Ge, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) PLUS expr(Y). { A = Expr::Binary { op: BinOp::Add, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) MINUS expr(Y). { A = Expr::Binary { op: BinOp::Sub, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) STAR expr(Y). { A = Expr::Binary { op: BinOp::Mul, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) SLASH expr(Y). { A = Expr::Binary { op: BinOp::Div, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) REM expr(Y). { A = Expr::Binary { op: BinOp::Rem, lhs: Box::new(X), rhs: Box::new(Y) }; }
expr(A) ::= expr(X) CONCAT expr(Y). { A = Expr::Binary { op: BinOp::Concat, lhs: Box::new(X), rhs: Box::new(Y) }; }

expr(A) ::= NOT expr(X). { A = Expr::Unary { op: UnOp::Not, expr: Box::new(X) }; }
expr(A) ::= MINUS expr(X). [UNARY] { A = Expr::Unary { op: UnOp::Neg, expr: Box::new(X) }; }
expr(A) ::= PLUS expr(X). [UNARY] { A = Expr::Unary { op: UnOp::Pos, expr: Box::new(X) }; }

// ===================== Lexical =====================

%type nm {String}
nm(A) ::= ID(X). { A = X.text.to_string(); }
