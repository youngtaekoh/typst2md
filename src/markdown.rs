//! Typst markup -> Markdown. Equations go through tylax (see `math.rs`);
//! labels and `@references` become HTML anchors and Markdown links.

use std::collections::HashMap;
use typst_syntax::{parse, SyntaxKind, SyntaxNode};

use crate::math;

/// What a label points to.
#[derive(Debug, Clone)]
enum Target {
    Equation { number: String },
    Heading { title: String },
}

#[derive(Default)]
struct Labels {
    targets: HashMap<String, Target>,
    /// Equation offset -> (tag, label)
    equations: HashMap<usize, (String, Option<String>)>,
    /// Heading offset -> label
    headings: HashMap<usize, String>,
}

pub struct Converted {
    pub markdown: String,
    pub warnings: Vec<String>,
}

/// How equation numbers are written into display math.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EqNumber {
    /// `\tag*{(1)}`: right-aligned number (KaTeX, MathJax). Also emits
    /// `<a id>` anchors so references become clickable links.
    Tag,
    /// `\qquad \text{(1)}` after the equation: works in any LaTeX math renderer.
    #[default]
    Qquad,
    /// No number in the math.
    None,
}

impl EqNumber {
    /// HTML anchors + `[(1)](#id)` links are only emitted in KaTeX (`tag`) mode;
    /// otherwise references are plain text.
    fn anchors(self) -> bool { self == Self::Tag }
}

impl std::str::FromStr for EqNumber {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "tag" => Ok(Self::Tag),
            "qquad" => Ok(Self::Qquad),
            "none" => Ok(Self::None),
            other => Err(format!("invalid --eq-number `{other}` (expected tag, qquad, or none)")),
        }
    }
}

pub fn convert(source: &str, eq_number: EqNumber) -> Converted {
    let tree = parse(source);
    let mut labels = Labels::default();
    let mut numbering = None;
    let mut counter = 0;
    collect_labels(&tree, 0, &mut labels, &mut numbering, &mut counter);
    let mut r = Renderer { labels, warnings: Vec::new(), indent: String::new(), eq_number };
    let body = r.markup(&tree, 0);
    Converted { markdown: tidy(&body), warnings: r.warnings }
}

// ---------------------------------------------------------------------------
// Pass 1: equation numbers and labels

/// An equation found in markup, possibly wrapped in `#math.equation(...)`.
struct EqSite<'a> {
    equation: &'a SyntaxNode,
    /// Absolute offset of the `Equation` node.
    offset: usize,
    /// Numbering pattern from `math.equation(numbering: ..)`, if given there.
    numbering: Option<Option<String>>,
}

/// If `node` (at `offset`) is an equation or `math.equation(...)` call, describe it.
fn equation_site(node: &SyntaxNode, offset: usize) -> Option<EqSite<'_>> {
    match node.kind() {
        SyntaxKind::Equation => Some(EqSite { equation: node, offset, numbering: None }),
        SyntaxKind::FuncCall if callee_name(node).as_deref() == Some("math.equation") => {
            let args = node.children().find(|c| c.kind() == SyntaxKind::Args)?;
            let mut pos = offset + node.len() - args.len();
            let mut site = None;
            let mut numbering = None;
            for arg in args.children() {
                match arg.kind() {
                    SyntaxKind::Equation => site = Some((arg, pos)),
                    SyntaxKind::Named if named_key(arg) == "numbering" => numbering = Some(numbering_value(arg)),
                    _ => {}
                }
                pos += arg.len();
            }
            let (equation, offset) = site?;
            Some(EqSite { equation, offset, numbering })
        }
        _ => None,
    }
}

