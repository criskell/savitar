use std::cell::RefCell;

use savitar_diagnostics::{SourceFile, Span};
use savitar_ir::{BinaryOp, Rvalue, Statement, Terminator, verify};
use savitar_lower::lower;
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_types::{Field, TypeKind, TypeTable};

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

impl TypeProvider for ByName {
    fn name(&self) -> &'static str {
        "by-name"
    }

    fn type_at(&self, _table: &mut TypeTable, _file: &str, span: Span) -> Answer {
        let at = span.start as usize;
        for (name, answer) in self.answers.borrow().iter() {
            let ends_here = self.source[..=at.min(self.source.len() - 1)].ends_with(name.as_str());
            if ends_here {
                return answer.clone();
            }
        }
        Answer::OutOfScope
    }
}

const ADD: &str = "function add(left: number, right: number): number {\n  const sum = left + right;\n  return sum;\n}\n";

#[test]
fn a_straight_line_function_lowers_to_valid_code() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        ADD,
        vec![
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
    let provider = ByName::new(source, vec![("name", Answer::Known(text))]);
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
fn a_name_that_leaves_the_branches_with_two_types_is_refused() {
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

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics[0]
            .message
            .contains("two different types"),
        "{}",
        report.diagnostics[0].message
    );
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
    let helper = table.function(vec![savitar_types::Parameter { type_id: number }], number);
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
fn an_empty_array_literal_takes_its_element_type_from_the_declaration() {
    let source = "function collect(): string[] {\n  const out: string[] = [];\n  return out;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let array = table.intern(TypeKind::Array(text));
    let provider = ByName::new(source, vec![("out", Answer::Known(array))]);
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
