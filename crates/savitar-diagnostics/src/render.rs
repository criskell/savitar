use std::fmt::Write as _;

use crate::{Diagnostic, SourceFile};

#[must_use]
pub fn render(file: &SourceFile, diagnostics: &[Diagnostic]) -> String {
    let mut out = String::new();
    for diagnostic in diagnostics {
        let at = file.line_col(diagnostic.span.start);
        let _ = writeln!(
            out,
            "{}:{}:{} {}[{}]: {}",
            file.name(),
            at.line,
            at.column,
            diagnostic.severity.label(),
            diagnostic.rule,
            diagnostic.message
        );

        let line = file.line_text(at.line);
        if !line.is_empty() {
            let _ = writeln!(out, "  {line}");
            let pad = " ".repeat(at.column.saturating_sub(1) as usize);
            let width = file.snippet(diagnostic.span).chars().count().max(1);
            let _ = writeln!(out, "  {pad}{}", "^".repeat(width));
        }

        if let Some(help) = &diagnostic.help {
            let _ = writeln!(out, "  help: {help}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    #[test]
    fn renders_position_rule_and_caret_run() {
        let file = SourceFile::new("a.ts", "const x = y as any;\n");
        let span = Span::new(10, 18);
        let diagnostic = Diagnostic::error("no-any", span, "`any` is not sound")
            .with_help("use `unknown` and narrow it");

        let text = render(&file, &[diagnostic]);

        assert!(
            text.starts_with("a.ts:1:11 error[no-any]: `any` is not sound\n"),
            "unexpected header in:\n{text}"
        );
        assert!(
            text.contains("  const x = y as any;\n"),
            "missing source line:\n{text}"
        );
        assert!(
            text.contains("\n            ^^^^^^^^\n"),
            "missing caret run in:\n{text}"
        );
        assert!(text.contains("  help: use `unknown` and narrow it\n"));
    }
}