fn collect_labels(node: &SyntaxNode, offset: usize, labels: &mut Labels, numbering: &mut Option<String>, counter: &mut usize) {
    let children: Vec<&SyntaxNode> = node.children().collect();
    let mut pos = offset;
    for (i, child) in children.iter().enumerate() {
        if child.kind() == SyntaxKind::SetRule && callee_name(child).as_deref() == Some("math.equation") {
            if let Some(args) = child.children().find(|c| c.kind() == SyntaxKind::Args) {
                for arg in args.children().filter(|a| a.kind() == SyntaxKind::Named && named_key(a) == "numbering") {
                    *numbering = numbering_value(arg);
                }
            }
        }
        if node.kind() == SyntaxKind::Markup {
            let label = following_label(&children, i);
            if let Some(site) = equation_site(child, pos) {
                let pattern = site.numbering.clone().unwrap_or_else(|| numbering.clone());
                if is_display(site.equation) {
                    if let Some(pattern) = pattern {
                        *counter += 1;
                        let number = apply_numbering(&pattern, *counter);
                        if let Some(l) = &label {
                            labels.targets.insert(l.clone(), Target::Equation { number: number.clone() });
                        }
                        labels.equations.insert(site.offset, (number, label.clone()));
                    }
                }
            }
            if child.kind() == SyntaxKind::Heading {
                if let Some(l) = label {
                    let title = plain_text(child.children().find(|c| c.kind() == SyntaxKind::Markup).unwrap_or(child));
                    labels.targets.insert(l.clone(), Target::Heading { title: title.trim().to_string() });
                    labels.headings.insert(pos, l);
                }
            }
        }
        collect_labels(child, pos, labels, numbering, counter);
        pos += child.len();
    }
}

/// Label attached to `children[i]`: `<label>` after optional non-paragraph spaces.
fn following_label(children: &[&SyntaxNode], i: usize) -> Option<String> {
    for next in &children[i + 1..] {
        match next.kind() {
            SyntaxKind::Space if !next.leaf_text().contains("\n\n") => continue,
            SyntaxKind::Label => return Some(label_name(next)),
            _ => return None,
        }
    }
    None
}

fn label_name(node: &SyntaxNode) -> String {
    node.leaf_text().trim_start_matches('<').trim_end_matches('>').to_string()
}

fn numbering_value(named: &SyntaxNode) -> Option<String> {
    let value = named.children().last()?;
    match value.kind() {
        SyntaxKind::Str => Some(value.leaf_text().trim_matches('"').to_string()),
        _ => None, // `none`, functions, ...
    }
}

/// `"(1)"` + 3 -> `"(3)"`. Only the first counter symbol is honored.
fn apply_numbering(pattern: &str, n: usize) -> String {
    match pattern.find(|c: char| matches!(c, '1' | 'a' | 'A' | 'i' | 'I')) {
        Some(i) => format!("{}{}{}", &pattern[..i], n, &pattern[i + 1..]),
        None => n.to_string(),
    }
}

fn anchor_id(label: &str) -> String {
    label.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}

// ---------------------------------------------------------------------------
// Pass 2: rendering

struct Renderer {
    labels: Labels,
    warnings: Vec<String>,
    /// Indentation for continuation lines inside list items.
    indent: String,
    eq_number: EqNumber,
}

impl Renderer {
    fn warn(&mut self, msg: String) {
        if !self.warnings.contains(&msg) { self.warnings.push(msg); }
    }

    /// Render a Markup node (or any node containing markup children).
    fn markup(&mut self, node: &SyntaxNode, offset: usize) -> String {
        let children: Vec<&SyntaxNode> = node.children().collect();
        let mut out = String::new();
        let mut pos = offset;
        for child in children.iter() {
            out.push_str(&self.node(child, pos));
            pos += child.len();
        }
        out
    }

    fn node(&mut self, node: &SyntaxNode, offset: usize) -> String {
        use SyntaxKind as K;
        match node.kind() {
            K::Markup => self.markup(node, offset),
            K::Text => node.leaf_text().to_string(),
            K::Space => {
                if node.leaf_text().contains('\n') { format!("\n{}", self.indent) } else { " ".into() }
            }
            K::Parbreak => "\n\n".into(),
            K::Linebreak => "\\\n".into(),
            K::Escape => escape(node.leaf_text()),
            K::Shorthand => shorthand(node.leaf_text()).into(),
            K::SmartQuote => node.leaf_text().to_string(),
            K::Strong => format!("**{}**", self.inner_markup(node, offset).trim()),
            K::Emph => format!("*{}*", self.inner_markup(node, offset).trim()),
            K::Raw => node.full_text().to_string(),
            K::Link => format!("<{}>", node.leaf_text()),
            K::Label => String::new(), // attached labels are emitted as anchors
            K::Ref => self.reference(node, offset),
            K::Heading => self.heading(node, offset),
            K::ListItem => self.list_item(node, offset, "- ".into()),
            K::EnumItem => {
                let marker = node.children().next().map(|m| m.leaf_text().to_string()).unwrap_or_default();
                let marker = if marker == "+" { "1. ".to_string() } else { format!("{marker} ") };
                self.list_item(node, offset, marker)
            }
            K::TermItem => {
                let parts: Vec<(usize, &SyntaxNode)> = with_offsets(node, offset).into_iter()
                    .filter(|(_, c)| c.kind() == K::Markup).collect();
                let term = parts.first().map(|(p, m)| self.markup(m, *p)).unwrap_or_default();
                let desc = parts.get(1).map(|(p, m)| self.markup(m, *p)).unwrap_or_default();
                format!("**{}**: {}", term.trim(), desc.trim())
            }
            K::Equation => self.equation(node, offset, false),
            K::Hash | K::LineComment | K::BlockComment => String::new(),
            K::FuncCall => self.func_call(node, offset),
            K::ContentBlock => self.inner_markup(node, offset),
            K::Str => node.leaf_text().trim_matches('"').to_string(),
            K::SetRule | K::ShowRule | K::LetBinding | K::ModuleImport | K::ModuleInclude => {
                self.warn(format!("dropped `{}`", first_line(&node.full_text())));
                String::new()
            }
            _ => {
                self.warn(format!("unsupported {:?}: `{}`", node.kind(), first_line(&node.full_text())));
                String::new()
            }
        }
    }

