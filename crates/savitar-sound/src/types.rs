use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use savitar_diagnostics::{Diagnostic, SourceFile, Span};
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_types::TypeTable;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub known: usize,
    pub unsupported: usize,
    pub out_of_scope: usize,
}

impl Coverage {
    #[must_use]
    pub fn total(&self) -> usize {
        self.known + self.unsupported + self.out_of_scope
    }

    #[must_use]
    pub fn percent_known(&self) -> u32 {
        let total = self.total();
        if total == 0 {
            return 100;
        }
        u32::try_from(self.known * 100 / total).unwrap_or(0)
    }
}

#[derive(Debug)]
pub struct TypeReport {
    pub diagnostics: Vec<Diagnostic>,
    pub coverage: Coverage,
}

#[must_use]
pub fn check_types(
    file: &SourceFile,
    provider: &dyn TypeProvider,
    table: &mut TypeTable,
) -> TypeReport {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(file.name()).unwrap_or_else(|_| SourceType::ts());
    let parsed = Parser::new(&allocator, file.text(), source_type).parse();
    if parsed.fatal_error {
        return TypeReport {
            diagnostics: Vec::new(),
            coverage: Coverage::default(),
        };
    }

    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&parsed.program)
        .semantic;

    let mut diagnostics = Vec::new();
    let mut coverage = Coverage::default();

    for node in semantic.nodes() {
        let (span, name) = match node.kind() {
            AstKind::BindingIdentifier(binding) => (binding.span, binding.name.as_str()),
            _ => continue,
        };

        let query = Span::new(span.end.saturating_sub(1), span.end);
        match provider.type_at(table, file.name(), query) {
            Answer::Known(_) => coverage.known += 1,
            Answer::OutOfScope => {
                coverage.out_of_scope += 1;
                diagnostics.push(
                    Diagnostic::unsupported(
                        "no-type-answer",
                        Span::new(span.start, span.end),
                        format!("the provider has no type for `{name}`"),
                    )
                    .with_help("check that the file belongs to the project the provider loaded"),
                );
            }
            Answer::Unsupported(reason) => {
                coverage.unsupported += 1;
                diagnostics.push(
                    Diagnostic::unsupported(
                        "unsupported-type",
                        Span::new(span.start, span.end),
                        format!("the type of `{name}` is not modelled yet: {reason}"),
                    )
                    .with_help("Savitar rejects what it cannot model instead of guessing"),
                );
            }
        }
    }

    diagnostics.sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.rule));
    TypeReport {
        diagnostics,
        coverage,
    }
}
