use std::cell::RefCell;

use savitar_diagnostics::{SourceFile, Span};
use savitar_ir::{BinaryOp, Rvalue, Statement, Terminator, verify};
use savitar_lower::lower;
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_types::{Field, IndexSignature, TypeKind, TypeTable};

struct ByName {
    answers: RefCell<Vec<(String, Answer)>>,
    source: String,
}

impl ByName {
    fn new(source: &str, answers: Vec<(&str, Answer)>) -> Self {
        Self {
            answers: RefCell::new(
                answers
                    .into_iter()
                    .map(|(name, answer)| (name.to_string(), answer))
                    .collect(),
            ),
            source: source.to_string(),
        }
    }
}

impl ByName {
    fn is_a_function_name_ending_at(&self, at: usize) -> bool {
        let prefix = &self.source[..=at.min(self.source.len() - 1)];
        let Some(start) = prefix.rfind("function ") else {
            return false;
        };
        let name = &prefix[start + "function ".len()..];
        !name.is_empty()
            && name
                .chars()
                .all(|letter| letter.is_alphanumeric() || letter == '_')
    }
}

impl TypeProvider for ByName {
    fn name(&self) -> &'static str {
        "by-name"
    }

    fn type_at(&self, table: &mut TypeTable, _file: &str, span: Span) -> Answer {
        let at = span.start as usize;
        for (name, answer) in self.answers.borrow().iter() {
            let ends_here = self.source[..=at.min(self.source.len() - 1)].ends_with(name.as_str());
            if ends_here {
                return answer.clone();
            }
        }
        if self.is_a_function_name_ending_at(at) {
            let unknown = table.intern(TypeKind::Unknown);
            return Answer::Known(table.function(Vec::new(), unknown));
        }
        Answer::OutOfScope
    }
}

fn returning(table: &mut TypeTable, returns: savitar_types::TypeId) -> Answer {
    Answer::Known(table.function(Vec::new(), returns))
}

const ADD: &str = "function add(left: number, right: number): number {\n  const sum = left + right;\n  return sum;\n}\n";

#[test]
fn a_straight_line_function_lowers_to_valid_code() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        ADD,
        vec![
            ("add", returning(&mut table, number)),
            ("left", Answer::Known(number)),
            ("right", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("add.ts", ADD);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
    let function = &report.functions[0];
    assert_eq!(function.name(), "add");
    assert_eq!(function.parameters().len(), 2);
    assert_eq!(verify(function), vec![]);

    let entry = function.entry().expect("the function should have a block");
    let block = function.block(entry);
    let Terminator::Return(Some(returned)) = block.terminator else {
        panic!("expected a return, got {:?}", block.terminator);
    };
    assert_eq!(table.kind(function.type_of(returned)), &TypeKind::Number);

    let sum = block
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::Binary { op, .. },
                ..
            } => (*op == BinaryOp::Add).then_some(*target),
            _ => None,
        })
        .expect("the sum should be lowered to a binary instruction");
    assert_eq!(sum, returned);
}