    /// Render the Markup child of a node such as Strong or ContentBlock.
    fn inner_markup(&mut self, node: &SyntaxNode, offset: usize) -> String {
        with_offsets(node, offset).into_iter()
            .filter(|(_, c)| c.kind() == SyntaxKind::Markup)
            .map(|(p, c)| self.markup(c, p))
            .collect()
    }

    fn heading(&mut self, node: &SyntaxNode, offset: usize) -> String {
        let level = node.children().next().map_or(1, |m| m.leaf_text().len());
        let title = self.inner_markup(node, offset);
        let anchor = match self.labels.headings.get(&offset) {
            Some(label) if self.eq_number.anchors() => format!("<a id=\"{}\"></a>", anchor_id(label)),
            _ => String::new(),
        };
        format!("{} {anchor}{}", "#".repeat(level), title.trim())
    }

    fn list_item(&mut self, node: &SyntaxNode, offset: usize, marker: String) -> String {
        let saved = self.indent.clone();
        self.indent.push_str(&" ".repeat(marker.chars().count()));
        let body = self.inner_markup(node, offset);
        self.indent = saved;
        format!("{marker}{}", body.trim_start())
    }

    fn reference(&mut self, node: &SyntaxNode, offset: usize) -> String {
        let marker = node.children().next().map(|m| m.leaf_text().to_string()).unwrap_or_default();
        let label = marker.trim_start_matches('@').to_string();
        let supplement = with_offsets(node, offset).into_iter()
            .find(|(_, c)| c.kind() == SyntaxKind::ContentBlock)
            .map(|(p, c)| self.inner_markup(c, p).trim().to_string());
        let id = anchor_id(&label);
        let text = match self.labels.targets.get(&label).cloned() {
            Some(Target::Equation { number }) => match supplement {
                Some(s) if !s.is_empty() => format!("{s} {number}"),
                _ => number,
            },
            Some(Target::Heading { title }) => supplement.unwrap_or(title),
            None => {
                self.warn(format!("unresolved reference `@{label}` (citation or unsupported target)"));
                return format!("[{}]", supplement.unwrap_or(label));
            }
        };
        if self.eq_number.anchors() { format!("[{text}](#{id})") } else { text }
    }

    fn equation(&mut self, node: &SyntaxNode, offset: usize, boxed: bool) -> String {
        let text = node.full_text();
        let inner = text.strip_prefix('$').and_then(|t| t.strip_suffix('$')).unwrap_or(&text);
        let display = is_display(node);
        let mut latex = math::to_latex(inner, display);
        if boxed { latex = format!("\\boxed{{{latex}}}"); }
        if !display {
            return format!("${latex}$");
        }
        let (anchor, tag) = match self.labels.equations.get(&offset) {
            Some((number, label)) => {
                let number = number.replace(['{', '}'], "");
                let tag = match self.eq_number {
                    EqNumber::Tag => format!(" \\tag*{{{number}}}"),
                    EqNumber::Qquad => format!(" \\qquad \\text{{{number}}}"),
                    EqNumber::None => String::new(),
                };
                let anchor = label.as_ref()
                    .filter(|_| self.eq_number.anchors())
                    .map(|l| format!("<a id=\"{}\"></a>\n", anchor_id(l)))
                    .unwrap_or_default();
                (anchor, tag)
            }
            None => (String::new(), String::new()),
        };
        format!("\n{anchor}$$\n{latex}{tag}\n$$\n")
    }

