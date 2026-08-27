# Component View

Zooming into the Tier 2 (Query) container from the [container view](c4-container.md):

```mermaid
C4Component
    title sqlite-rs Query Tier Components

    Component(tokenizer, "Tokenizer", "SQL string → tokens")
    Component(parser, "Parser", "Tokens → AST (grammar in .openspec/grammar/sqlite.ebnf)")
    Component(planner, "Planner", "AST → logical Plan")
    Component(codegen, "Codegen", "Plan → VDBE bytecode")

    Rel(tokenizer, parser, "feeds")
    Rel(parser, planner, "feeds")
    Rel(planner, codegen, "feeds")
```

Module-level detail for each component lives with its spec under
`.openspec/specs/002-parser` and `.openspec/specs/009-vdbe-codegen`.
