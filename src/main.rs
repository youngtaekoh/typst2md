mod expand;
mod markdown;
mod math;

use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;

const USAGE: &str = "Usage: typst2md [options] input.typ output.(md|typ)

  output.md   expand macros, then convert to Markdown (math -> LaTeX via tylax)
  output.typ  only expand macros (Typst output)

Options:
  --eq-number <qquad|tag|none>
              how equation numbers appear in display math (default: qquad)
                qquad  \\qquad \\text{(1)}  works in any math renderer
                tag    \\tag*{(1)}         right-aligned; KaTeX / MathJax.
                       Also adds <a id> anchors and makes @refs links.
                none   no number
  --keep-defs keep #let macro definitions (Typst output only)";

fn main() -> Result<(), Box<dyn Error>> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("{USAGE}");
        return Ok(());
    }
    let keep_defs = args.iter().any(|a| a == "--keep-defs");
    args.retain(|a| a != "--keep-defs");
    let mut eq_number = markdown::EqNumber::default();
    if let Some(i) = args.iter().position(|a| a == "--eq-number" || a.starts_with("--eq-number=")) {
        let value = match args[i].strip_prefix("--eq-number=") {
            Some(v) => v.to_string(),
            None if i + 1 < args.len() => args.remove(i + 1),
            None => return Err("--eq-number needs a value (tag, qquad, or none)".into()),
        };
        args.remove(i);
        eq_number = value.parse()?;
    }
    if args.len() != 2 {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let input = Path::new(&args[0]);
    let output = Path::new(&args[1]);
    if input == output {
        return Err("input and output must be different paths".into());
    }
    let to_markdown = output.extension().is_some_and(|e| e == "md" || e == "markdown");

    let src = fs::read_to_string(input)?;
    // Definitions are irrelevant in Markdown output, so always drop them there.
    let expanded = expand::transform(&src, to_markdown || !keep_defs);
    eprintln!(
        "Made {} macro replacement(s); found {} simple macro(s); removed {} definition(s).",
        expanded.count, expanded.macros.len(), expanded.removed.len()
    );
    for name in &expanded.kept {
        eprintln!("warning: macro `{name}` is still referenced after expansion");
    }

    if to_markdown {
        let converted = markdown::convert(&expanded.output, eq_number);
        for w in &converted.warnings {
            eprintln!("warning: {w}");
        }
        fs::write(output, converted.markdown)?;
    } else {
        fs::write(output, expanded.output)?;
    }
    Ok(())
}
