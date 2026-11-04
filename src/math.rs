//! Typst math -> LaTeX via tylax, with small pre/post fixes for known tylax gaps.

use tylax::{typst_to_latex_with_options, T2LOptions};
use typst_syntax::{parse_math, SyntaxKind, SyntaxNode};

/// Convert the inside of a Typst equation (without `$`) to LaTeX.
///
/// For display math containing top-level `&` or `\` line breaks, the result is
/// wrapped in `aligned` / `gathered` so it renders inside `$$ ... $$`.
pub fn to_latex(typst_math: &str, display: bool) -> String {
    let tree = parse_math(typst_math.trim());
    let (has_align, has_break) = top_level_structure(&tree);
    let prepared = normalize_calls(&tree);
    let options = T2LOptions { math_only: true, block_math_mode: display, ..Default::default() };
    let latex = typst_to_latex_with_options(&prepared, &options);
    let latex = simplify_bold_upright(&fix_linebreaks(latex.trim()));
    if display && (has_align || has_break) {
        let env = if has_align { "aligned" } else { "gathered" };
        format!("\\begin{{{env}}}\n{latex}\n\\end{{{env}}}")
    } else {
        latex
    }
}

fn top_level_structure(node: &SyntaxNode) -> (bool, bool) {
    let mut align = false;
    let mut brk = false;
    for child in node.children() {
        match child.kind() {
            SyntaxKind::MathAlignPoint => align = true,
            SyntaxKind::Linebreak => brk = true,
            // `parse_math` may wrap everything in a single Math node.
            SyntaxKind::Math => {
                let (a, b) = top_level_structure(child);
                align |= a;
                brk |= b;
            }
            _ => {}
        }
    }
    (align, brk)
}

/// Rewrite calls whose defaults tylax gets wrong:
/// - `mat(...)` without `delim:` -> `mat(delim: "(", ...)` (Typst's default is parentheses)
/// - `vec(a, b)` -> `mat(delim: "(", a; b)` (tylax emits a bogus `\vec{a}{b}`)
fn normalize_calls(node: &SyntaxNode) -> String {
    if node.kind() == SyntaxKind::MathCall {
        if let Some(out) = normalize_call(node) {
            return out;
        }
    }
    if node.children().len() == 0 {
        return node.leaf_text().to_string();
    }
    node.children().map(normalize_calls).collect()
}

fn normalize_call(node: &SyntaxNode) -> Option<String> {
    let mut children = node.children();
    let name = children.next()?.leaf_text().as_str();
    if !matches!(name, "mat" | "vec") { return None; }
    let args = children.find(|c| matches!(c.kind(), SyntaxKind::Args | SyntaxKind::MathArgs))?;

    let mut delim = None;
    let mut body = String::new();
    for arg in args.children() {
        match arg.kind() {
            SyntaxKind::LeftParen | SyntaxKind::RightParen => {}
            SyntaxKind::Named if named_key(arg) == "delim" => delim = Some(arg.full_text().to_string()),
            SyntaxKind::Named => return None, // gap, align, ...: leave untouched
            // `vec(a, b)` is a column: rows are separated by `;` in `mat`.
            SyntaxKind::Comma if name == "vec" => body.push(';'),
            _ => body.push_str(&normalize_calls(arg)),
        }
    }
    let delim = delim.unwrap_or_else(|| "delim: \"(\"".to_string());
    let body = body.trim().trim_start_matches([',', ';']).trim();
    Some(format!("mat({delim}, {body})"))
}

fn named_key(named: &SyntaxNode) -> String {
    named.children().next().map(|k| k.leaf_text().to_string()).unwrap_or_default()
}

/// tylax emits a single `\` for Typst math line breaks; LaTeX needs `\\`.
fn fix_linebreaks(latex: &str) -> String {
    let chars: Vec<char> = latex.chars().collect();
    let mut out = String::with_capacity(latex.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            // Count a run of backslashes.
            let start = i;
            while i < chars.len() && chars[i] == '\\' { i += 1; }
            let run = i - start;
            let at_line_end = i >= chars.len() || chars[i] == '\n';
            if run % 2 == 1 && at_line_end {
                out.extend(std::iter::repeat('\\').take(run + 1));
            } else {
                out.extend(std::iter::repeat('\\').take(run));
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// `\mathbf{\mathrm{X}}` -> `\mathbf{X}` (`\mathbf` is already upright).
fn simplify_bold_upright(latex: &str) -> String {
    const PAT: &str = "\\mathbf{\\mathrm{";
    let mut out = String::new();
    let mut rest = latex;
    while let Some(pos) = rest.find(PAT) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + PAT.len()..];
        // `after` starts inside \mathrm{...}; find its closing brace, then the outer one.
        match closing_brace(after) {
            Some(inner_end) if after[inner_end + 1..].starts_with('}') => {
                out.push_str("\\mathbf{");
                out.push_str(&after[..inner_end]);
                out.push('}');
                rest = &after[inner_end + 2..];
            }
            _ => {
                out.push_str(PAT);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Index of the `}` closing a group whose `{` was just before `s`.
fn closing_brace(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped { escaped = false; continue; }
        match c {
            '\\' => escaped = true,
            '{' => depth += 1,
            '}' if depth == 0 => return Some(i),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linebreaks_and_alignment() {
        let out = to_latex("a &= b \\\n &= c", true);
        assert_eq!(out, "\\begin{aligned}\na & = b \\\\\n & = c\n\\end{aligned}");
    }

    #[test]
    fn matrix_rows_untouched() {
        assert!(fix_linebreaks("1 \\\\\n 2").contains("1 \\\\\n 2"));
    }

    #[test]
    fn default_delimiters() {
        assert!(to_latex("mat(1, 2; 3, 4)", true).starts_with("\\begin{pmatrix}"));
        assert!(to_latex("mat(delim: \"[\", 1; 2)", true).starts_with("\\begin{bmatrix}"));
        let v = to_latex("vec(a, b)", true);
        assert!(v.contains("pmatrix") && v.contains("a \\\\"), "{v}");
    }

    #[test]
    fn bold_upright() {
        assert_eq!(simplify_bold_upright("\\mathbf{\\mathrm{\\bar{S}}}^T"), "\\mathbf{\\bar{S}}^T");
        assert_eq!(to_latex("bold(upright(w))_2", false), "\\mathbf{w}_2");
    }
}
