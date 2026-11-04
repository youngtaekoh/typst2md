//! Simple Typst macro (`#let f(x) = ...`) expansion.

use std::collections::HashMap;
use typst_syntax::{parse, parse_code, parse_math, SyntaxKind, SyntaxNode};

#[derive(Debug, Clone)]
pub struct Macro {
    params: Vec<String>,
    body: String,
}

pub type Macros = HashMap<String, Macro>;
const MAX_DEPTH: usize = 32;

pub struct Transformed {
    pub output: String,
    pub count: usize,
    pub macros: Macros,
    pub removed: Vec<String>,
    pub kept: Vec<String>,
}

pub fn transform(source: &str, remove_defs: bool) -> Transformed {
    let mut macros = HashMap::new();
    collect_macros(&parse(source), &mut macros);
    let mut output = source.to_owned();
    let mut count = 0;
    // Code-level expansions may produce new equations or calls, so iterate to a fixed point.
    for _ in 0..MAX_DEPTH {
        let tree = parse(&output);
        let mut changes = Vec::new();
        collect_changes(&tree, 0, None, &macros, &mut changes);
        if changes.is_empty() { break; }
        count += changes.len();
        for (start, end, replacement) in changes.into_iter().rev() {
            output.replace_range(start..end, &replacement);
        }
    }
    let (mut removed, mut kept) = (Vec::new(), Vec::new());
    if remove_defs {
        (output, removed, kept) = remove_definitions(&output, &macros);
    }
    Transformed { output, count, macros, removed, kept }
}

