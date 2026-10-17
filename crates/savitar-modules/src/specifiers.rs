use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ImportDeclaration, ImportDeclarationSpecifier, ImportOrExportKind, Statement as AstStatement,
};
use oxc_parser::Parser;
use oxc_span::SourceType;
use savitar_diagnostics::Span;

use crate::Carries;

pub struct Found {
    pub specifier: String,
    pub span: Span,
    pub carries: Carries,
}

#[must_use]
pub fn specifiers_of(path: &Path, text: &str) -> Vec<Found> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap_or_else(|_| SourceType::ts());
    let parsed = Parser::new(&allocator, text, source_type).parse();
    if parsed.fatal_error {
        return Vec::new();
    }

    let mut found = Vec::new();
    for statement in &parsed.program.body {
        match statement {
            AstStatement::ImportDeclaration(declaration) => found.push(Found {
                specifier: declaration.source.value.to_string(),
                span: span_of(declaration.source.span),
                carries: what_an_import_carries(declaration),
            }),
            AstStatement::ExportFromDeclaration(declaration) => found.push(Found {
                specifier: declaration.source.value.to_string(),
                span: span_of(declaration.source.span),
                carries: if declaration.export_kind == ImportOrExportKind::Type
                    || every_export_specifier_is_a_type(declaration)
                {
                    Carries::TypesOnly
                } else {
                    Carries::Values
                },
            }),
            AstStatement::ExportAllDeclaration(declaration) => found.push(Found {
                specifier: declaration.source.value.to_string(),
                span: span_of(declaration.source.span),
                carries: if declaration.export_kind == ImportOrExportKind::Type {
                    Carries::TypesOnly
                } else {
                    Carries::Values
                },
            }),
            _ => {}
        }
    }
    found
}

fn span_of(span: oxc_span::Span) -> Span {
    Span::new(span.start, span.end)
}

fn what_an_import_carries(declaration: &ImportDeclaration<'_>) -> Carries {
    if declaration.import_kind == ImportOrExportKind::Type {
        return Carries::TypesOnly;
    }
    let Some(specifiers) = declaration.specifiers.as_ref() else {
        return Carries::Values;
    };
    if specifiers.is_empty() {
        return Carries::Values;
    }
    if specifiers.iter().all(is_a_type_specifier) {
        return Carries::TypesOnly;
    }
    Carries::Values
}

fn is_a_type_specifier(specifier: &ImportDeclarationSpecifier<'_>) -> bool {
    match specifier {
        ImportDeclarationSpecifier::ImportSpecifier(named) => {
            named.import_kind == ImportOrExportKind::Type
        }
        ImportDeclarationSpecifier::ImportDefaultSpecifier(_)
        | ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => false,
    }
}

fn every_export_specifier_is_a_type(declaration: &oxc_ast::ast::ExportFromDeclaration<'_>) -> bool {
    !declaration.specifiers.is_empty()
        && declaration
            .specifiers
            .iter()
            .all(|specifier| specifier.export_kind == ImportOrExportKind::Type)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::Carries;

    use super::specifiers_of;

    fn what_one_line_carries(text: &str) -> Carries {
        let found = specifiers_of(Path::new("module.ts"), text);
        assert_eq!(found.len(), 1, "the line should hold one specifier");
        found[0].carries
    }

    #[test]
    fn an_import_of_values_carries_values() {
        assert_eq!(
            what_one_line_carries("import { a, b } from \"./c.ts\";\n"),
            Carries::Values
        );
    }

    #[test]
    fn an_import_whose_every_name_is_a_type_carries_types_only() {
        assert_eq!(
            what_one_line_carries("import { type A, type B } from \"./c.ts\";\n"),
            Carries::TypesOnly
        );
    }

    #[test]
    fn a_whole_import_of_types_carries_types_only() {
        assert_eq!(
            what_one_line_carries("import type { A } from \"./c.ts\";\n"),
            Carries::TypesOnly
        );
    }

    #[test]
    fn an_import_mixing_a_type_and_a_value_carries_values() {
        assert_eq!(
            what_one_line_carries("import { type A, b } from \"./c.ts\";\n"),
            Carries::Values
        );
    }

    #[test]
    fn an_import_mixing_a_value_and_a_type_carries_values_whichever_comes_first() {
        assert_eq!(
            what_one_line_carries("import { a, type B } from \"./c.ts\";\n"),
            Carries::Values
        );
    }

    #[test]
    fn an_import_mixing_a_type_with_a_default_carries_values() {
        assert_eq!(
            what_one_line_carries("import a, { type B } from \"./c.ts\";\n"),
            Carries::Values
        );
    }

    #[test]
    fn an_import_with_no_specifier_carries_values_because_running_the_module_is_the_point() {
        assert_eq!(
            what_one_line_carries("import \"./c.ts\";\n"),
            Carries::Values
        );
    }

    #[test]
    fn a_re_export_mixing_a_type_and_a_value_carries_values() {
        assert_eq!(
            what_one_line_carries("export { type A, b } from \"./c.ts\";\n"),
            Carries::Values
        );
    }

    #[test]
    fn a_re_export_whose_every_name_is_a_type_carries_types_only() {
        assert_eq!(
            what_one_line_carries("export { type A, type B } from \"./c.ts\";\n"),
            Carries::TypesOnly
        );
    }

    #[test]
    fn a_re_export_of_everything_carries_values_unless_it_is_written_as_types() {
        assert_eq!(
            what_one_line_carries("export * from \"./c.ts\";\n"),
            Carries::Values
        );
        assert_eq!(
            what_one_line_carries("export type * from \"./c.ts\";\n"),
            Carries::TypesOnly
        );
    }
}