    fn func_call(&mut self, node: &SyntaxNode, offset: usize) -> String {
        let name = callee_name(node).unwrap_or_default();
        if name == "math.equation" {
            if let Some(site) = equation_site(node, offset) {
                return self.equation(site.equation, site.offset, false);
            }
        }
        let Some((args_offset, args)) = with_offsets(node, offset).into_iter()
            .find(|(_, c)| c.kind() == SyntaxKind::Args) else { return String::new() };
        let arg_list = with_offsets(args, args_offset);
        let content: Vec<(usize, &SyntaxNode)> = arg_list.iter().copied()
            .filter(|(_, c)| matches!(c.kind(), SyntaxKind::ContentBlock | SyntaxKind::Equation | SyntaxKind::Str | SyntaxKind::FuncCall))
            .collect();
        let has_stroke = arg_list.iter().any(|(_, c)| c.kind() == SyntaxKind::Named && named_key(c) == "stroke");

        match name.as_str() {
            "strong" => format!("**{}**", self.render_all(&content).trim()),
            "emph" => format!("*{}*", self.render_all(&content).trim()),
            "link" => {
                let url = arg_list.iter().find(|(_, c)| c.kind() == SyntaxKind::Str)
                    .map(|(_, c)| c.leaf_text().trim_matches('"').to_string()).unwrap_or_default();
                let body: Vec<_> = content.into_iter().filter(|(_, c)| c.kind() != SyntaxKind::Str).collect();
                let text = self.render_all(&body);
                if text.trim().is_empty() { format!("<{url}>") } else { format!("[{}]({url})", text.trim()) }
            }
            "image" => {
                let path = arg_list.iter().find(|(_, c)| c.kind() == SyntaxKind::Str)
                    .map(|(_, c)| c.leaf_text().trim_matches('"').to_string()).unwrap_or_default();
                format!("![]({path})")
            }
            "box" | "rect" | "block" if has_stroke => {
                // A framed display equation becomes `\boxed{...}`.
                match sole_equation(&content) {
                    Some((p, eq)) => self.equation(eq, p, true),
                    None => self.render_all(&content),
                }
            }
            "align" | "box" | "rect" | "block" | "pad" | "text" | "par" | "underline" | "highlight"
            | "smallcaps" | "center" | "figure" | "quote" | "hide" | "move" | "scale" | "rotate" => {
                self.render_all(&content)
            }
            _ => {
                self.warn(format!("unknown function `{name}`: only its content arguments were kept"));
                self.render_all(&content)
            }
        }
    }

    fn render_all(&mut self, nodes: &[(usize, &SyntaxNode)]) -> String {
        nodes.iter().map(|(p, c)| self.node(c, *p)).collect()
    }
}