/// Remove `#let name(...) = ...` lines of recognized macros that are no longer referenced.
fn remove_definitions(source: &str, macros: &Macros) -> (String, Vec<String>, Vec<String>) {
    let tree = parse(source);
    let mut used = std::collections::HashSet::new();
    collect_references(&tree, None, 0, macros, &mut used);
    let mut ranges = Vec::new();
    let (mut removed, mut kept) = (Vec::new(), Vec::new());
    find_definitions(&tree, 0, macros, &mut |name, start, end| {
        if used.contains(&name) { kept.push(name); } else { removed.push(name); ranges.push((start, end)); }
    });
    let mut out = source.to_owned();
    for (mut start, mut end) in ranges.into_iter().rev() {
        let line_start = out[..start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = out[end..].find('\n').map_or(out.len(), |i| end + i + 1);
        let whole_line = out[line_start..start].trim().is_empty()
            && out[end..line_end].trim().is_empty();
        if whole_line { start = line_start; end = line_end; }
        out.replace_range(start..end, "");
        // Avoid leaving extra blank lines where the definitions used to be.
        if whole_line && out[..start].ends_with("\n\n") && out[start..].starts_with('\n') {
            out.replace_range(start..start + 1, "");
        }
    }
    (out, removed, kept)
}

fn find_definitions(node: &SyntaxNode, offset: usize, macros: &Macros, found: &mut dyn FnMut(String, usize, usize)) {
    let mut pos = offset;
    let children: Vec<&SyntaxNode> = node.children().collect();
    for (i, child) in children.iter().enumerate() {
        if child.kind() == SyntaxKind::LetBinding {
            if let Some((name, _)) = parse_macro_definition(&child.full_text()) {
                if macros.contains_key(&name) {
                    let hash = i > 0 && children[i - 1].kind() == SyntaxKind::Hash;
                    let start = if hash { pos - children[i - 1].len() } else { pos };
                    found(name, start, pos + child.len());
                }
            }
        } else {
            find_definitions(child, pos, macros, found);
        }
        pos += child.len();
    }
}

/// Collect identifiers that still refer to macros, ignoring the macro definitions themselves.
fn collect_references(node: &SyntaxNode, parent: Option<SyntaxKind>, index: usize, macros: &Macros, used: &mut std::collections::HashSet<String>) {
    match node.kind() {
        SyntaxKind::LetBinding if parse_macro_definition(&node.full_text()).is_some_and(|(n, _)| macros.contains_key(&n)) => return,
        SyntaxKind::Ident | SyntaxKind::MathIdent if is_variable_position(parent, index) => {
            let name = node.leaf_text().to_string();
            if macros.contains_key(&name) { used.insert(name); }
        }
        _ => {}
    }
    for (i, child) in node.children().enumerate() {
        collect_references(child, Some(node.kind()), i, macros, used);
    }
}

/// An identifier is a variable reference unless it is a field name or a named-argument key.
fn is_variable_position(parent: Option<SyntaxKind>, index: usize) -> bool {
    !matches!((parent, index), (Some(SyntaxKind::FieldAccess), i) if i > 0)
        && !matches!((parent, index), (Some(SyntaxKind::Named), 0))
}

fn collect_macros(node: &SyntaxNode, macros: &mut Macros) {
    if node.kind() == SyntaxKind::LetBinding {
        if let Some((name, definition)) = parse_macro_definition(&node.full_text()) {
            macros.insert(name, definition);
        }
    }
    for child in node.children() {
        collect_macros(child, macros);
    }
}

fn collect_changes(node: &SyntaxNode, offset: usize, parent: Option<SyntaxKind>, macros: &Macros, changes: &mut Vec<(usize, usize, String)>) {
    // Never rewrite the macro definitions themselves.
    if node.kind() == SyntaxKind::LetBinding { return; }
    if node.kind() == SyntaxKind::FuncCall {
        if let Some(mut replacement) = expand_code_call(node, macros) {
            if parent == Some(SyntaxKind::Markup) && !is_markup_safe(&replacement) {
                replacement = format!("({replacement})");
            }
            changes.push((offset, offset + node.len(), replacement));
            return;
        }
    }
    if node.kind() == SyntaxKind::Equation {
        let before = node.full_text().to_string();
        if before.starts_with('$') && before.ends_with('$') && before.len() >= 2 {
            let inner = &before[1..before.len() - 1];
            let after = expand_math(inner, macros, 0);
            if after != inner {
                changes.push((offset, offset + node.len(), format!("${after}$")));
            }
        }
        return;
    }
    let mut pos = offset;
    for child in node.children() {
        collect_changes(child, pos, Some(node.kind()), macros, changes);
        pos += child.len();
    }
}

/// Expand a code-mode call such as `#boxed[...]` or `boxed(x)` of a known macro.
fn expand_code_call(node: &SyntaxNode, macros: &Macros) -> Option<String> {
    let mut children = node.children();
    let callee = children.next()?;
    if callee.kind() != SyntaxKind::Ident { return None; }
    let def = macros.get(callee.leaf_text().as_str())?;
    let args_node = children.find(|c| c.kind() == SyntaxKind::Args)?;
    let mut args = Vec::new();
    for arg in args_node.children() {
        match arg.kind() {
            SyntaxKind::LeftParen | SyntaxKind::RightParen | SyntaxKind::Comma
            | SyntaxKind::Space | SyntaxKind::LineComment | SyntaxKind::BlockComment => {}
            SyntaxKind::Named | SyntaxKind::Spread => return None,
            _ => args.push(arg.full_text().to_string()),
        }
    }
    if args.len() != def.params.len() { return None; }
    Some(substitute_code(&def.body, &def.params, &args))
}

/// Substitute parameters in a code-mode body via the AST, so markup text, field
/// names, and named-argument keys are never touched.
fn substitute_code(body: &str, params: &[String], args: &[String]) -> String {
    fn walk(node: &SyntaxNode, parent: Option<SyntaxKind>, index: usize, params: &[String], args: &[String], out: &mut String) {
        if matches!(node.kind(), SyntaxKind::Ident | SyntaxKind::MathIdent) && is_variable_position(parent, index) {
            if let Some(i) = params.iter().position(|p| p == node.leaf_text().as_str()) {
                let arg = args[i].trim();
                if node.kind() == SyntaxKind::Ident && !is_atomic(arg) {
                    out.push_str(&format!("({arg})"));
                } else {
                    out.push_str(arg);
                }
                return;
            }
        }
        if node.children().len() == 0 {
            out.push_str(node.leaf_text());
            return;
        }
        for (i, child) in node.children().enumerate() {
            walk(child, Some(node.kind()), i, params, args, out);
        }
    }
    let mut out = String::new();
    walk(&parse_code(body), None, 0, params, args, &mut out);
    out
}

/// Whether a code expression can be substituted without parentheses.
fn is_atomic(expr: &str) -> bool {
    let tree = parse_code(expr);
    let mut exprs = tree.children().filter(|c| !matches!(c.kind(), SyntaxKind::Space));
    match (exprs.next(), exprs.next()) {
        (Some(e), None) => e.errors_and_warnings().0.is_empty() && matches!(e.kind(),
            SyntaxKind::ContentBlock | SyntaxKind::CodeBlock | SyntaxKind::Ident | SyntaxKind::FieldAccess
            | SyntaxKind::FuncCall | SyntaxKind::Parenthesized | SyntaxKind::Array | SyntaxKind::Dict
            | SyntaxKind::Str | SyntaxKind::Int | SyntaxKind::Float | SyntaxKind::Numeric
            | SyntaxKind::Bool | SyntaxKind::None | SyntaxKind::Auto | SyntaxKind::Equation),
        _ => false,
    }
}

/// Whether `#<expr>` in markup parses as the whole expression.
fn is_markup_safe(expr: &str) -> bool { is_atomic(expr) }

// Expand only math AST calls. No transformations in strings, comments, code, or prose.
fn expand_math(src: &str, macros: &Macros, depth: usize) -> String {
    if depth >= MAX_DEPTH { return src.to_owned(); }
    let tree = parse_math(src);
    rewrite_math_node(&tree, macros, depth)
}

fn rewrite_math_node(node: &SyntaxNode, macros: &Macros, depth: usize) -> String {
    if node.kind() == SyntaxKind::MathCall {
        if let Some(output) = expand_call(&node.full_text(), macros, depth) {
            return output;
        }
    }
    let mut result = String::new();
    let children: Vec<&SyntaxNode> = node.children().collect();
    if children.is_empty() { return node.full_text().to_string(); }
    for child in children {
        result.push_str(&rewrite_math_node(child, macros, depth));
    }
    result
}

fn expand_call(call: &str, macros: &Macros, depth: usize) -> Option<String> {
    let open = call.find('(')?;
    let name = call[..open].trim();
    if !valid_path(name) { return None; }
    let end = matching_paren(call, open)?;
    if !call[end + 1..].trim().is_empty() { return None; }
    let raw_args = split_top_level(&call[open + 1..end], ',')?;
    let args: Vec<String> = raw_args.iter().map(|v| expand_math(v.trim(), macros, depth + 1)).collect();

    if let Some(def) = macros.get(name) {
        if def.params.len() != args.len() { return None; }
        // Substitute identifiers only; never replace substrings of identifiers.
        let mut body = substitute(&def.body, &def.params, &args);
        body = strip_math_prefixes(&body);
        return Some(expand_math(&body, macros, depth + 1));
    }
    // Tylax recognizes bold(), not math.bold(). Only normalize explicitly
    // supported built-in mathematical formatting functions.
    if matches!(name, "math.bold" | "math.upright" | "math.italic" | "math.serif" | "math.sans" | "math.mono") {
        return Some(format!("{}({})", &name[5..], args.join(", ")));
    }
    None
}

fn parse_macro_definition(text: &str) -> Option<(String, Macro)> {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix("let")?.trim_start();
    let open = rest.find('(')?;
    let name = rest[..open].trim();
    if !valid_identifier(name) { return None; }
    let close = matching_paren(rest, open)?;
    let after = rest[close + 1..].trim_start();
    let body = after.strip_prefix('=')?.trim();
    if body.is_empty() || body.starts_with('{') || body.starts_with('[') { return None; }
    let params_raw = &rest[open + 1..close];
    let params = split_top_level(params_raw, ',')?;
    if params.len() == 1 && params[0].trim().is_empty() {
        return Some((name.to_string(), Macro {params: vec![], body: body.into()}));
    }
    let params: Vec<String> = params.iter().map(|p| p.trim().to_string()).collect();
    if !params.iter().all(|p| valid_identifier(p)) { return None; }
    if params.iter().collect::<std::collections::HashSet<_>>().len() != params.len() { return None; }
    Some((name.to_string(), Macro { params, body: body.into() }))
}

fn valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}
fn valid_path(s: &str) -> bool { s.split('.').all(valid_identifier) }