#[test]
fn adding_a_number_to_a_string_is_refused_rather_than_guessed() {
    let source = "function bad(left: number, right: string): string {\n  const joined = left + right;\n  return joined;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("left", Answer::Known(number)),
            ("right", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("bad.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-operator");
    assert!(
        report.diagnostics[0].message.contains("joins two strings"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn reading_a_field_the_shape_does_not_declare_is_refused() {
    let source =
        "function take(point: Point): number {\n  const value = point.z;\n  return value;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let point = table.object(vec![Field {
        name: "x".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let provider = ByName::new(source, vec![("point", Answer::Known(point))]);
    let file = SourceFile::new("take.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
    assert!(
        report.diagnostics[0].message.contains("no field `z`"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_element_read_is_optional_because_the_index_may_be_out_of_range() {
    let source = "function first(items: number[]): number | undefined {\n  const head = items[0];\n  return head;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let array = table.intern(TypeKind::Array(number));
    let provider = ByName::new(source, vec![("items", Answer::Known(array))]);
    let file = SourceFile::new("first.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let entry = function.entry().expect("a block");
    let Terminator::Return(Some(returned)) = function.block(entry).terminator else {
        panic!("expected a return");
    };
    assert!(
        table.admits_undefined(function.type_of(returned)),
        "reading `items[0]` yields `number | undefined`, never `number`"
    );
}

#[test]
fn an_unbound_name_is_refused_rather_than_invented() {
    let source = "function stray(): number {\n  return elsewhere;\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("stray.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(
        report.diagnostics[0].rule, "no-type-answer",
        "a free name is the provider's business, and it has no type for this one"
    );
}

#[test]
fn an_unsupported_statement_names_itself_instead_of_being_skipped() {
    let source = "function loops(limit: number): number {\n  do {\n    return limit;\n  } while (true);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("limit", Answer::Known(number))]);
    let file = SourceFile::new("loops.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-statement");
}

#[test]
fn an_object_literal_lowers_to_a_shape_built_from_its_values() {
    let source = "function make(name: string): { label: string; count: number } {\n  const built = { label: name, count: 1 };\n  return built;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let number = table.intern(TypeKind::Number);
    let shape = table.object(vec![
        Field {
            name: "label".to_string(),
            type_id: text,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "count".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    let provider = ByName::new(
        source,
        vec![
            ("make", returning(&mut table, shape)),
            ("name", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("make.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let entry = function.entry().expect("a block");
    let Terminator::Return(Some(returned)) = function.block(entry).terminator else {
        panic!("expected a return");
    };
    let TypeKind::Object { fields, .. } = table.kind(function.type_of(returned)).clone() else {
        panic!(
            "expected an object, got {:?}",
            table.kind(function.type_of(returned))
        );
    };
    let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    assert_eq!(names, vec!["count", "label"]);
}

#[test]
fn a_value_that_differs_between_branches_arrives_as_a_block_parameter() {
    let source = "function pick(flag: boolean, left: number, right: number): number {\n  let chosen = left;\n  if (flag) {\n    chosen = right;\n  }\n  return chosen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, number)),
            ("flag", Answer::Known(boolean)),
            ("left", Answer::Known(number)),
            ("right", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let joins: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .collect();
    assert_eq!(joins.len(), 1, "exactly one join should take a parameter");
    let (join, block) = &joins[0];
    assert_eq!(block.parameters.len(), 1);

    let Terminator::Return(Some(returned)) = block.terminator else {
        panic!("the join should return, got {:?}", block.terminator);
    };
    assert_eq!(
        returned, block.parameters[0],
        "the returned value must be the parameter the branches carry in"
    );

    let arguments: Vec<usize> = function
        .blocks()
        .filter_map(|(_, candidate)| match &candidate.terminator {
            Terminator::Jump { target, arguments } if target == join => Some(arguments.len()),
            _ => None,
        })
        .collect();
    assert_eq!(arguments, vec![1, 1], "both arms must pass one argument");
}

#[test]
fn a_value_untouched_by_the_branches_needs_no_parameter() {
    let source = "function keep(flag: boolean, value: number): number {\n  const held = value;\n  if (flag) {\n    const ignored = value;\n  }\n  return held;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("flag", Answer::Known(boolean)),
            ("value", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("keep.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        function
            .blocks()
            .all(|(_, block)| block.parameters.is_empty()),
        "nothing changed across the branches, so no join parameter is needed"
    );
}

#[test]
fn a_branch_that_returns_does_not_feed_the_join() {
    let source = "function guard(flag: boolean, value: number): number {\n  let held = value;\n  if (flag) {\n    return value;\n  } else {\n    held = value;\n  }\n  return held;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("flag", Answer::Known(boolean)),
            ("value", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("guard.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        function
            .blocks()
            .all(|(_, block)| block.parameters.is_empty()),
        "only one arm reaches the join, so its values flow straight through"
    );
    let returns = function
        .blocks()
        .filter(|(_, block)| matches!(block.terminator, Terminator::Return(_)))
        .count();
    assert_eq!(returns, 2, "both the guard and the tail return");
}

#[test]
fn both_branches_returning_leaves_no_join_at_all() {
    let source = "function always(flag: boolean, value: number): number {\n  if (flag) {\n    return value;\n  } else {\n    return value;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("flag", Answer::Known(boolean)),
            ("value", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("always.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let dominance = savitar_ir::Dominance::of(function);
    assert_eq!(
        dominance.reachable_blocks().len(),
        3,
        "entry plus two arms; the join exists but nothing jumps to it"
    );
    let unreachable = function
        .blocks()
        .filter(|(id, _)| !dominance.reachable(*id))
        .count();
    assert_eq!(unreachable, 1, "the join is left stranded, not deleted");
}

#[test]
fn a_name_that_leaves_the_branches_with_two_types_carries_their_union() {
    let source = "function mixed(flag: boolean, left: number, right: string): void {\n  let held = left;\n  if (flag) {\n    held = right;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("flag", Answer::Known(boolean)),
            ("left", Answer::Known(number)),
            ("right", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("mixed.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let joins: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .collect();
    assert_eq!(joins.len(), 1, "the join should take the merged name");
    let held = joins[0].1.parameters[0];
    let merged = table.union(vec![number, text]);
    assert_eq!(function.type_of(held), merged);
}

#[test]
fn assigning_to_an_unbound_name_is_refused() {
    let source = "function stray(value: number): void {\n  missing = value;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("value", Answer::Known(number))]);
    let file = SourceFile::new("stray.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-reference");
}

#[test]
fn a_loop_carries_its_running_total_through_a_header_parameter() {
    let source = "function total(limit: number): number {\n  let sum = 0;\n  let seen = 0;\n  while (seen < limit) {\n    sum += seen;\n    seen++;\n  }\n  return sum;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("limit", Answer::Known(number))]);
    let file = SourceFile::new("total.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let headers: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .collect();
    assert_eq!(headers.len(), 1, "only the loop header takes parameters");
    let (header, block) = &headers[0];
    assert_eq!(
        block.parameters.len(),
        2,
        "both `sum` and `seen` are carried across iterations"
    );

    let entering: Vec<usize> = function
        .blocks()
        .filter_map(|(_, candidate)| match &candidate.terminator {
            Terminator::Jump { target, arguments } if target == header => Some(arguments.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        entering,
        vec![2, 2],
        "the entry and the back edge both pass both values"
    );
}

#[test]
fn a_loop_that_assigns_nothing_takes_no_header_parameter() {
    let source = "function spin(flag: boolean): void {\n  while (flag) {\n    const ignored = flag;\n  }\n}\n";
    let mut table = TypeTable::new();
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(source, vec![("flag", Answer::Known(boolean))]);
    let file = SourceFile::new("spin.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        function
            .blocks()
            .all(|(_, block)| block.parameters.is_empty())
    );
}

#[test]
fn a_loop_whose_body_branches_still_carries_every_name() {
    let source = "function count(limit: number, flag: boolean): number {\n  let seen = 0;\n  while (seen < limit) {\n    if (flag) {\n      seen += 2;\n    } else {\n      seen += 1;\n    }\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("limit", Answer::Known(number)),
            ("flag", Answer::Known(boolean)),
        ],
    );
    let file = SourceFile::new("count.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(
        verify(function),
        vec![],
        "the inner join and the loop header must both carry `seen`"
    );
    let carrying = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .count();
    assert_eq!(carrying, 2, "one for the inner join, one for the header");
}

#[test]
fn a_compound_assignment_on_a_string_joins_instead_of_adding() {
    let source = "function join(left: string, right: string): string {\n  let out = left;\n  out += right;\n  return out;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("join", returning(&mut table, text)),
            ("left", Answer::Known(text)),
            ("right", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("join.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let entry = function.entry().expect("a block");
    let Terminator::Return(Some(returned)) = function.block(entry).terminator else {
        panic!("expected a return");
    };
    assert_eq!(table.kind(function.type_of(returned)), &TypeKind::String);
}

#[test]
fn incrementing_something_that_is_not_a_number_is_refused() {
    let source = "function bump(label: string): void {\n  let held = label;\n  held++;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(source, vec![("label", Answer::Known(text))]);
    let file = SourceFile::new("bump.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-operator");
}

#[test]
fn a_binding_declared_in_a_block_does_not_escape_it() {
    let source = "function scoped(value: number): number {\n  let out = value;\n  {\n    const inner = value;\n    out = inner;\n  }\n  return out;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("value", Answer::Known(number))]);
    let file = SourceFile::new("scoped.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(verify(&report.functions[0]), vec![]);
}

#[test]
fn a_name_used_after_its_block_closed_is_refused() {
    let source = "function leaked(value: number): number {\n  {\n    const inner = value;\n  }\n  return inner;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("value", Answer::Known(number))]);
    let file = SourceFile::new("leaked.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(
        report.diagnostics[0].rule, "no-type-answer",
        "the block closed, so the name is free, and the provider has no type for it"
    );
}

#[test]
fn a_for_of_over_an_array_yields_the_element_not_an_optional() {
    let source = "function total(values: readonly number[]): number {\n  let sum = 0;\n  for (const value of values) {\n    sum += value;\n  }\n  return sum;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let array = table.intern(TypeKind::ReadonlyArray(number));
    let provider = ByName::new(source, vec![("values", Answer::Known(array))]);
    let file = SourceFile::new("total.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let element = function
        .blocks()
        .flat_map(|(_, block)| block.statements.iter())
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::ReadElement { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the loop should read an element");
    assert_eq!(
        table.kind(function.type_of(element)),
        &TypeKind::Number,
        "the loop bound keeps the element present, and the sparse-array rules keep it dense"
    );

    let headers: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .collect();
    assert_eq!(headers.len(), 1);
    assert_eq!(
        headers[0].1.parameters.len(),
        2,
        "the index and the running total are both carried"
    );
}

#[test]
fn a_for_of_over_something_that_is_not_an_array_is_refused() {
    let source = "function walk(values: Iterable<number>): number {\n  let sum = 0;\n  for (const value of values) {\n    sum += value;\n  }\n  return sum;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let opaque = table.object(vec![Field {
        name: "size".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let provider = ByName::new(source, vec![("values", Answer::Known(opaque))]);
    let file = SourceFile::new("walk.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics[0].message.contains("iterator protocol"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_conditional_expression_joins_both_arms_through_a_parameter() {
    let source = "function pick(flag: boolean, left: number, right: number): number {\n  const chosen = flag ? left : right;\n  return chosen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, number)),
            ("flag", Answer::Known(boolean)),
            ("left", Answer::Known(number)),
            ("right", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let joins: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .collect();
    assert_eq!(joins.len(), 1);
    let Terminator::Return(Some(returned)) = joins[0].1.terminator else {
        panic!("the join should return the chosen value");
    };
    assert_eq!(returned, joins[0].1.parameters[0]);
}

#[test]
fn a_conditional_with_two_types_carries_their_union() {
    let source = "function maybe(flag: boolean, value: number): number | undefined {\n  const held = flag ? value : undefined;\n  return held;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("flag", Answer::Known(boolean)),
            ("value", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("maybe.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let Terminator::Return(Some(returned)) = function
        .blocks()
        .find(|(_, block)| !block.parameters.is_empty())
        .expect("a join")
        .1
        .terminator
    else {
        panic!("expected a return");
    };
    assert!(table.admits_undefined(function.type_of(returned)));
}

#[test]
fn a_name_the_function_does_not_bind_is_read_as_a_global() {
    let source = "function call(value: number): number {\n  return helper(value);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let helper = table.function(
        vec![savitar_types::Parameter {
            type_id: number,
            rest: false,
        }],
        number,
    );
    let provider = ByName::new(
        source,
        vec![
            ("value", Answer::Known(number)),
            ("helper", Answer::Known(helper)),
        ],
    );
    let file = SourceFile::new("call.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        function
            .blocks()
            .flat_map(|(_, block)| block.statements.iter())
            .any(|statement| matches!(
                statement,
                Statement::Assign {
                    value: Rvalue::Global(_),
                    ..
                }
            )),
        "the free name should become a global read, not a refusal"
    );
}

#[test]
fn a_generic_call_takes_its_type_argument_from_the_argument_it_is_given() {
    let source =
        "function head(items: number[]): number | undefined {\n  return firstOf(items);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let numbers = table.intern(TypeKind::Array(number));
    let maybe = table.optional(number);
    let held = table.fresh_type_parameter();
    let of_held = table.intern(TypeKind::ReadonlyArray(held));
    let maybe_held = table.optional(held);
    let first_of = table.generic_function(
        vec![held],
        vec![savitar_types::Parameter {
            type_id: of_held,
            rest: false,
        }],
        maybe_held,
    );
    let provider = ByName::new(
        source,
        vec![
            ("head", returning(&mut table, maybe)),
            ("items", Answer::Known(numbers)),
            ("firstOf", Answer::Known(first_of)),
        ],
    );
    let file = SourceFile::new("head.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let called = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::Call { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the call should be lowered");
    assert_eq!(
        function.type_of(called),
        maybe,
        "the type parameter stands for the element of the array it was given"
    );
}

#[test]
fn a_generic_call_with_nothing_to_bind_its_type_parameter_is_refused() {
    let source = "function head(): number {\n  return makeOne();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let held = table.fresh_type_parameter();
    let make_one = table.generic_function(vec![held], Vec::new(), held);
    let provider = ByName::new(
        source,
        vec![
            ("head", returning(&mut table, number)),
            ("makeOne", Answer::Known(make_one)),
        ],
    );
    let file = SourceFile::new("head.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0]
            .message
            .contains("nothing to stand for"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_generic_call_that_binds_one_type_parameter_twice_is_refused() {
    let source =
        "function join(left: number, right: string): number {\n  return pick(left, right);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let held = table.fresh_type_parameter();
    let pick = table.generic_function(
        vec![held],
        vec![
            savitar_types::Parameter {
                type_id: held,
                rest: false,
            },
            savitar_types::Parameter {
                type_id: held,
                rest: false,
            },
        ],
        held,
    );
    let provider = ByName::new(
        source,
        vec![
            ("join", returning(&mut table, number)),
            ("left", Answer::Known(number)),
            ("right", Answer::Known(text)),
            ("pick", Answer::Known(pick)),
        ],
    );
    let file = SourceFile::new("join.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0]
            .message
            .contains("two different types"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_returned_value_the_declared_type_does_not_admit_is_refused() {
    let source = "function count(label: string): number {\n  return label;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("count", returning(&mut table, number)),
            ("label", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("count.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-return");
}

#[test]
fn a_returned_literal_arrives_at_the_return_as_the_declared_type() {
    let source = "function one(): number {\n  return 1;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("one", returning(&mut table, number))]);
    let file = SourceFile::new("one.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let Terminator::Return(Some(returned)) = only_block(function).terminator else {
        panic!("expected a return with a value");
    };
    assert_eq!(function.type_of(returned), number);
}

#[test]
fn a_function_that_can_reach_its_end_without_a_value_is_refused() {
    let source = "function count(flag: boolean): number {\n  if (flag) {\n    return 1;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("count", returning(&mut table, number)),
            ("flag", Answer::Known(boolean)),
        ],
    );
    let file = SourceFile::new("count.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-return");
    assert!(
        report.diagnostics[0].message.contains("reach its end"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_bare_return_where_a_value_is_declared_is_refused() {
    let source = "function count(flag: boolean): number {\n  return;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("count", returning(&mut table, number)),
            ("flag", Answer::Known(boolean)),
        ],
    );
    let file = SourceFile::new("count.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-return");
}

#[test]
fn an_async_function_returns_what_its_promise_holds() {
    let source = "async function later(label: string): Promise<string> {\n  return label;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let promised = table.intern(TypeKind::Promise(text));
    let provider = ByName::new(
        source,
        vec![
            ("later", returning(&mut table, promised)),
            ("label", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("later.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let Terminator::Return(Some(returned)) = only_block(function).terminator else {
        panic!("expected a return with a value");
    };
    assert_eq!(
        function.type_of(returned),
        text,
        "the value returned inside an async function fits what the promise holds"
    );
}

#[test]
fn an_async_function_whose_declared_return_is_not_a_promise_is_refused() {
    let source = "async function later(label: string): Promise<string> {\n  return label;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("later", returning(&mut table, text)),
            ("label", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("later.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-function");
    assert!(
        report.diagnostics[0].message.contains("not a promise"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_generator_function_is_refused_rather_than_lowered_as_a_plain_one() {
    let source = "function* counting(): Generator<number> {\n  return;\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, Vec::new());
    let file = SourceFile::new("counting.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-function");
    assert!(
        report.diagnostics[0].message.contains("generator"),
        "{}",
        report.diagnostics[0].message
    );
}

fn tagged_union(table: &mut TypeTable) -> (savitar_types::TypeId, savitar_types::TypeId) {
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let counted_tag = table.intern(TypeKind::StringLiteral("counted".to_string()));
    let named_tag = table.intern(TypeKind::StringLiteral("named".to_string()));
    let counted = table.object(vec![
        Field {
            name: "tag".to_string(),
            type_id: counted_tag,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "value".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    let named = table.object(vec![
        Field {
            name: "tag".to_string(),
            type_id: named_tag,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "value".to_string(),
            type_id: text,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    let union = table.union(vec![counted, named]);
    (union, named)
}

#[test]
fn an_object_literal_takes_the_union_member_its_discriminant_names() {
    let source =
        "function hold(): void {\n  const kept: Tagged = { tag: \"named\", value: \"hi\" };\n}\n";
    let mut table = TypeTable::new();
    let (union, named) = tagged_union(&mut table);
    let provider = ByName::new(source, vec![("kept", Answer::Known(union))]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let built = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::NewObject { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the literal should be built");
    assert_eq!(
        function.type_of(built),
        named,
        "the member with the matching discriminant types the literal, so `value` stays text"
    );
}

#[test]
fn an_object_literal_whose_discriminant_names_no_member_is_refused() {
    let source =
        "function hold(): void {\n  const kept: Tagged = { tag: \"other\", value: \"hi\" };\n}\n";
    let mut table = TypeTable::new();
    let (union, _) = tagged_union(&mut table);
    let provider = ByName::new(source, vec![("kept", Answer::Known(union))]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics[0]
            .message
            .contains("fits the declared type"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_array_literal_takes_the_tuple_the_declaration_asks_for() {
    let source = "function hold(): void {\n  const kept: [number, string] = [1, \"hi\"];\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![number, text], false);
    let provider = ByName::new(source, vec![("kept", Answer::Known(pair))]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let Some(Statement::Assign { target, .. }) = only_block(function).statements.last() else {
        panic!("expected the declaration to assign");
    };
    assert_eq!(function.type_of(*target), pair);
}

#[test]
fn an_array_literal_the_tuple_has_no_room_for_is_refused() {
    let source = "function hold(): void {\n  const kept: [number, string] = [1, \"hi\", 2];\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![number, text], false);
    let provider = ByName::new(source, vec![("kept", Answer::Known(pair))]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics[0]
            .message
            .contains("fits the declared type"),
        "{}",
        report.diagnostics[0].message
    );
}

fn one_parameter(table: &mut TypeTable, takes: savitar_types::TypeId) -> savitar_types::TypeId {
    let number = table.intern(TypeKind::Number);
    table.function(
        vec![savitar_types::Parameter {
            type_id: takes,
            rest: false,
        }],
        number,
    )
}

#[test]
fn an_argument_the_parameter_does_not_admit_is_refused() {
    let source = "function call(label: string): number {\n  return helper(label);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let helper = one_parameter(&mut table, number);
    let provider = ByName::new(
        source,
        vec![
            ("label", Answer::Known(text)),
            ("helper", Answer::Known(helper)),
        ],
    );
    let file = SourceFile::new("call.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-argument");
}

#[test]
fn a_call_that_leaves_out_an_argument_is_refused_because_the_minimum_arity_is_unknown() {
    let source = "function call(): number {\n  return helper();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let maybe = table.optional(number);
    let helper = one_parameter(&mut table, maybe);
    let provider = ByName::new(source, vec![("helper", Answer::Known(helper))]);
    let file = SourceFile::new("call.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0].message.contains("leaves out"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_call_with_more_arguments_than_the_signature_takes_is_refused() {
    let source = "function call(value: number): number {\n  return helper(value, value);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let helper = one_parameter(&mut table, number);
    let provider = ByName::new(
        source,
        vec![
            ("value", Answer::Known(number)),
            ("helper", Answer::Known(helper)),
        ],
    );
    let file = SourceFile::new("call.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0].message.contains("more arguments"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_object_literal_argument_is_typed_by_the_parameter_it_is_passed_to() {
    let source = "function call(): number {\n  return helper({ x: 1 });\n}\n";
    let mut table = TypeTable::new();
    let point = point_shape(&mut table);
    let helper = one_parameter(&mut table, point);
    let provider = ByName::new(source, vec![("helper", Answer::Known(helper))]);
    let file = SourceFile::new("call.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let passed = function
        .blocks()
        .flat_map(|(_, block)| block.statements.iter())
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::NewObject { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the literal should be built");
    assert_eq!(function.type_of(passed), point);
}

#[test]
fn a_rest_parameter_takes_every_argument_past_the_fixed_ones() {
    let source =
        "function call(label: string): number {\n  return joinAll(label, label, label);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let texts = table.intern(TypeKind::Array(text));
    let join_all = table.function(
        vec![
            savitar_types::Parameter {
                type_id: text,
                rest: false,
            },
            savitar_types::Parameter {
                type_id: texts,
                rest: true,
            },
        ],
        number,
    );
    let provider = ByName::new(
        source,
        vec![
            ("label", Answer::Known(text)),
            ("joinAll", Answer::Known(join_all)),
        ],
    );
    let file = SourceFile::new("call.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let Some(Statement::Assign {
        value: Rvalue::Call { arguments, .. },
        ..
    }) = only_block(function).statements.iter().find(|statement| {
        matches!(
            statement,
            Statement::Assign {
                value: Rvalue::Call { .. },
                ..
            }
        )
    })
    else {
        panic!("expected a call");
    };
    assert_eq!(arguments.len(), 3);
}

#[test]
fn a_rest_argument_the_element_type_does_not_admit_is_refused() {
    let source = "function call(label: string, count: number): number {\n  return joinAll(label, count);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let texts = table.intern(TypeKind::Array(text));
    let join_all = table.function(
        vec![
            savitar_types::Parameter {
                type_id: text,
                rest: false,
            },
            savitar_types::Parameter {
                type_id: texts,
                rest: true,
            },
        ],
        number,
    );
    let provider = ByName::new(
        source,
        vec![
            ("label", Answer::Known(text)),
            ("count", Answer::Known(number)),
            ("joinAll", Answer::Known(join_all)),
        ],
    );
    let file = SourceFile::new("call.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-argument");
}

#[test]
fn an_empty_array_literal_takes_its_element_type_from_the_declaration() {
    let source = "function collect(): string[] {\n  const out: string[] = [];\n  return out;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let array = table.intern(TypeKind::Array(text));
    let provider = ByName::new(
        source,
        vec![
            ("collect", returning(&mut table, array)),
            ("out", Answer::Known(array)),
        ],
    );
    let file = SourceFile::new("collect.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let entry = function.entry().expect("a block");
    let Terminator::Return(Some(returned)) = function.block(entry).terminator else {
        panic!("expected a return");
    };
    assert_eq!(
        table.kind(function.type_of(returned)),
        &TypeKind::Array(text)
    );
}

#[test]
fn an_empty_array_literal_with_nothing_declaring_it_is_refused() {
    let source = "function collect(): void {\n  const out = [];\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("collect.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics[0]
            .message
            .contains("nothing declares it"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_template_literal_turns_every_hole_into_text() {
    let source =
        "function label(name: string, count: number): string {\n  return `${name}: ${count}`;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("label", returning(&mut table, text)),
            ("name", Answer::Known(text)),
            ("count", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("label.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let block = function.block(savitar_ir::BlockId::from_index(0));
    let Some(Statement::Assign {
        value: Rvalue::Concat(parts),
        target,
        ..
    }) = block.statements.last()
    else {
        panic!("the template should end in a concatenation");
    };
    assert_eq!(parts.len(), 3, "two holes and the text between them");
    assert_eq!(function.type_of(*target), text);
}

#[test]
fn a_template_literal_without_holes_is_a_text_constant() {
    let source = "function fixed(): string {\n  return `ready`;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(source, vec![("fixed", returning(&mut table, text))]);
    let file = SourceFile::new("fixed.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let block = function.block(savitar_ir::BlockId::from_index(0));
    let constants: Vec<&str> = block
        .statements
        .iter()
        .filter_map(|statement| match statement {
            Statement::Assign {
                value: Rvalue::Constant(savitar_ir::Constant::String(held)),
                ..
            } => Some(held.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        constants,
        vec!["ready"],
        "a template with no holes is one constant, got {:?}",
        block.statements
    );
}

#[test]
fn a_template_literal_refuses_a_hole_that_is_not_a_primitive() {
    let source = "function label(point: { x: number }): string {\n  return `${point}`;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = table.object(vec![Field {
        name: "x".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let provider = ByName::new(source, vec![("point", Answer::Known(shape))]);
    let file = SourceFile::new("label.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics[0].message.contains("into text"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_or_carries_both_sides_into_the_join() {
    let source =
        "function fallback(left: string, right: string): string {\n  return left || right;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("fallback", returning(&mut table, text)),
            ("left", Answer::Known(text)),
            ("right", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("fallback.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let joins: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .collect();
    assert_eq!(joins.len(), 1);
    assert_eq!(function.type_of(joins[0].1.parameters[0]), text);
    let Terminator::Return(Some(returned)) = joins[0].1.terminator else {
        panic!("the join should return the chosen side");
    };
    assert_eq!(returned, joins[0].1.parameters[0]);
}

#[test]
fn an_and_only_reaches_the_right_side_when_the_left_holds() {
    let source = "function both(flag: boolean, right: string): boolean | string {\n  return flag && right;\n}\n";
    let mut table = TypeTable::new();
    let boolean = table.intern(TypeKind::Boolean);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("flag", Answer::Known(boolean)),
            ("right", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("both.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let merged = table.union(vec![boolean, text]);
    let joins: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .collect();
    assert_eq!(function.type_of(joins[0].1.parameters[0]), merged);

    let branches = function
        .blocks()
        .filter(|(_, block)| matches!(block.terminator, Terminator::Branch { .. }))
        .count();
    assert_eq!(branches, 1, "the right side must sit behind a branch");
}

#[test]
fn a_coalesce_drops_the_nullish_side() {
    let source = "function orZero(maybe: number | undefined): number {\n  return maybe ?? 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let undefined = table.intern(TypeKind::Undefined);
    let maybe = table.union(vec![number, undefined]);
    let provider = ByName::new(source, vec![("maybe", Answer::Known(maybe))]);
    let file = SourceFile::new("orZero.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let joins: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .collect();
    assert_eq!(
        function.type_of(joins[0].1.parameters[0]),
        number,
        "undefined is gone from both paths"
    );

    let nullish = function
        .blocks()
        .flat_map(|(_, block)| block.statements.clone())
        .filter(|statement| {
            matches!(
                statement,
                Statement::Assign {
                    value: Rvalue::Unary {
                        op: savitar_ir::UnaryOp::IsNullish,
                        ..
                    },
                    ..
                }
            )
        })
        .count();
    assert_eq!(nullish, 1, "the test is for null and undefined, not truth");
}

#[test]
fn an_annotated_declaration_holds_the_declared_type() {
    let source = "function hold(): void {\n  const kept: number | undefined = 1;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let undefined = table.intern(TypeKind::Undefined);
    let maybe = table.union(vec![number, undefined]);
    let provider = ByName::new(source, vec![("kept", Answer::Known(maybe))]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let block = function.block(savitar_ir::BlockId::from_index(0));
    let Some(Statement::Assign { target, .. }) = block.statements.last() else {
        panic!("expected the declaration to assign");
    };
    assert_eq!(function.type_of(*target), maybe);
}

#[test]
fn an_object_literal_widens_its_fields_to_fit_the_declared_shape() {
    let source = "function hold(): void {\n  const kept: Point = { x: 1 };\n}\n";
    let mut table = TypeTable::new();
    let point = point_shape(&mut table);
    let provider = ByName::new(source, vec![("kept", Answer::Known(point))]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let Some(Statement::Assign { target, .. }) = only_block(function).statements.last() else {
        panic!("expected the declaration to assign");
    };
    assert_eq!(function.type_of(*target), point);
}

#[test]
fn an_object_literal_with_no_declared_shape_still_widens_its_fields() {
    let source = "function hold(): void {\n  const kept = { x: 1 };\n}\n";
    let mut table = TypeTable::new();
    let point = point_shape(&mut table);
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let Some(Statement::Assign { target, .. }) = only_block(function).statements.last() else {
        panic!("expected the declaration to assign");
    };
    assert_eq!(
        function.type_of(*target),
        point,
        "a field of an object literal holds the widened type, as it does in TypeScript"
    );
}

#[test]
fn an_object_literal_keeps_a_literal_field_the_declaration_asks_for() {
    let source = "function hold(): void {\n  const kept: Tagged = { kind: \"ok\" };\n}\n";
    let mut table = TypeTable::new();
    let ok = table.intern(TypeKind::StringLiteral("ok".to_string()));
    let tagged = table.object(vec![Field {
        name: "kind".to_string(),
        type_id: ok,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let provider = ByName::new(source, vec![("kept", Answer::Known(tagged))]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let Some(Statement::Assign { target, .. }) = only_block(function).statements.last() else {
        panic!("expected the declaration to assign");
    };
    assert_eq!(function.type_of(*target), tagged);
}

#[test]
fn an_array_literal_widens_its_elements_to_fit_the_declared_array() {
    let source = "function hold(): void {\n  const kept: number[] = [1, 2];\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let numbers = table.intern(TypeKind::Array(number));
    let provider = ByName::new(source, vec![("kept", Answer::Known(numbers))]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let Some(Statement::Assign { target, .. }) = only_block(function).statements.last() else {
        panic!("expected the declaration to assign");
    };
    assert_eq!(function.type_of(*target), numbers);
}

#[test]
fn an_array_literal_with_no_declared_type_still_widens_its_elements() {
    let source = "function hold(): void {\n  const kept = [1, 2];\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let numbers = table.intern(TypeKind::Array(number));
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let Some(Statement::Assign { target, .. }) = only_block(function).statements.last() else {
        panic!("expected the declaration to assign");
    };
    assert_eq!(function.type_of(*target), numbers);
}

#[test]
fn a_declaration_refuses_a_value_the_declared_type_does_not_admit() {
    let source = "function hold(): void {\n  const kept: number = \"hi\";\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("kept", Answer::Known(number))]);
    let file = SourceFile::new("hold.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics[0]
            .message
            .contains("fits the declared type"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_member_the_shape_does_not_declare_is_asked_of_the_provider() {
    let source = "function size(items: number[]): number {\n  return items.length;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let items = table.intern(TypeKind::Array(number));
    let provider = ByName::new(
        source,
        vec![
            ("size", returning(&mut table, number)),
            ("items", Answer::Known(items)),
            ("length", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("size.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let block = function.block(savitar_ir::BlockId::from_index(0));
    let Some(Statement::Assign {
        value: Rvalue::ReadField { name, .. },
        target,
        ..
    }) = block.statements.last()
    else {
        panic!("expected a field read, got {:?}", block.statements);
    };
    assert_eq!(name, "length");
    assert_eq!(function.type_of(*target), number);
}

#[test]
fn a_member_no_one_can_type_is_refused() {
    let source = "function size(items: number[]): number {\n  return items.length;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let items = table.intern(TypeKind::Array(number));
    let provider = ByName::new(source, vec![("items", Answer::Known(items))]);
    let file = SourceFile::new("size.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics[0].message.contains("no declared shape"),
        "{}",
        report.diagnostics[0].message
    );
}

fn point_shape(table: &mut TypeTable) -> savitar_types::TypeId {
    let number = table.intern(TypeKind::Number);
    table.object(vec![Field {
        name: "x".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: false,
    }])
}

fn only_block(function: &savitar_ir::Function) -> &savitar_ir::Block {
    function.block(function.entry().expect("a block"))
}

#[test]
fn writing_a_field_the_shape_declares_lowers_to_a_field_write() {
    let source = "function bump(point: Point): void {\n  point.x = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let point = point_shape(&mut table);
    let provider = ByName::new(source, vec![("point", Answer::Known(point))]);
    let file = SourceFile::new("bump.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let Some(Statement::WriteField { name, value, .. }) = only_block(function).statements.last()
    else {
        panic!(
            "expected a field write, got {:?}",
            only_block(function).statements
        );
    };
    assert_eq!(name, "x");
    assert_eq!(function.type_of(*value), number);
}

#[test]
fn writing_a_field_the_shape_does_not_declare_is_refused() {
    let source = "function bump(point: Point): void {\n  point.z = 2;\n}\n";
    let mut table = TypeTable::new();
    let point = point_shape(&mut table);
    let provider = ByName::new(source, vec![("point", Answer::Known(point))]);
    let file = SourceFile::new("bump.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0].message.contains("no field `z`"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn writing_a_value_the_field_does_not_hold_is_refused() {
    let source = "function bump(point: Point, label: string): void {\n  point.x = label;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let point = point_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("point", Answer::Known(point)),
            ("label", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("bump.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-assignment");
}

#[test]
fn writing_a_field_of_something_with_no_shape_asks_the_provider_for_the_slot() {
    let source = "function bump(box: Box): void {\n  box.count = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boxed = table.intern(TypeKind::Unknown);
    let provider = ByName::new(
        source,
        vec![
            ("box", Answer::Known(boxed)),
            ("count", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("bump.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let Some(Statement::WriteField { name, value, .. }) = only_block(function).statements.last()
    else {
        panic!(
            "expected a field write, got {:?}",
            only_block(function).statements
        );
    };
    assert_eq!(name, "count");
    assert_eq!(function.type_of(*value), number);
}

#[test]
fn writing_a_field_no_one_can_type_is_refused() {
    let source = "function bump(box: Box): void {\n  box.count = 2;\n}\n";
    let mut table = TypeTable::new();
    let boxed = table.intern(TypeKind::Unknown);
    let provider = ByName::new(source, vec![("box", Answer::Known(boxed))]);
    let file = SourceFile::new("bump.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0].message.contains("no declared shape"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn writing_an_element_takes_the_element_type_not_the_optional_of_a_read() {
    let source = "function fill(items: number[]): void {\n  items[0] = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let items = table.intern(TypeKind::Array(number));
    let provider = ByName::new(source, vec![("items", Answer::Known(items))]);
    let file = SourceFile::new("fill.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let Some(Statement::WriteElement { value, .. }) = only_block(function).statements.last() else {
        panic!(
            "expected an element write, got {:?}",
            only_block(function).statements
        );
    };
    assert_eq!(function.type_of(*value), number);
    assert!(
        !table.admits_undefined(function.type_of(*value)),
        "a write takes the element type, while only a read may yield undefined"
    );
}

#[test]
fn writing_through_a_readonly_array_is_refused() {
    let source = "function fill(items: readonly number[]): void {\n  items[0] = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let items = table.intern(TypeKind::ReadonlyArray(number));
    let provider = ByName::new(source, vec![("items", Answer::Known(items))]);
    let file = SourceFile::new("fill.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0].message.contains("readonly array"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn writing_a_field_the_shape_declares_readonly_is_refused() {
    let source = "function bump(point: Frozen): void {\n  point.x = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let frozen = table.object(vec![Field {
        name: "x".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: true,
    }]);
    let provider = ByName::new(source, vec![("point", Answer::Known(frozen))]);
    let file = SourceFile::new("bump.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0].message.contains("readonly"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn reading_a_field_the_shape_declares_readonly_is_still_lowered() {
    let source = "function peek(point: Frozen): number {\n  return point.x;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let frozen = table.object(vec![Field {
        name: "x".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: true,
    }]);
    let provider = ByName::new(source, vec![("point", Answer::Known(frozen))]);
    let file = SourceFile::new("peek.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
}

#[test]
fn writing_through_a_readonly_index_signature_is_refused() {
    let source = "function fill(out: Frozen, key: string): void {\n  out[key] = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let out = table.object_with_indexes(
        Vec::new(),
        vec![IndexSignature {
            key: text,
            value: number,
            readonly: true,
        }],
    );
    let provider = ByName::new(
        source,
        vec![("out", Answer::Known(out)), ("key", Answer::Known(text))],
    );
    let file = SourceFile::new("fill.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0]
            .message
            .contains("readonly index signature"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn writing_through_an_index_signature_takes_the_value_the_signature_declares() {
    let source =
        "function fill(out: Record<string, number>, key: string): void {\n  out[key] = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let out = table.object_with_indexes(
        Vec::new(),
        vec![IndexSignature {
            key: text,
            value: number,
            readonly: false,
        }],
    );
    let provider = ByName::new(
        source,
        vec![("out", Answer::Known(out)), ("key", Answer::Known(text))],
    );
    let file = SourceFile::new("fill.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let Some(Statement::WriteElement { value, .. }) = only_block(function).statements.last() else {
        panic!(
            "expected an element write, got {:?}",
            only_block(function).statements
        );
    };
    assert_eq!(function.type_of(*value), number);
}

#[test]
fn writing_a_key_no_index_signature_accepts_is_refused() {
    let source =
        "function fill(out: Record<string, number>, at: number): void {\n  out[at] = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let out = table.object_with_indexes(
        Vec::new(),
        vec![IndexSignature {
            key: text,
            value: number,
            readonly: false,
        }],
    );
    let provider = ByName::new(
        source,
        vec![("out", Answer::Known(out)), ("at", Answer::Known(number))],
    );
    let file = SourceFile::new("fill.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0].message.contains("no index signature"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn writing_to_a_tuple_at_a_literal_index_takes_that_position() {
    let source = "function set(pair: [number, string]): void {\n  pair[1] = \"ok\";\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![number, text], false);
    let provider = ByName::new(source, vec![("pair", Answer::Known(pair))]);
    let file = SourceFile::new("set.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let Some(Statement::WriteElement { value, .. }) = only_block(function).statements.last() else {
        panic!(
            "expected an element write, got {:?}",
            only_block(function).statements
        );
    };
    assert_eq!(function.type_of(*value), text);
}

#[test]
fn writing_to_a_tuple_at_an_index_no_one_knows_is_refused() {
    let source = "function set(pair: [number, string], at: number): void {\n  pair[at] = 1;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![number, text], false);
    let provider = ByName::new(
        source,
        vec![("pair", Answer::Known(pair)), ("at", Answer::Known(number))],
    );
    let file = SourceFile::new("set.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
}

#[test]
fn writing_to_a_readonly_tuple_is_refused() {
    let source = "function set(pair: readonly [number, string]): void {\n  pair[0] = 1;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![number, text], true);
    let provider = ByName::new(source, vec![("pair", Answer::Known(pair))]);
    let file = SourceFile::new("set.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0].message.contains("readonly tuple"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn writing_past_the_end_of_a_tuple_is_refused() {
    let source = "function set(pair: [number, string]): void {\n  pair[2] = 1;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![number, text], false);
    let provider = ByName::new(source, vec![("pair", Answer::Known(pair))]);
    let file = SourceFile::new("set.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics[0].message.contains("outside the tuple"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_compound_write_to_a_field_reads_then_combines_then_writes() {
    let source = "function bump(point: Point): void {\n  point.x += 1;\n}\n";
    let mut table = TypeTable::new();
    let point = point_shape(&mut table);
    let provider = ByName::new(source, vec![("point", Answer::Known(point))]);
    let file = SourceFile::new("bump.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let statements = &only_block(function).statements;
    let read = statements
        .iter()
        .position(|statement| {
            matches!(
                statement,
                Statement::Assign {
                    value: Rvalue::ReadField { .. },
                    ..
                }
            )
        })
        .expect("the compound write should read the field first");
    let combined = statements
        .iter()
        .position(|statement| {
            matches!(
                statement,
                Statement::Assign {
                    value: Rvalue::Binary {
                        op: BinaryOp::Add,
                        ..
                    },
                    ..
                }
            )
        })
        .expect("the compound write should add");
    let written = statements
        .iter()
        .position(|statement| matches!(statement, Statement::WriteField { .. }))
        .expect("the compound write should write the field");
    assert!(read < combined && combined < written);
}

#[test]
fn an_increment_of_a_field_steps_through_a_read_and_a_write() {
    let source = "function bump(point: Point): void {\n  point.x++;\n}\n";
    let mut table = TypeTable::new();
    let point = point_shape(&mut table);
    let provider = ByName::new(source, vec![("point", Answer::Known(point))]);
    let file = SourceFile::new("bump.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let statements = &only_block(function).statements;
    assert!(statements.iter().any(|statement| matches!(
        statement,
        Statement::Assign {
            value: Rvalue::ReadField { .. },
            ..
        }
    )));
    assert!(matches!(
        statements.last(),
        Some(Statement::WriteField { .. })
    ));
}

#[test]
fn a_compound_write_to_an_element_is_refused_because_the_read_may_be_undefined() {
    let source = "function bump(items: number[]): void {\n  items[0] += 1;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let items = table.intern(TypeKind::Array(number));
    let provider = ByName::new(source, vec![("items", Answer::Known(items))]);
    let file = SourceFile::new("bump.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-operator");
    assert!(
        report.diagnostics[0].message.contains("two numbers"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn the_object_and_the_index_are_evaluated_before_the_value() {
    let source = "function fill(items: number[]): void {\n  items[next()] = value();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let items = table.intern(TypeKind::Array(number));
    let giving_number = table.function(Vec::new(), number);
    let provider = ByName::new(
        source,
        vec![
            ("items", Answer::Known(items)),
            ("next", Answer::Known(giving_number)),
            ("value", Answer::Known(giving_number)),
        ],
    );
    let file = SourceFile::new("fill.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let statements = &only_block(function).statements;
    let position_of = |wanted: &str| {
        statements
            .iter()
            .position(|statement| match statement {
                Statement::Assign {
                    value: Rvalue::Global(name),
                    ..
                } => name == wanted,
                _ => false,
            })
            .unwrap_or_else(|| panic!("`{wanted}` should be read as a global"))
    };
    assert!(position_of("next") < position_of("value"));
    assert!(matches!(
        statements.last(),
        Some(Statement::WriteElement { .. })
    ));
}

#[test]
fn an_assignment_to_a_pattern_is_refused_rather_than_guessed() {
    let source = "function take(point: Point): void {\n  [point.x] = [1];\n}\n";
    let mut table = TypeTable::new();
    let point = point_shape(&mut table);
    let provider = ByName::new(source, vec![("point", Answer::Known(point))]);
    let file = SourceFile::new("take.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-statement");
}

#[test]
fn writing_a_field_an_index_signature_would_cover_is_still_refused() {
    let source = "function fill(out: Record<string, number>): void {\n  out.hits = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let out = table.object_with_indexes(
        Vec::new(),
        vec![IndexSignature {
            key: text,
            value: number,
            readonly: false,
        }],
    );
    let provider = ByName::new(source, vec![("out", Answer::Known(out))]);
    let file = SourceFile::new("fill.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0].message.contains("declares no field"),
        "{}",
        report.diagnostics[0].message
    );
}

fn outcome_union(table: &mut TypeTable) -> savitar_types::TypeId {
    let text = table.intern(TypeKind::String);
    let number = table.intern(TypeKind::Number);
    let ok_kind = table.intern(TypeKind::StringLiteral("ok".to_string()));
    let error_kind = table.intern(TypeKind::StringLiteral("error".to_string()));
    let ok = table.object(vec![
        Field {
            name: "kind".to_string(),
            type_id: ok_kind,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "body".to_string(),
            type_id: text,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    let failed = table.object(vec![
        Field {
            name: "kind".to_string(),
            type_id: error_kind,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "detail".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    table.union(vec![ok, failed])
}

fn equality_tests(function: &savitar_ir::Function) -> usize {
    function
        .blocks()
        .flat_map(|(_, block)| block.statements.iter())
        .filter(|statement| {
            matches!(
                statement,
                Statement::Assign {
                    value: Rvalue::Binary {
                        op: BinaryOp::StrictEqual,
                        ..
                    },
                    ..
                }
            )
        })
        .count()
}

fn unreachable_terminators(function: &savitar_ir::Function) -> usize {
    let dominance = savitar_ir::Dominance::of(function);
    function
        .blocks()
        .filter(|(id, block)| {
            dominance.reachable(*id) && block.terminator == Terminator::Unreachable
        })
        .count()
}

fn returns_without_a_value(function: &savitar_ir::Function) -> usize {
    function
        .blocks()
        .filter(|(_, block)| block.terminator == Terminator::Return(None))
        .count()
}

#[test]
fn a_switch_over_a_discriminant_becomes_a_chain_of_strict_equality_tests() {
    let source = "function describe(outcome: Outcome): string | number {\n  switch (outcome.kind) {\n    case \"ok\":\n      return outcome.body;\n    case \"error\":\n      return outcome.detail;\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let number = table.intern(TypeKind::Number);
    let outcome = outcome_union(&mut table);
    let ok_kind = table.intern(TypeKind::StringLiteral("ok".to_string()));
    let error_kind = table.intern(TypeKind::StringLiteral("error".to_string()));
    let discriminant = table.union(vec![ok_kind, error_kind]);
    let either = table.union(vec![text, number]);
    let provider = ByName::new(
        source,
        vec![
            ("describe", returning(&mut table, either)),
            ("outcome", Answer::Known(outcome)),
            ("kind", Answer::Known(discriminant)),
            ("body", Answer::Known(text)),
            ("detail", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("describe.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        equality_tests(function),
        2,
        "one strict equality per case label"
    );

    let read: Vec<_> = function
        .blocks()
        .flat_map(|(_, block)| block.statements.iter())
        .filter_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::ReadField { name, .. },
                ..
            } if name != "kind" => Some(function.type_of(*target)),
            _ => None,
        })
        .collect();
    assert_eq!(
        read,
        vec![text, number],
        "each case reads the field of the member it matched"
    );
    let returned: Vec<_> = function
        .blocks()
        .filter_map(|(_, block)| match block.terminator {
            Terminator::Return(Some(value)) => Some(function.type_of(value)),
            _ => None,
        })
        .collect();
    assert_eq!(
        returned,
        vec![either, either],
        "each return carries the declared type, widened where the branch was narrower"
    );
}

#[test]
fn a_switch_with_no_default_reaches_the_join_on_the_path_no_case_matched() {
    let source = "function describe(outcome: Outcome): string {\n  switch (outcome.kind) {\n    case \"ok\":\n      return outcome.body;\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let outcome = outcome_union(&mut table);
    let ok_kind = table.intern(TypeKind::StringLiteral("ok".to_string()));
    let error_kind = table.intern(TypeKind::StringLiteral("error".to_string()));
    let discriminant = table.union(vec![ok_kind, error_kind]);
    let provider = ByName::new(
        source,
        vec![
            ("outcome", Answer::Known(outcome)),
            ("kind", Answer::Known(discriminant)),
            ("body", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("describe.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        returns_without_a_value(function),
        1,
        "`error` has no case, so the unmatched path falls out of the function with no value"
    );
}

#[test]
fn a_switch_whose_labels_cover_the_union_leaves_no_unmatched_path() {
    let source = "function describe(outcome: Outcome): string {\n  switch (outcome.kind) {\n    case \"ok\":\n      return outcome.body;\n    case \"error\":\n      return outcome.detail;\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let number = table.intern(TypeKind::Number);
    let outcome = outcome_union(&mut table);
    let ok_kind = table.intern(TypeKind::StringLiteral("ok".to_string()));
    let error_kind = table.intern(TypeKind::StringLiteral("error".to_string()));
    let discriminant = table.union(vec![ok_kind, error_kind]);
    let provider = ByName::new(
        source,
        vec![
            ("outcome", Answer::Known(outcome)),
            ("kind", Answer::Known(discriminant)),
            ("body", Answer::Known(text)),
            ("detail", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("describe.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        returns_without_a_value(function),
        0,
        "every member of the union has a case, so no path leaves the function without a value"
    );
    assert_eq!(
        unreachable_terminators(function),
        1,
        "the path no case matched is marked unreachable instead of joining"
    );
}

#[test]
fn a_switch_over_text_is_never_exhaustive_however_many_labels_it_has() {
    let source = "function describe(kind: string): string {\n  switch (kind) {\n    case \"ok\":\n      return kind;\n    case \"error\":\n      return kind;\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(source, vec![("kind", Answer::Known(text))]);
    let file = SourceFile::new("describe.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        returns_without_a_value(function),
        1,
        "`string` holds values no label names, so the unmatched path stays"
    );
}

#[test]
fn a_switch_over_a_boolean_is_exhaustive_once_both_labels_appear() {
    let source = "function pick(flag: boolean, value: number): number {\n  switch (flag) {\n    case true:\n      return value;\n    case false:\n      return value;\n  }\n}\n";
    let mut table = TypeTable::new();
    let boolean = table.intern(TypeKind::Boolean);
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("flag", Answer::Known(boolean)),
            ("value", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(returns_without_a_value(function), 0);
}

#[test]
fn a_switch_whose_every_clause_returns_leaves_the_join_stranded() {
    let source = "function always(kind: string, value: number): number {\n  switch (kind) {\n    case \"a\":\n      return value;\n    default:\n      return value;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("kind", Answer::Known(text)),
            ("value", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("always.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        returns_without_a_value(function),
        0,
        "the default covers every path, so nothing returns without a value"
    );
    let dominance = savitar_ir::Dominance::of(function);
    let unreachable = function
        .blocks()
        .filter(|(id, _)| !dominance.reachable(*id))
        .count();
    assert_eq!(unreachable, 1, "the join is left stranded, not deleted");
}

#[test]
fn a_name_a_break_changed_arrives_at_the_join_as_a_parameter() {
    let source = "function size(kind: string, small: number, large: number): number {\n  let chosen = small;\n  switch (kind) {\n    case \"large\":\n      chosen = large;\n      break;\n    default:\n      chosen = small;\n  }\n  return chosen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("size", returning(&mut table, number)),
            ("kind", Answer::Known(text)),
            ("small", Answer::Known(number)),
            ("large", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("size.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let joins: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .collect();
    assert_eq!(joins.len(), 1, "exactly one join should take a parameter");
    let (join, block) = &joins[0];
    assert_eq!(block.parameters.len(), 1);
    assert_eq!(function.type_of(block.parameters[0]), number);
    assert_eq!(
        block.terminator,
        Terminator::Return(Some(block.parameters[0]))
    );

    let arguments: Vec<usize> = function
        .blocks()
        .filter_map(|(_, candidate)| match &candidate.terminator {
            Terminator::Jump { target, arguments } if target == join => Some(arguments.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        arguments,
        vec![1, 1],
        "the break and the clause that runs out both pass one argument"
    );
}

#[test]
fn a_case_with_no_statements_shares_the_body_of_the_next_one() {
    let source = "function rank(kind: string): number {\n  switch (kind) {\n    case \"a\":\n    case \"b\":\n      return 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(source, vec![("kind", Answer::Known(text))]);
    let file = SourceFile::new("rank.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(equality_tests(function), 2);

    let bodies = function
        .blocks()
        .flat_map(|(_, block)| block.statements.iter())
        .filter(|statement| {
            matches!(
                statement,
                Statement::Assign {
                    value: Rvalue::Constant(savitar_ir::Constant::Number(bits)),
                    ..
                } if *bits == 1.0f64.to_bits()
            )
        })
        .count();
    assert_eq!(bodies, 1, "both labels reach one body, not a copy each");
}

#[test]
fn a_case_that_runs_into_the_next_one_is_refused_rather_than_flattened() {
    let source = "function pick(kind: string, value: number): number {\n  switch (kind) {\n    case \"a\":\n      value = 1;\n    case \"b\":\n      return value;\n  }\n  return value;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("kind", Answer::Known(text)),
            ("value", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-statement");
    assert!(
        report.diagnostics[0]
            .message
            .contains("runs into the next one"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_case_label_no_one_can_type_is_refused_rather_than_guessed() {
    let source = "function rank(kind: string): number {\n  switch (kind) {\n    case missing:\n      return 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(source, vec![("kind", Answer::Known(text))]);
    let file = SourceFile::new("rank.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "no-type-answer");
}

#[test]
fn a_break_inside_a_loop_is_refused_instead_of_leaving_the_loop_wrong() {
    let source = "function first(values: readonly number[], start: number): number {\n  let found = start;\n  for (const value of values) {\n    found = value;\n    break;\n  }\n  return found;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let values = table.intern(TypeKind::ReadonlyArray(number));
    let provider = ByName::new(
        source,
        vec![
            ("values", Answer::Known(values)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("first.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-statement");
    assert!(
        report.diagnostics[0].message.contains("out of a loop"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_break_in_one_arm_of_an_if_inside_a_case_still_reaches_the_join() {
    let source = "function size(kind: string, small: number, large: number): number {\n  let chosen = small;\n  switch (kind) {\n    case \"large\":\n      if (large > small) {\n        chosen = large;\n        break;\n      }\n      chosen = small;\n      break;\n    default:\n      chosen = large;\n  }\n  return chosen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("kind", Answer::Known(text)),
            ("small", Answer::Known(number)),
            ("large", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("size.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let joins: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .collect();
    assert_eq!(joins.len(), 1, "only the switch join takes a parameter");
    let (join, _) = &joins[0];
    let arguments: Vec<usize> = function
        .blocks()
        .filter_map(|(_, candidate)| match &candidate.terminator {
            Terminator::Jump { target, arguments } if target == join => Some(arguments.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        arguments,
        vec![1, 1, 1],
        "two breaks and the default all carry the name"
    );
}

#[test]
fn a_switch_inside_a_loop_carries_the_name_it_changed_through_the_header() {
    let source = "function total(kind: string, step: number): number {\n  let sum = step;\n  while (sum < step) {\n    switch (kind) {\n      case \"add\":\n        sum = sum + step;\n        break;\n      default:\n        sum = step;\n    }\n  }\n  return sum;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("kind", Answer::Known(text)),
            ("step", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("total.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let headers = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .count();
    assert_eq!(
        headers, 2,
        "the loop header and the switch join each take the name"
    );
}

#[test]
fn a_default_clause_before_the_last_case_still_takes_the_unmatched_path() {
    let source = "function pick(kind: string, small: number, large: number): number {\n  switch (kind) {\n    default:\n      return small;\n    case \"large\":\n      return large;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("kind", Answer::Known(text)),
            ("small", Answer::Known(number)),
            ("large", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(returns_without_a_value(function), 0);
}

#[test]
fn a_break_in_a_nested_switch_leaves_only_the_switch_around_it() {
    let source = "function label(outer: string, inner: string, first: number, second: number): number {\n  let chosen = first;\n  switch (outer) {\n    case \"x\":\n      switch (inner) {\n        case \"y\":\n          chosen = second;\n          break;\n        default:\n          chosen = first;\n      }\n      break;\n    default:\n      chosen = second;\n  }\n  return chosen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("outer", Answer::Known(text)),
            ("inner", Answer::Known(text)),
            ("first", Answer::Known(number)),
            ("second", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("label.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let joins: Vec<_> = function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .map(|(id, _)| id)
        .collect();
    assert_eq!(joins.len(), 2, "each switch keeps a join of its own");
    let incoming: Vec<usize> = joins
        .iter()
        .map(|join| {
            function
                .blocks()
                .filter(|(_, block)| {
                    matches!(&block.terminator, Terminator::Jump { target, .. } if target == join)
                })
                .count()
        })
        .collect();
    assert_eq!(
        incoming,
        vec![2, 2],
        "the inner break feeds the inner join, not the outer one"
    );
}
