# typst2md

Convert a Typst document to Markdown, with math converted to LaTeX by [Tylax](https://github.com/scipenai/tylax).

Pipeline: **macro expansion → equation extraction → Tylax (Typst math → LaTeX) → Markdown**.

## Build

```bash
cargo build --release
cargo test
```

## Use

```bash
# Typst -> Markdown
./target/release/typst2md r.typ out.md

# Macro expansion only (Typst output); --keep-defs keeps the #let definitions
./target/release/typst2md [--keep-defs] r.typ expanded.typ
```

The output format is chosen by the output file extension (`.md` / `.typ`).

`--eq-number <qquad|tag|none>` sets how equation numbers are written (default `qquad`):

| value | equation number | references (`@eq:x`, `@sec`) | renderer |
|---|---|---|---|
| `qquad` | `\qquad \text{(1)}` right after the equation | plain text `(1)` | any LaTeX math renderer |
| `tag` | `\tag*{(1)}` right-aligned | `<a id>` anchors + `[(1)](#eq-x)` links | KaTeX, MathJax |
| `none` | none | plain text `(1)` | — |

## Macro expansion

Recognizes simple `#let name(x, ...) = expression` definitions and expands their calls both inside math (`$ bvec(w) $`) and in code/markup (`#boxed[...]`), including nested macros. `math.bold`, `math.upright`, … are normalized to `bold`, `upright`, … in math. Definitions of expanded macros are removed; a definition that is still referenced is kept with a warning.

Not supported: imported definitions, reassignment, scoping, default/named/spread parameters, or arbitrary Typst evaluation. This is not a Typst interpreter.

## Markdown conversion

| Typst | Markdown |
|---|---|
| `$x$` | `$x$` |
| `$ x $` | `$$ … $$` (`&` / `\` lines → `aligned` / `gathered`) |
| `#math.equation(numbering: "(1)", ..) <eq:a>` or `#set math.equation(numbering: ..)` | `$$ … \qquad \text{(1)} $$` (see `--eq-number`) |
| `@eq:a`, `@eq:a[Eq.]` | `(1)`, `Eq. (1)` (links with `--eq-number tag`) |
| `= Title <sec>` / `@sec` | `# Title` / `Title` (anchor + link with `--eq-number tag`) |
| `#box(stroke: .., $ x $)` | `$$ \boxed{x} $$` |
| `*strong*`, `_emph_`, `` `raw` ``, lists, enums, terms, links | Markdown equivalents |
| `#align`, `#box`, `#block`, … | content only |
| `#set`, `#show`, `#import`, … | dropped (warning) |

Fixes applied around Tylax: math line breaks (`\` → `\\`), `mat(..)` default `(` delimiter, `vec(..)` → column `pmatrix`, and `\mathbf{\mathrm{X}}` → `\mathbf{X}`.

All generated math (`aligned`, `gathered`, `pmatrix`, `\boxed`, `\tag*`) is KaTeX-supported; use `--eq-number tag` for KaTeX renderers such as the VS Code built-in Markdown preview.

Built with Claude Code and Codex.