/// The single display equation inside `[ ... ]` (ignoring whitespace), if any.
fn sole_equation<'a>(content: &[(usize, &'a SyntaxNode)]) -> Option<(usize, &'a SyntaxNode)> {
    let [(pos, node)] = content else { return None };
    match node.kind() {
        SyntaxKind::Equation => Some((*pos, *node)),
        SyntaxKind::ContentBlock => {
            let (mpos, markup) = with_offsets(node, *pos).into_iter().find(|(_, c)| c.kind() == SyntaxKind::Markup)?;
            let mut found = None;
            for (p, c) in with_offsets(markup, mpos) {
                match c.kind() {
                    SyntaxKind::Space | SyntaxKind::Parbreak => {}
                    SyntaxKind::Equation if found.is_none() => found = Some((p, c)),
                    _ => return None,
                }
            }
            found
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers

fn with_offsets(node: &SyntaxNode, offset: usize) -> Vec<(usize, &SyntaxNode)> {
    let mut pos = offset;
    node.children().map(|c| { let p = pos; pos += c.len(); (p, c) }).collect()
}

/// `$ x $` (with surrounding whitespace) is display math in Typst.
fn is_display(eq: &SyntaxNode) -> bool {
    let t = eq.full_text();
    let inner = t.strip_prefix('$').and_then(|s| s.strip_suffix('$')).unwrap_or("");
    inner.len() >= 2 && inner.starts_with(char::is_whitespace) && inner.ends_with(char::is_whitespace)
}

/// Callee of a FuncCall / SetRule as a dotted path (`math.equation`).
fn callee_name(node: &SyntaxNode) -> Option<String> {
    let callee = node.children().find(|c| matches!(c.kind(), SyntaxKind::Ident | SyntaxKind::FieldAccess))?;
    Some(callee.full_text().chars().filter(|c| !c.is_whitespace()).collect())
}

fn named_key(named: &SyntaxNode) -> String {
    named.children().next().map(|k| k.leaf_text().to_string()).unwrap_or_default()
}

fn plain_text(node: &SyntaxNode) -> String {
    match node.kind() {
        SyntaxKind::Text | SyntaxKind::Space => node.leaf_text().to_string(),
        SyntaxKind::Equation => node.full_text().to_string(),
        _ => node.children().map(plain_text).collect(),
    }
}

fn escape(text: &str) -> String {
    let body = &text[1..];
    if let Some(hex) = body.strip_prefix("u{").and_then(|s| s.strip_suffix('}')) {
        if let Some(c) = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
            return c.to_string();
        }
    }
    // Markdown-escape ASCII punctuation, pass anything else through.
    if body.chars().all(|c| c.is_ascii_punctuation()) { format!("\\{body}") } else { body.to_string() }
}

fn shorthand(s: &str) -> &str {
    match s {
        "~" => "\u{a0}",
        "--" => "–",
        "---" => "—",
        "..." => "…",
        "-?" => "\u{ad}",
        "-" => "−",
        other => other,
    }
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.chars().count() > 60 { format!("{}…", line.chars().take(60).collect::<String>()) } else { line.to_string() }
}

/// Strip trailing spaces and collapse runs of blank lines.
fn tidy(md: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in md.lines() {
        let line = line.trim_end_matches([' ', '\t']);
        if line.is_empty() {
            blank += 1;
            if blank > 1 || out.is_empty() { continue; }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_equations_and_refs() {
        let src = "See @eq:a.\n\n#math.equation(numbering: \"(1)\", block: true, $ x = 1 $) <eq:a>\n";
        let md = convert(src, EqNumber::Tag).markdown;
        assert!(md.contains("See [(1)](#eq-a)."), "{md}");
        assert!(md.contains("<a id=\"eq-a\"></a>\n$$\nx = 1 \\tag*{(1)}\n$$"), "{md}");
    }

    #[test]
    fn set_rule_numbering() {
        let src = "#set math.equation(numbering: \"1\")\n$ a $ <x>\n\n$ b $\n\nInline $c$ and @x[Eq.]\n";
        let md = convert(src, EqNumber::Tag).markdown;
        assert!(md.contains("a \\tag*{1}") && md.contains("b \\tag*{2}"), "{md}");
        assert!(md.contains("Inline $c$ and [Eq. 1](#x)"), "{md}");
    }

    #[test]
    fn markup_basics() {
        let src = "= Title <sec>\n\n*bold* _it_ `code` -- @sec\n\n- one\n  two\n+ first\n";
        let md = convert(src, EqNumber::Tag).markdown;
        assert!(md.contains("# <a id=\"sec\"></a>Title"), "{md}");
        assert!(md.contains("**bold** *it* `code` – [Title](#sec)"), "{md}");
        assert!(md.contains("- one\n  two\n1. first"), "{md}");
    }

    #[test]
    fn boxed_equation() {
        let src = "#align(center)[#box(stroke: 0.5pt, [\n  $ x = y $\n])]\n";
        let md = convert(src, EqNumber::Tag).markdown;
        assert!(md.contains("$$\n\\boxed{x = y}\n$$"), "{md}");
    }

    #[test]
    fn eq_number_styles() {
        let src = "#set math.equation(numbering: \"(1)\")\n$ a $ <x>\n";
        let qquad = convert(src, EqNumber::Qquad).markdown;
        assert!(qquad.contains("$$\na \\qquad \\text{(1)}\n$$"), "{qquad}");
        let none = convert(src, EqNumber::None).markdown;
        assert!(none.contains("$$\na\n$$"), "{none}");
        assert!(!qquad.contains("<a") && !none.contains("<a"));
    }

    #[test]
    fn plain_references_without_anchors() {
        let src = "= Title <sec>\n\n#set math.equation(numbering: \"(1)\")\n$ a $ <x>\n\nSee @x, @x[Eq.] and @sec.\n";
        let md = convert(src, EqNumber::default()).markdown;
        assert!(md.contains("# Title\n"), "{md}");
        assert!(md.contains("See (1), Eq. (1) and Title."), "{md}");
    }
}