fn matching_paren(s: &str, open: usize) -> Option<usize> {
    if !s.is_char_boundary(open) || s.as_bytes().get(open) != Some(&b'(') { return None; }
    let mut depth = 0usize;
    let mut quote = false;
    let mut escape = false;
    for (i, c) in s.char_indices().filter(|(i, _)| *i >= open) {
        if quote {
            if escape { escape = false; }
            else if c == '\\' { escape = true; }
            else if c == '"' { quote = false; }
        } else if c == '"' { quote = true; }
        else if c == '(' { depth += 1; }
        else if c == ')' {
            depth -= 1;
            if depth == 0 { return Some(i); }
        }
    }
    None
}

fn split_top_level(s: &str, separator: char) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut stack = Vec::new();
    let mut quote = false;
    let mut escape = false;
    for (i, c) in s.char_indices() {
        if quote {
            if escape { escape = false; }
            else if c == '\\' { escape = true; }
            else if c == '"' { quote = false; }
            continue;
        }
        match c {
            '"' => quote = true,
            '(' | '[' | '{' => stack.push(c),
            ')' | ']' | '}' => {
                let expected = match c { ')' => '(', ']' => '[', _ => '{' };
                if stack.pop() != Some(expected) { return None; }
            }
            _ if c == separator && stack.is_empty() => {
                parts.push(s[start..i].to_string());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    if quote || !stack.is_empty() { return None; }
    parts.push(s[start..].to_string());
    Some(parts)
}

fn substitute(body: &str, params: &[String], args: &[String]) -> String {
    // Scan identifiers without touching quoted strings or longer identifiers.
    let mut out = String::new();
    let mut chars = body.char_indices().peekable();
    let mut quoted = false;
    let mut escaped = false;
    while let Some((start, ch)) = chars.next() {
        if quoted {
            out.push(ch);
            if escaped { escaped = false; }
            else if ch == '\\' { escaped = true; }
            else if ch == '"' { quoted = false; }
            continue;
        }
        if ch == '"' { quoted = true; out.push(ch); continue; }
        if ch.is_alphabetic() || ch == '_' {
            let mut end = start + ch.len_utf8();
            while let Some(&(_, next)) = chars.peek() {
                if next.is_alphanumeric() || next == '_' || next == '-' {
                    let (i, c) = chars.next().unwrap();
                    end = i + c.len_utf8();
                } else { break; }
            }
            let ident = &body[start..end];
            let previous_is_dot = start > 0 && body[..start].ends_with('.');
            if !previous_is_dot {
                if let Some(index) = params.iter().position(|param| param == ident) {
                    out.push_str(&args[index]);
                    continue;
                }
            }
            out.push_str(ident);
        } else { out.push(ch); }
    }
    out
}

fn strip_math_prefixes(body: &str) -> String {
    let mut output = body.to_owned();
    for name in ["bold", "upright", "italic", "serif", "sans", "mono"] {
        output = output.replace(&format!("math.{name}("), &format!("{name}("));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_and_substitution() {
        let (name, def) = parse_macro_definition("let bvec(x) = math.bold(math.upright(x))").unwrap();
        assert_eq!(name, "bvec");
        assert_eq!(substitute(&def.body, &def.params, &["z".into()]), "math.bold(math.upright(z))");
    }

    #[test]
    fn nested_macros() {
        let src = "#let bmat(x) = math.bold(math.upright(x))\n#let bvec(x) = bmat(x)\n$ bmat(A) bvec(y) $";
        let r = transform(src, false);
        assert_eq!(r.count, 1);
        assert!(r.output.contains("$ bold(upright(A)) bold(upright(y)) $"), "{}", r.output);
    }

    #[test]
    fn preserve_non_math_and_rewrite_builtins() {
        let src = "Some math.bold(x) text.\n$ f = f_0 + math.bold(D) bold(w) $";
        let r = transform(src, true);
        let out = r.output;
        assert_eq!(r.count, 1);
        assert!(out.contains("Some math.bold(x) text."));
        assert!(out.contains("$ f = f_0 + bold(D) bold(w) $"));
    }

    #[test]
    fn split_nested_arguments() {
        assert_eq!(split_top_level("x, mat(a, b), vec(y, z)", ',').unwrap().len(), 3);
    }

    #[test]
    fn markup_macro_and_definition_removal() {
        let src = "= T\n\n#let bmat(x) = math.bold(math.upright(x))\n#let boxed(x) = align(center)[#box(stroke: 0.5pt, x)]\n\nText $bmat(A)$.\n\n#boxed[\n  $ bmat(B) $\n]\n";
        let r = transform(src, true);
        assert_eq!(r.output, "= T\n\nText $bold(upright(A))$.\n\n#align(center)[#box(stroke: 0.5pt, [\n  $ bold(upright(B)) $\n])]\n");
        assert_eq!(r.removed.len(), 2);
    }

    #[test]
    fn keep_definition_still_in_use() {
        // Arity mismatch cannot be expanded, so the definition must stay.
        let src = "#let foo(x) = x\n$ foo(a, b) $\n";
        let r = transform(src, true);
        assert!(r.output.contains("#let foo(x) = x"));
        assert_eq!(r.kept, vec!["foo".to_string()]);
    }

    #[test]
    fn code_substitution_skips_text_and_keys() {
        let out = substitute_code("box(x: 1)[x #x]", &["x".into()], &["a + b".into()]);
        assert_eq!(out, "box(x: 1)[x #(a + b)]");
    }
}
