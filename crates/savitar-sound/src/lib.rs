mod config;
mod rules;
mod types;

use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use savitar_diagnostics::{Diagnostic, Severity, SourceFile, Span};

pub use config::{CONFIG_REQUIREMENTS, ConfigRequirement, Demand, check_tsconfig};
pub use rules::{RULES, Rule};
pub use types::{Coverage, TypeReport, check_types};

#[derive(Debug)]
pub struct Report {
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    #[must_use]
    pub fn rejected(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity.rejects())
    }

    #[must_use]
    pub fn count(&self, severity: Severity) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == severity)
            .count()
    }
}

#[must_use]
pub fn check(file: &SourceFile) -> Report {
    if file
        .name()
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|base| base == "tsconfig.json" || base.ends_with(".tsconfig.json"))
    {
        return Report {
            diagnostics: check_tsconfig(file),
        };
    }

    let allocator = Allocator::default();
    let source_type = SourceType::from_path(file.name()).unwrap_or_else(|_| SourceType::ts());
    let parsed = Parser::new(&allocator, file.text(), source_type).parse();

    let mut diagnostics: Vec<Diagnostic> = parsed
        .diagnostics
        .iter()
        .map(|error| {
            let span = error.labels.first().map_or(Span::new(0, 0), |label| {
                let start = label.offset();
                Span::new(start, start + label.len())
            });
            Diagnostic::error("parse", span, error.message.to_string())
        })
        .collect();

    if parsed.fatal_error {
        return Report { diagnostics };
    }

    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&parsed.program)
        .semantic;
    for node in semantic.nodes() {
        for rule in RULES {
            if let Some(diagnostic) = (rule.run)(node.kind()) {
                diagnostics.push(diagnostic);
            }
        }
    }

    diagnostics.sort_by_key(|d| (d.span.start, d.span.end, d.rule));
    Report { diagnostics }
}
