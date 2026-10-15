use std::cell::RefCell;

use savitar_diagnostics::{Severity, Span};
use savitar_sound::check_types;
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_types::{TypeKind, TypeTable};

struct Scripted {
    answers: RefCell<Vec<Answer>>,
    asked: RefCell<Vec<u32>>,
}

impl Scripted {
    fn new(answers: Vec<Answer>) -> Self {
        Self {
            answers: RefCell::new(answers),
            asked: RefCell::new(Vec::new()),
        }
    }
}

impl TypeProvider for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }

    fn type_at(&self, _table: &mut TypeTable, _file: &str, span: Span) -> Answer {
        self.asked.borrow_mut().push(span.start);
        let mut answers = self.answers.borrow_mut();
        if answers.is_empty() {
            return Answer::OutOfScope;
        }
        answers.remove(0)
    }
}

const SOURCE: &str = "declare const a: number;\ndeclare const b: number;\n";

fn source_file() -> savitar_diagnostics::SourceFile {
    savitar_diagnostics::SourceFile::new("fixture.ts", SOURCE)
}

#[test]
fn a_fully_typed_file_reports_no_diagnostics_and_full_coverage() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = Scripted::new(vec![Answer::Known(number), Answer::Known(number)]);

    let report = check_types(&source_file(), &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.coverage.known, 2);
    assert_eq!(report.coverage.total(), 2);
    assert_eq!(report.coverage.percent_known(), 100);
}

#[test]
fn an_unsupported_type_is_reported_with_its_reason_and_rejects() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = Scripted::new(vec![
        Answer::Known(number),
        Answer::Unsupported("tuple types are not resolved yet".into()),
    ]);

    let report = check_types(&source_file(), &provider, &mut table);

    assert_eq!(report.diagnostics.len(), 1);
    let diagnostic = &report.diagnostics[0];
    assert_eq!(diagnostic.rule, "unsupported-type");
    assert_eq!(diagnostic.severity, Severity::Unsupported);
    assert!(diagnostic.severity.rejects());
    assert!(diagnostic.message.contains('b'), "{}", diagnostic.message);
    assert!(
        diagnostic.message.contains("tuple"),
        "{}",
        diagnostic.message
    );
    assert_eq!(report.coverage.known, 1);
    assert_eq!(report.coverage.unsupported, 1);
    assert_eq!(report.coverage.percent_known(), 50);
}

#[test]
fn a_silent_provider_is_reported_rather_than_assumed_typed() {
    let mut table = TypeTable::new();
    let provider = Scripted::new(vec![]);

    let report = check_types(&source_file(), &provider, &mut table);

    assert_eq!(report.coverage.out_of_scope, 2);
    assert_eq!(report.coverage.known, 0);
    assert!(
        report
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.rule == "no-type-answer")
    );
}

#[test]
fn the_pass_asks_inside_each_binding_span() {
    let mut table = TypeTable::new();
    let provider = Scripted::new(vec![]);

    let _ = check_types(&source_file(), &provider, &mut table);

    let asked = provider.asked.borrow().clone();
    assert_eq!(asked.len(), 2);
    for position in asked {
        let at = position as usize;
        assert!(
            SOURCE[at..].starts_with('a') || SOURCE[at..].starts_with('b'),
            "asked at {at}, which is {:?}",
            &SOURCE[at..at + 1]
        );
    }
}

#[test]
fn coverage_of_a_file_with_no_bindings_is_complete() {
    let mut table = TypeTable::new();
    let provider = Scripted::new(vec![]);
    let file = savitar_diagnostics::SourceFile::new("empty.ts", "export {};\n");

    let report = check_types(&file, &provider, &mut table);

    assert_eq!(report.coverage.total(), 0);
    assert_eq!(report.coverage.percent_known(), 100);
}

#[test]
fn a_type_only_import_is_not_asked_for_a_value_type() {
    let mut table = TypeTable::new();
    let provider = Scripted::new(vec![]);
    let file = savitar_diagnostics::SourceFile::new(
        "fixture.ts",
        "import type { User } from \"./model.ts\";\nimport { type Role, kind } from \"./role.ts\";\n",
    );

    let report = check_types(&file, &provider, &mut table);

    assert_eq!(
        report.coverage.total(),
        1,
        "only `kind` is a value binding; the type-only ones have no value type"
    );
    assert_eq!(provider.asked.borrow().len(), 1);
}

#[test]
fn a_type_parameter_is_not_asked_for_a_value_type() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = Scripted::new(vec![Answer::Known(number), Answer::Known(number)]);
    let file = savitar_diagnostics::SourceFile::new(
        "fixture.ts",
        "export function firstOf<T>(items: readonly T[]): T | undefined {\n  return items[0];\n}\n",
    );

    let report = check_types(&file, &provider, &mut table);

    assert_eq!(
        report.coverage.total(),
        2,
        "`firstOf` and `items` are value bindings; `T` is not"
    );
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}
