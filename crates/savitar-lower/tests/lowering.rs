use std::cell::RefCell;

use savitar_diagnostics::{SourceFile, Span};

use savitar_ir::{BinaryOp, Constant, Rvalue, Statement, Terminator, verify};

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
    let source = "function loops(limit: number): number {\n  throw limit;\n}\n";
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
fn writing_text_to_a_name_that_holds_a_number_is_refused_instead_of_retyping_the_name() {
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
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-assignment");
    assert!(
        report.diagnostics[0].message.contains("`held` holds"),
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

    let carried: Vec<usize> = parameterised(function)
        .iter()
        .map(|(_, count)| *count)
        .collect();
    assert_eq!(
        carried,
        vec![2, 2],
        "the header and the latch that steps the index both carry the index and the running total"
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
            optional: false,
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
fn an_await_takes_what_the_promise_holds() {
    let source = "async function body(route: string): Promise<string> {\n  const held = await fetchOne(route);\n  return held;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let promised = table.intern(TypeKind::Promise(text));
    let fetch_one = table.function(
        vec![savitar_types::Parameter {
            type_id: text,
            rest: false,
            optional: false,
        }],
        promised,
    );
    let provider = ByName::new(
        source,
        vec![
            ("body", returning(&mut table, promised)),
            ("route", Answer::Known(text)),
            ("fetchOne", Answer::Known(fetch_one)),
        ],
    );
    let file = SourceFile::new("body.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let awaited = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::Await(_),
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the await should be lowered");
    assert_eq!(function.type_of(awaited), text);
}

#[test]
fn an_await_of_something_that_is_not_a_promise_is_refused() {
    let source = "async function body(route: string): Promise<string> {\n  const held = await route;\n  return held;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let promised = table.intern(TypeKind::Promise(text));
    let provider = ByName::new(
        source,
        vec![
            ("body", returning(&mut table, promised)),
            ("route", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("body.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-expression");
    assert!(
        report.diagnostics[0].message.contains("not a promise"),
        "{}",
        report.diagnostics[0].message
    );
}

fn overloaded(
    table: &mut TypeTable,
    forms: Vec<(savitar_types::TypeId, savitar_types::TypeId)>,
) -> savitar_types::TypeId {
    let built: Vec<savitar_types::TypeId> = forms
        .into_iter()
        .map(|(takes, returns)| {
            table.function(
                vec![savitar_types::Parameter {
                    type_id: takes,
                    rest: false,
                    optional: false,
                }],
                returns,
            )
        })
        .collect();
    table.overloaded(built)
}

fn maybe_number(table: &mut TypeTable) -> (savitar_types::TypeId, savitar_types::TypeId) {
    let number = table.intern(TypeKind::Number);
    let maybe = table.optional(number);
    (number, maybe)
}

#[test]
fn a_name_tested_against_undefined_is_narrowed_where_it_cannot_be_undefined() {
    let source = "function use(value: number | undefined): number {\n  if (value === undefined) {\n    return 0;\n  }\n  return value + 1;\n}\n";
    let mut table = TypeTable::new();
    let (number, maybe) = maybe_number(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(maybe)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

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
                    value: Rvalue::Narrow(_),
                    ..
                }
            )),
        "the branch that excludes undefined narrows the name"
    );
}

#[test]
fn a_ternary_tested_against_undefined_narrows_the_arm_that_excludes_it() {
    let source = "function use(value: number | undefined): number {\n  return value === undefined ? 0 : value + 1;\n}\n";
    let mut table = TypeTable::new();
    let (number, maybe) = maybe_number(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(maybe)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(verify(&report.functions[0]), vec![]);
}

#[test]
fn a_ternary_tested_the_other_way_round_narrows_the_other_arm() {
    let source = "function use(value: number | undefined): number {\n  return value !== undefined ? value + 1 : 0;\n}\n";
    let mut table = TypeTable::new();
    let (number, maybe) = maybe_number(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(maybe)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn a_name_tested_against_null_is_narrowed_the_same_way() {
    let source = "function use(value: number | null): number {\n  return value === null ? 0 : value + 1;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let null = table.intern(TypeKind::Null);
    let maybe = table.union(vec![number, null]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(maybe)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn the_branch_where_the_name_is_undefined_is_narrowed_to_undefined() {
    let source = "function use(value: number | undefined): number {\n  if (value === undefined) {\n    return value + 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let (number, maybe) = maybe_number(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(maybe)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

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
fn a_new_expression_takes_the_form_that_fits_and_the_type_it_builds() {
    let source = "function make(start: number): Counter {\n  return new Counter(start);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let counter = table.object(vec![Field {
        name: "value".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let form = table.function(
        vec![savitar_types::Parameter {
            type_id: number,
            rest: false,
            optional: false,
        }],
        counter,
    );
    let constructor = table.constructor(vec![form]);
    let provider = ByName::new(
        source,
        vec![
            ("make", returning(&mut table, counter)),
            ("start", Answer::Known(number)),
            ("Counter", Answer::Known(constructor)),
        ],
    );
    let file = SourceFile::new("make.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let built = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::New { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the construction should be lowered");
    assert_eq!(function.type_of(built), counter);
}

#[test]
fn constructing_something_that_declares_no_way_to_build_it_is_refused() {
    let source = "function make(start: number): number {\n  return new Counter(start);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("make", returning(&mut table, number)),
            ("start", Answer::Known(number)),
            ("Counter", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("make.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0]
            .message
            .contains("no way to construct"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_written_type_argument_stands_for_the_type_parameter_the_form_declares() {
    let source = "function make(): Box {\n  return new Holder<number>();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let held = table.fresh_type_parameter();
    let boxed_held = table.object(vec![Field {
        name: "held".to_string(),
        type_id: held,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let form = table.generic_function(vec![held], Vec::new(), boxed_held);
    let constructor = table.constructor(vec![form]);
    let boxed_number = table.object(vec![Field {
        name: "held".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let provider = ByName::new(
        source,
        vec![
            ("make", returning(&mut table, boxed_number)),
            ("Holder", Answer::Known(constructor)),
            ("number", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("make.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    let built = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::New { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the construction should be lowered");
    assert_eq!(
        function.type_of(built),
        boxed_number,
        "the type argument written at the call site binds the type parameter"
    );
}

#[test]
fn two_forms_that_agree_on_the_types_are_not_a_refusal() {
    let source = "function call(label: string): string {\n  return either(label);\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let number = table.intern(TypeKind::Number);
    let first = table.function(
        vec![savitar_types::Parameter {
            type_id: text,
            rest: false,
            optional: false,
        }],
        text,
    );
    let second = table.function(
        vec![
            savitar_types::Parameter {
                type_id: text,
                rest: false,
                optional: false,
            },
            savitar_types::Parameter {
                type_id: number,
                rest: false,
                optional: true,
            },
        ],
        text,
    );
    let either = table.overloaded(vec![first, second]);
    let provider = ByName::new(
        source,
        vec![
            ("call", returning(&mut table, text)),
            ("label", Answer::Known(text)),
            ("either", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("call.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(
        report.diagnostics.is_empty(),
        "both forms type this call the same way, so there is nothing to choose: {:?}",
        report.diagnostics
    );
}

#[test]
fn an_overloaded_call_takes_the_only_form_that_fits() {
    let source = "function call(label: string): string {\n  return either(label);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = overloaded(&mut table, vec![(number, number), (text, text)]);
    let provider = ByName::new(
        source,
        vec![
            ("call", returning(&mut table, text)),
            ("label", Answer::Known(text)),
            ("either", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("call.ts", source);

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
        text,
        "the form whose parameter takes the argument decides the result"
    );
}

#[test]
fn an_overloaded_call_that_fits_no_form_is_refused() {
    let source = "function call(flag: boolean): string {\n  return either(flag);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let boolean = table.intern(TypeKind::Boolean);
    let either = overloaded(&mut table, vec![(number, number), (text, text)]);
    let provider = ByName::new(
        source,
        vec![
            ("call", returning(&mut table, text)),
            ("flag", Answer::Known(boolean)),
            ("either", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("call.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0].message.contains("no declared form"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_overloaded_call_that_fits_two_forms_is_refused_rather_than_picked() {
    let source = "function call(count: number): string {\n  return either(count);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either_type = table.union(vec![number, text]);
    let either = overloaded(&mut table, vec![(number, number), (either_type, text)]);
    let provider = ByName::new(
        source,
        vec![
            ("call", returning(&mut table, text)),
            ("count", Answer::Known(number)),
            ("either", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("call.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0].message.contains("more than one"),
        "{}",
        report.diagnostics[0].message
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
            optional: false,
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
fn a_deferred_indexed_access_is_resolved_once_the_call_binds_both_sides() {
    let source = "function at(route: Route): string {\n  return indexAt(route, \"path\");\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = table.object(vec![Field {
        name: "path".to_string(),
        type_id: text,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let shape = table.fresh_type_parameter();
    let key = table.fresh_type_parameter();
    let reached = table.indexed_access(shape, key);
    let index_at = table.generic_function(
        vec![shape, key],
        vec![
            savitar_types::Parameter {
                type_id: shape,
                rest: false,
                optional: false,
            },
            savitar_types::Parameter {
                type_id: key,
                rest: false,
                optional: false,
            },
        ],
        reached,
    );
    let provider = ByName::new(
        source,
        vec![
            ("at", returning(&mut table, text)),
            ("route", Answer::Known(route)),
            ("indexAt", Answer::Known(index_at)),
        ],
    );
    let file = SourceFile::new("at.ts", source);

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
        text,
        "`T[K]` reaches the field the key names once both sides are bound"
    );
}

#[test]
fn a_deferred_indexed_access_that_reaches_no_field_is_refused() {
    let source = "function at(route: Route): string {\n  return indexAt(route, \"missing\");\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = table.object(vec![Field {
        name: "path".to_string(),
        type_id: text,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let shape = table.fresh_type_parameter();
    let key = table.fresh_type_parameter();
    let reached = table.indexed_access(shape, key);
    let index_at = table.generic_function(
        vec![shape, key],
        vec![
            savitar_types::Parameter {
                type_id: shape,
                rest: false,
                optional: false,
            },
            savitar_types::Parameter {
                type_id: key,
                rest: false,
                optional: false,
            },
        ],
        reached,
    );
    let provider = ByName::new(
        source,
        vec![
            ("at", returning(&mut table, text)),
            ("route", Answer::Known(route)),
            ("indexAt", Answer::Known(index_at)),
        ],
    );
    let file = SourceFile::new("at.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0].message.contains("no field named"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_deferred_conditional_is_decided_once_the_call_binds_its_check() {
    let source = "function name(value: string): string {\n  return nameOf(value);\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let number = table.intern(TypeKind::Number);
    let held = table.fresh_type_parameter();
    let decided = table.conditional(held, text, text, number);
    let name_of = table.generic_function(
        vec![held],
        vec![savitar_types::Parameter {
            type_id: held,
            rest: false,
            optional: false,
        }],
        decided,
    );
    let provider = ByName::new(
        source,
        vec![
            ("name", returning(&mut table, text)),
            ("value", Answer::Known(text)),
            ("nameOf", Answer::Known(name_of)),
        ],
    );
    let file = SourceFile::new("name.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
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
        text,
        "the bound check type fits the bound, so the true branch decides"
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
                optional: false,
            },
            savitar_types::Parameter {
                type_id: held,
                rest: false,
                optional: false,
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
            optional: false,
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
                optional: false,
            },
            savitar_types::Parameter {
                type_id: texts,
                rest: true,
                optional: false,
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
                optional: false,
            },
            savitar_types::Parameter {
                type_id: texts,
                rest: true,
                optional: false,
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
fn an_array_pattern_reads_each_position_and_slices_the_rest() {
    let source = "function split(parts: string[]): string[] {\n  const [head, ...rest] = parts;\n  return rest;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let texts = table.intern(TypeKind::Array(text));
    let provider = ByName::new(
        source,
        vec![
            ("split", returning(&mut table, texts)),
            ("parts", Answer::Known(texts)),
        ],
    );
    let file = SourceFile::new("split.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let read = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::ReadElement { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the first position should be read");
    assert!(
        table.admits_undefined(function.type_of(read)),
        "a read at an index may fall outside the array, so the first name admits undefined"
    );
    let sliced = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::Slice { from, .. },
                ..
            } => Some((*target, *from)),
            _ => None,
        })
        .expect("the rest should be sliced");
    assert_eq!(sliced.1, 1, "the rest starts after the names before it");
    assert_eq!(
        function.type_of(sliced.0),
        texts,
        "the rest is a fresh array of the same element"
    );
}

#[test]
fn an_array_pattern_over_a_tuple_keeps_each_position_exact() {
    let source = "function head(pair: [number, string]): number {\n  const [first, ...rest] = pair;\n  return first;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![number, text], false);
    let provider = ByName::new(
        source,
        vec![
            ("head", returning(&mut table, number)),
            ("pair", Answer::Known(pair)),
        ],
    );
    let file = SourceFile::new("head.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let sliced = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::Slice { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the rest should be sliced");
    let kept = table.tuple(vec![text], false);
    assert_eq!(
        function.type_of(sliced),
        kept,
        "the rest of a tuple keeps the positions that are left"
    );
}

#[test]
fn an_array_pattern_over_something_that_is_not_a_list_is_refused() {
    let source = "function head(count: number): number {\n  const [first, ...rest] = count;\n  return first;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("head", returning(&mut table, number)),
            ("count", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("head.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

#[test]
fn writing_a_field_an_index_signature_covers_takes_the_type_the_signature_declares() {
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

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let entry = function.entry().expect("a block");
    let written = function
        .block(entry)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::WriteField { name, value, .. } => Some((name.clone(), *value)),
            _ => None,
        })
        .expect("the write should be lowered");
    assert_eq!(written.0, "hits");
    assert_eq!(function.type_of(written.1), number);
}

#[test]
fn writing_a_field_a_readonly_index_signature_covers_is_refused() {
    let source = "function fill(out: Frozen): void {\n  out.hits = 2;\n}\n";
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
    let provider = ByName::new(source, vec![("out", Answer::Known(out))]);
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
fn writing_a_field_no_index_signature_accepts_is_refused() {
    let source = "function fill(out: Counted): void {\n  out.hits = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let out = table.object_with_indexes(
        Vec::new(),
        vec![IndexSignature {
            key: number,
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

fn blocks_jumping_to(function: &savitar_ir::Function, wanted: savitar_ir::BlockId) -> Vec<usize> {
    function
        .blocks()
        .filter_map(|(_, block)| match &block.terminator {
            Terminator::Jump { target, arguments } if *target == wanted => Some(arguments.len()),
            _ => None,
        })
        .collect()
}

fn operators_in(function: &savitar_ir::Function, block: savitar_ir::BlockId) -> Vec<BinaryOp> {
    function
        .block(block)
        .statements
        .iter()
        .filter_map(|statement| match statement {
            Statement::Assign {
                value: Rvalue::Binary { op, .. },
                ..
            } => Some(*op),
            _ => None,
        })
        .collect()
}

fn parameterised(function: &savitar_ir::Function) -> Vec<(savitar_ir::BlockId, usize)> {
    function
        .blocks()
        .filter(|(_, block)| !block.parameters.is_empty())
        .map(|(id, block)| (id, block.parameters.len()))
        .collect()
}

#[test]
fn a_break_out_of_a_loop_meets_the_normal_exit_at_a_second_join() {
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

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    assert_eq!(
        carrying.len(),
        2,
        "the header carries the iteration, the exit carries what the two ways out disagree on"
    );
    let (header, header_parameters) = carrying[0];
    assert_eq!(header_parameters, 2, "the index and `found` are carried");
    let (exit, exit_parameters) = carrying[1];
    assert_eq!(exit_parameters, 1, "`found` leaves the loop as a parameter");
    assert_eq!(
        blocks_jumping_to(function, exit),
        vec![1, 1],
        "the normal exit and the `break` both carry `found`"
    );
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![2],
        "the body always breaks, so only the entry reaches the header"
    );
}

#[test]
fn a_loop_whose_body_only_breaks_grows_no_back_edge() {
    let source = "function once(flag: boolean, start: number): number {\n  let found = start;\n  while (flag) {\n    found = start;\n    break;\n  }\n  return found;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("flag", Answer::Known(boolean)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("once.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let header = parameterised(function)[0].0;
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![1],
        "only the entry reaches the header, because the body never runs twice"
    );
}

#[test]
fn a_break_in_one_arm_of_an_if_inside_a_loop_still_reaches_the_exit() {
    let source = "function until(limit: number, stop: number): number {\n  let seen = 0;\n  while (seen < limit) {\n    if (seen === stop) {\n      break;\n    }\n    seen = seen + 1;\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("limit", Answer::Known(number)),
            ("stop", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("until.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let header = parameterised(function)[0].0;
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![1, 1],
        "the entry and the back edge both carry `seen`"
    );
}

#[test]
fn a_break_inside_a_switch_inside_a_loop_leaves_only_the_switch() {
    let source = "function tally(kind: string, step: number): number {\n  let sum = 0;\n  while (sum < step) {\n    switch (kind) {\n      case \"add\":\n        sum = sum + step;\n        break;\n      default:\n        sum = sum + 1;\n    }\n  }\n  return sum;\n}\n";
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
    let file = SourceFile::new("tally.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let header = parameterised(function)[0].0;
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![1, 1],
        "the `break` ends the `switch`, so the loop still comes back"
    );
}

#[test]
fn a_break_in_an_inner_loop_leaves_only_the_inner_loop() {
    let source = "function pairs(rows: readonly number[], limit: number): number {\n  let seen = 0;\n  for (const row of rows) {\n    let inner = 0;\n    while (inner < limit) {\n      inner = inner + 1;\n      if (inner === row) {\n        break;\n      }\n      continue;\n    }\n    seen = seen + inner;\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let rows = table.intern(TypeKind::ReadonlyArray(number));
    let provider = ByName::new(
        source,
        vec![
            ("rows", Answer::Known(rows)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pairs.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    let (outer_header, outer_parameters) = carrying[0];
    assert_eq!(
        outer_parameters, 2,
        "the outer header carries the index and `seen`"
    );
    assert_eq!(
        blocks_jumping_to(function, outer_header),
        vec![2, 2],
        "the inner `break` and the inner `continue` never reach the outer header"
    );
}

#[test]
fn a_break_that_narrows_a_name_unions_it_with_the_normal_exit() {
    let source = "function find(values: readonly string[], wanted: string): string | undefined {\n  let found: string | undefined = undefined;\n  for (const value of values) {\n    if (value === wanted) {\n      found = value;\n      break;\n    }\n  }\n  return found;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let undefined = table.intern(TypeKind::Undefined);
    let optional = table.union(vec![text, undefined]);
    let values = table.intern(TypeKind::ReadonlyArray(text));
    let provider = ByName::new(
        source,
        vec![
            ("found", Answer::Known(optional)),
            ("find", returning(&mut table, optional)),
            ("values", Answer::Known(values)),
            ("wanted", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("find.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    let (exit, exit_parameters) = carrying[carrying.len() - 1];
    assert_eq!(exit_parameters, 1, "`found` leaves the loop as a parameter");
    let parameter = function.block(exit).parameters[0];
    assert_eq!(
        function.type_of(parameter),
        optional,
        "the `break` carries the string, and the union with the normal exit keeps the undefined"
    );
    assert_eq!(
        function.block(exit).terminator,
        Terminator::Return(Some(parameter))
    );
}

#[test]
fn a_do_while_runs_its_body_before_the_test() {
    let source = "function backoff(limit: number): number {\n  let waited = 1;\n  do {\n    waited = waited * 2;\n  } while (waited < limit);\n  return waited;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("limit", Answer::Known(number))]);
    let file = SourceFile::new("backoff.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    assert_eq!(
        carrying.len(),
        2,
        "the body and the test each carry `waited`"
    );
    let (header, _) = carrying[0];
    let (latch, _) = carrying[1];
    assert!(
        operators_in(function, header).contains(&BinaryOp::Multiply),
        "the body is the block the loop is entered through"
    );
    assert!(
        operators_in(function, latch).contains(&BinaryOp::Less),
        "the test is the block the body falls into"
    );
    assert_eq!(
        blocks_jumping_to(function, latch),
        vec![1],
        "only the end of the body reaches the test"
    );
    let Terminator::Branch {
        then_block,
        then_arguments,
        ..
    } = &function.block(latch).terminator
    else {
        panic!(
            "the test should branch, got {:?}",
            function.block(latch).terminator
        );
    };
    assert_eq!(*then_block, header, "the test decides on another iteration");
    assert_eq!(then_arguments.len(), 1, "and carries `waited` back");
}

#[test]
fn a_continue_in_a_do_while_still_runs_the_test() {
    let source = "function tally(limit: number, flag: boolean): number {\n  let seen = 0;\n  do {\n    seen = seen + 1;\n    if (flag) {\n      continue;\n    }\n    seen = seen + 1;\n  } while (seen < limit);\n  return seen;\n}\n";
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
    let file = SourceFile::new("tally.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let (latch, _) = parameterised(function)[1];
    assert_eq!(
        blocks_jumping_to(function, latch),
        vec![1, 1],
        "the `continue` and the end of the body both reach the test"
    );
}

#[test]
fn a_do_while_whose_body_returns_leaves_its_test_stranded() {
    let source =
        "function once(limit: number): number {\n  do {\n    return limit;\n  } while (true);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("limit", Answer::Known(number))]);
    let file = SourceFile::new("once.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        returns_without_a_value(function),
        0,
        "nothing leaves the loop, so no path falls off the end"
    );

    let dominance = savitar_ir::Dominance::of(function);
    let stranded = function
        .blocks()
        .filter(|(id, _)| !dominance.reachable(*id))
        .count();
    assert_eq!(stranded, 2, "the test and the exit are both left stranded");
}

#[test]
fn a_classic_for_carries_its_counter_through_the_header() {
    let source = "function upto(limit: number): number {\n  let sum = 0;\n  for (let index = 0; index < limit; index += 1) {\n    sum = sum + index;\n  }\n  return sum;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("limit", Answer::Known(number))]);
    let file = SourceFile::new("upto.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    let (header, header_parameters) = carrying[0];
    assert_eq!(
        header_parameters, 2,
        "the counter and the running total are both carried"
    );
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![2, 2],
        "the entry and the latch that runs the update"
    );
}

#[test]
fn a_continue_in_a_classic_for_runs_the_update_before_the_test() {
    let source = "function evens(limit: number): number {\n  let sum = 0;\n  for (let index = 0; index < limit; index += 1) {\n    if (index === 0) {\n      continue;\n    }\n    sum = sum + index;\n  }\n  return sum;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("limit", Answer::Known(number))]);
    let file = SourceFile::new("evens.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    let (header, _) = carrying[0];
    let (latch, _) = carrying[1];
    assert_eq!(
        blocks_jumping_to(function, latch),
        vec![2, 2],
        "the `continue` and the end of the body both land on the latch"
    );
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![2, 2],
        "nothing but the entry and the latch reaches the test, so the update always runs first"
    );
}

#[test]
fn a_name_the_for_initialiser_declares_does_not_outlive_the_loop() {
    let source = "function after(limit: number): number {\n  let sum = 0;\n  for (let index = 0; index < limit; index += 1) {\n    sum = sum + 1;\n  }\n  return index;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("limit", Answer::Known(number)),
            ("index", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("after.ts", source);

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
                    value: Rvalue::Global(name),
                    ..
                } if name == "index"
            )),
        "the `index` after the loop is a global read, not the loop counter"
    );
}

#[test]
fn a_continue_in_a_while_goes_straight_back_to_the_header() {
    let source = "function odds(limit: number, flag: boolean): number {\n  let seen = 0;\n  let kept = 0;\n  while (seen < limit) {\n    seen = seen + 1;\n    if (flag) {\n      continue;\n    }\n    kept = kept + 1;\n  }\n  return kept;\n}\n";
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
    let file = SourceFile::new("odds.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    assert_eq!(
        carrying.len(),
        1,
        "a `while` steps nothing, so the `continue` needs no latch of its own"
    );
    let (header, _) = carrying[0];
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![2, 2, 2],
        "the entry, the `continue` and the end of the body all carry both names"
    );
}

#[test]
fn a_continue_in_a_for_of_steps_the_index_through_the_latch() {
    let source = "function kept(values: readonly number[], floor: number): number {\n  let seen = 0;\n  for (const value of values) {\n    if (value < floor) {\n      continue;\n    }\n    seen = seen + 1;\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let values = table.intern(TypeKind::ReadonlyArray(number));
    let provider = ByName::new(
        source,
        vec![
            ("values", Answer::Known(values)),
            ("floor", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("kept.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    let (header, _) = carrying[0];
    let (latch, latch_parameters) = carrying[1];
    assert_eq!(
        latch_parameters, 2,
        "the latch carries the index and `seen`"
    );
    assert_eq!(
        blocks_jumping_to(function, latch),
        vec![2, 2],
        "the `continue` and the end of the body both reach the latch"
    );
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![2, 2],
        "only the entry and the latch reach the header, so the index is stepped once"
    );
}

#[test]
fn a_continue_inside_a_switch_inside_a_loop_leaves_the_switch_as_well() {
    let source = "function tally(kind: string, limit: number): number {\n  let seen = 0;\n  while (seen < limit) {\n    switch (kind) {\n      case \"skip\":\n        seen = seen + 1;\n        continue;\n      default:\n        seen = seen + 2;\n    }\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("kind", Answer::Known(text)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("tally.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let (header, _) = parameterised(function)[0];
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![1, 1, 1],
        "the entry, the `continue` that skipped the `switch`, and the end of the body"
    );
}

#[test]
fn a_continue_with_no_loop_around_it_is_refused() {
    let source = "function pick(kind: string): number {\n  switch (kind) {\n    case \"a\":\n      continue;\n    default:\n      return 1;\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(source, vec![("kind", Answer::Known(text))]);
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("no loop around it")),
        "{:?}",
        report.diagnostics
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

fn narrowed(function: &savitar_ir::Function) -> Vec<savitar_types::TypeId> {
    function
        .blocks()
        .flat_map(|(_, block)| block.statements.iter())
        .filter_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::Narrow(_),
                ..
            } => Some(function.type_of(*target)),
            _ => None,
        })
        .collect()
}

fn text_or_number(table: &mut TypeTable) -> (savitar_types::TypeId, savitar_types::TypeId) {
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![text, number]);
    (number, either)
}

fn named_field(name: &str, type_id: savitar_types::TypeId, optional: bool) -> Field {
    Field {
        name: name.to_string(),
        type_id,
        optional,
        method: false,
        readonly: false,
    }
}

fn taking(
    table: &mut TypeTable,
    takes: savitar_types::TypeId,
    returns: savitar_types::TypeId,
) -> savitar_types::TypeId {
    table.function(
        vec![savitar_types::Parameter {
            type_id: takes,
            rest: false,
            optional: false,
        }],
        returns,
    )
}

struct Frames {
    union: savitar_types::TypeId,
    tags: savitar_types::TypeId,
    text: savitar_types::TypeId,
    binary: savitar_types::TypeId,
}

fn frames(
    table: &mut TypeTable,
    tags: (savitar_types::TypeId, savitar_types::TypeId),
    tag_optional: bool,
) -> Frames {
    let text = table.intern(TypeKind::String);
    let number = table.intern(TypeKind::Number);
    let text_frame = table.object(vec![
        named_field("kind", tags.0, tag_optional),
        named_field("text", text, false),
    ]);
    let binary_frame = table.object(vec![
        named_field("kind", tags.1, tag_optional),
        named_field("size", number, false),
    ]);
    Frames {
        union: table.union(vec![text_frame, binary_frame]),
        tags: table.union(vec![tags.0, tags.1]),
        text: text_frame,
        binary: binary_frame,
    }
}

fn frame_tags(table: &mut TypeTable) -> (savitar_types::TypeId, savitar_types::TypeId) {
    (
        table.intern(TypeKind::StringLiteral("text".to_string())),
        table.intern(TypeKind::StringLiteral("binary".to_string())),
    )
}

#[test]
fn a_name_tested_by_typeof_number_is_narrowed_to_number_in_that_branch() {
    let source = "function use(value: string | number): number {\n  if (typeof value === \"number\") {\n    return value + 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let (number, either) = text_or_number(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        narrowed(function).contains(&number),
        "the branch where typeof answers number types the name as a number"
    );
}

#[test]
fn a_typeof_test_with_the_word_on_the_left_narrows_the_same_way() {
    let source = "function use(value: string | number): number {\n  if (\"number\" === typeof value) {\n    return value + 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let (number, either) = text_or_number(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(narrowed(&report.functions[0]).contains(&number));
}

#[test]
fn the_branch_where_typeof_fails_drops_only_that_primitive() {
    let source = "function use(value: string | number | undefined): number {\n  if (typeof value === \"string\") {\n    return 0;\n  }\n  return count(value);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let nothing = table.intern(TypeKind::Undefined);
    let any_of_three = table.union(vec![text, number, nothing]);
    let number_or_nothing = table.union(vec![number, nothing]);
    let counts = taking(&mut table, number_or_nothing, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(any_of_three)),
            ("count", Answer::Known(counts)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(
        narrowed(&report.functions[0]).contains(&number_or_nothing),
        "the other branch keeps every member whose typeof is not string"
    );
}

#[test]
fn a_ternary_narrows_both_arms_by_typeof() {
    let source = "function use(value: string | number): number {\n  return typeof value === \"string\" ? width(value) : value + 1;\n}\n";
    let mut table = TypeTable::new();
    let (number, either) = text_or_number(&mut table);
    let text = table.intern(TypeKind::String);
    let widths = taking(&mut table, text, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
            ("width", Answer::Known(widths)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let narrowings = narrowed(&report.functions[0]);
    assert!(
        narrowings.contains(&text),
        "the arm that holds takes string"
    );
    assert!(
        narrowings.contains(&number),
        "the arm that fails takes number"
    );
}

#[test]
fn typeof_object_keeps_null_in_the_branch_that_holds_because_typeof_null_answers_object() {
    let source = "function use(value: string | Shape | null): number {\n  if (typeof value === \"object\") {\n    return count(value);\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let nothing = table.intern(TypeKind::Null);
    let shape = table.object(vec![named_field("size", number, false)]);
    let shape_or_null = table.union(vec![shape, nothing]);
    let counts = taking(&mut table, shape_or_null, number);
    let any_of_three = table.union(vec![text, shape, nothing]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(any_of_three)),
            ("count", Answer::Known(counts)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(
        narrowed(&report.functions[0]).contains(&shape_or_null),
        "the branch that holds keeps null next to the shape"
    );
}

#[test]
fn typeof_object_drops_null_in_the_branch_that_fails() {
    let source = "function use(value: string | Shape | null): number {\n  if (typeof value === \"object\") {\n    return 0;\n  }\n  return width(value);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let nothing = table.intern(TypeKind::Null);
    let shape = table.object(vec![named_field("size", number, false)]);
    let widths = taking(&mut table, text, number);
    let any_of_three = table.union(vec![text, shape, nothing]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(any_of_three)),
            ("width", Answer::Known(widths)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(
        narrowed(&report.functions[0]).contains(&text),
        "the branch that fails drops every member whose typeof answers object, null included"
    );
}

#[test]
fn typeof_function_narrows_to_the_member_that_declares_a_call() {
    let source = "function use(value: string | Handler): number {\n  if (typeof value === \"function\") {\n    return run(value);\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let handler = table.function(Vec::new(), number);
    let runs = taking(&mut table, handler, number);
    let either = table.union(vec![text, handler]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
            ("run", Answer::Known(runs)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(
        narrowed(&report.functions[0]).contains(&handler),
        "the branch that holds keeps only the callable member"
    );
}

#[test]
fn the_void_type_is_never_decided_by_typeof_because_it_may_carry_anything() {
    let source = "function use(value: string | void): number {\n  if (typeof value === \"object\") {\n    return 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let nothing = table.intern(TypeKind::Void);
    let either = table.union(vec![text, nothing]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        narrowed(&report.functions[0]),
        vec![nothing],
        "void stays in the branch that holds and in the branch that fails"
    );
}

#[test]
fn the_object_keyword_is_never_decided_by_typeof_because_it_includes_function() {
    let source = "function use(value: string | object): number {\n  if (typeof value === \"object\") {\n    return 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let anything = table.intern(TypeKind::NonPrimitive);
    let either = table.union(vec![text, anything]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        narrowed(&report.functions[0]),
        vec![anything],
        "the object keyword stays in both branches, because a function is an object"
    );
}

#[test]
fn a_typeof_test_on_something_that_is_not_a_name_narrows_nothing() {
    let source = "function use(value: string | number): number {\n  if (typeof pick() === \"string\") {\n    return 0;\n  }\n  return value + 1;\n}\n";
    let mut table = TypeTable::new();
    let (number, either) = text_or_number(&mut table);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
            ("pick", returning(&mut table, text)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

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
fn a_union_of_objects_is_filtered_to_the_member_the_discriminant_names() {
    let source = "function use(frame: Frame): number {\n  if (frame.kind === \"binary\") {\n    return frame.size;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        narrowed(function).contains(&shapes.binary),
        "the branch that tests the tag reads the member that carries it"
    );
}

#[test]
fn the_branch_where_the_discriminant_does_not_match_narrows_to_the_other_member() {
    let source = "function use(frame: Frame): string {\n  if (frame.kind === \"binary\") {\n    return \"\";\n  }\n  return frame.text;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, text)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(narrowed(&report.functions[0]).contains(&shapes.text));
}

#[test]
fn a_discriminant_test_with_strict_inequality_filters_the_other_way() {
    let source = "function use(frame: Frame): number {\n  if (frame.kind !== \"text\") {\n    return frame.size;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(narrowed(&report.functions[0]).contains(&shapes.binary));
}

#[test]
fn a_ternary_filters_the_union_by_discriminant_in_both_arms() {
    let source = "function use(frame: Frame): string {\n  return frame.kind === \"text\" ? frame.text : label(frame);\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let labels = taking(&mut table, shapes.binary, text);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, text)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
            ("label", Answer::Known(labels)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let narrowings = narrowed(&report.functions[0]);
    assert!(narrowings.contains(&shapes.text));
    assert!(narrowings.contains(&shapes.binary));
}

#[test]
fn a_discriminant_tested_against_a_number_literal_narrows_the_same_way() {
    let source = "function use(frame: Frame): number {\n  if (frame.kind === 2) {\n    return frame.size;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let first = table.number_literal(1.0);
    let second = table.number_literal(2.0);
    let shapes = frames(&mut table, (first, second), false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(narrowed(&report.functions[0]).contains(&shapes.binary));
}

#[test]
fn a_discriminant_field_that_is_not_a_literal_type_narrows_nothing() {
    let source = "function use(frame: Frame): number {\n  if (frame.kind === \"binary\") {\n    return frame.size;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let shapes = frames(&mut table, (text, text), false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
}

#[test]
fn an_optional_discriminant_field_narrows_nothing_because_it_may_be_absent() {
    let source = "function use(frame: Frame): number {\n  if (frame.kind === \"binary\") {\n    return frame.size;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, true);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
}

#[test]
fn an_in_test_keeps_only_the_members_that_declare_the_field() {
    let source = "function use(frame: Frame): number {\n  if (\"size\" in frame) {\n    return count(frame);\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let counts = taking(&mut table, shapes.binary, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("count", Answer::Known(counts)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        narrowed(function).contains(&shapes.binary),
        "the branch that holds keeps the member that declares the field"
    );
}

#[test]
fn an_in_test_drops_the_members_that_declare_the_field_in_the_other_branch() {
    let source = "function use(frame: Frame): string {\n  if (\"size\" in frame) {\n    return \"\";\n  }\n  return label(frame);\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let labels = taking(&mut table, shapes.text, text);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, text)),
            ("frame", Answer::Known(shapes.union)),
            ("label", Answer::Known(labels)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(
        narrowed(&report.functions[0]).contains(&shapes.text),
        "the branch that fails keeps what does not declare the field"
    );
}

#[test]
fn a_member_with_an_index_signature_stays_in_both_branches_of_an_in_test() {
    let source = "function use(value: Holder): number {\n  if (\"size\" in value) {\n    return 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let bag = table.object_with_indexes(
        Vec::new(),
        vec![IndexSignature {
            key: text,
            value: number,
            readonly: false,
        }],
    );
    let sized = table.object(vec![named_field("size", number, false)]);
    let either = table.union(vec![bag, sized]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        narrowed(&report.functions[0]),
        vec![bag],
        "an index signature may answer any field, so it decides neither branch"
    );
}

#[test]
fn a_member_whose_field_is_optional_stays_in_both_branches_of_an_in_test() {
    let source = "function use(value: Holder): number {\n  if (\"size\" in value) {\n    return 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let maybe = table.object(vec![named_field("size", number, true)]);
    let surely = table.object(vec![
        named_field("size", number, false),
        named_field("label", text, false),
    ]);
    let either = table.union(vec![maybe, surely]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        narrowed(&report.functions[0]),
        vec![maybe],
        "an optional field may be absent, so it decides neither branch"
    );
}

#[test]
fn a_negated_in_test_narrows_the_branch_where_the_field_is_declared() {
    let source = "function use(frame: Frame): number {\n  if (!(\"size\" in frame)) {\n    return 0;\n  }\n  return count(frame);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let counts = taking(&mut table, shapes.binary, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("count", Answer::Known(counts)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(narrowed(&report.functions[0]).contains(&shapes.binary));
}

#[test]
fn the_in_operator_is_refused_when_the_right_side_carries_no_field() {
    let source = "function use(value: string): boolean {\n  return \"size\" in value;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let flag = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, flag)),
            ("value", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-operator");
    assert!(
        report.diagnostics[0]
            .message
            .contains("fields on the right"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn the_truthy_branch_drops_undefined_null_false_zero_and_the_empty_string() {
    let source = "function use(value: Loose): number {\n  if (value) {\n    return 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let empty = table.intern(TypeKind::StringLiteral(String::new()));
    let letter = table.intern(TypeKind::StringLiteral("a".to_string()));
    let zero = table.number_literal(0.0);
    let one = table.number_literal(1.0);
    let no = table.intern(TypeKind::BooleanLiteral(false));
    let yes = table.intern(TypeKind::BooleanLiteral(true));
    let nothing = table.intern(TypeKind::Undefined);
    let nil = table.intern(TypeKind::Null);
    let loose = table.union(vec![empty, letter, zero, one, no, yes, nothing, nil]);
    let truthy = table.union(vec![letter, one, yes]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(loose)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        narrowed(function).contains(&truthy),
        "only the members that no value of them makes true are dropped"
    );
}

#[test]
fn the_truth_of_a_name_leaves_string_number_and_boolean_alone_because_each_holds_a_false_value() {
    let source = "function use(value: string | number | boolean): number {\n  if (value) {\n    return 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let flag = table.intern(TypeKind::Boolean);
    let any_of_three = table.union(vec![text, number, flag]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(any_of_three)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        narrowed(&report.functions[0]),
        Vec::new(),
        "the empty string, zero and false still fit their own types"
    );
}

#[test]
fn the_falsy_branch_of_a_name_narrows_nothing_because_a_false_value_keeps_its_type() {
    let source = "function use(value: string | undefined): number {\n  if (value) {\n    return 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let nothing = table.intern(TypeKind::Undefined);
    let either = table.union(vec![text, nothing]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        narrowed(&report.functions[0]),
        vec![text],
        "only the branch where the name holds is narrowed"
    );
}

#[test]
fn a_negated_name_narrows_the_branch_where_it_holds() {
    let source = "function use(value: string | undefined): number {\n  if (!value) {\n    return 0;\n  }\n  return width(value);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let nothing = table.intern(TypeKind::Undefined);
    let either = table.union(vec![text, nothing]);
    let widths = taking(&mut table, text, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
            ("width", Answer::Known(widths)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        narrowed(&report.functions[0]),
        vec![text],
        "`!` swaps the branches and nothing else"
    );
}

#[test]
fn a_ternary_narrows_the_arm_where_the_name_holds() {
    let source = "function use(value: string | undefined): number {\n  return value ? width(value) : 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let nothing = table.intern(TypeKind::Undefined);
    let either = table.union(vec![text, nothing]);
    let widths = taking(&mut table, text, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
            ("width", Answer::Known(widths)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(narrowed(&report.functions[0]).contains(&text));
}

#[test]
fn a_switch_case_narrows_the_discriminant_to_the_member_its_label_names() {
    let source = "function use(frame: Frame): number {\n  switch (frame.kind) {\n    case \"binary\":\n      return count(frame);\n    default:\n      return 0;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let counts = taking(&mut table, shapes.binary, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
            ("count", Answer::Known(counts)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        narrowed(function).contains(&shapes.binary),
        "the body of the case holds the member the label names"
    );
}

#[test]
fn the_default_of_a_switch_drops_every_member_a_label_decided() {
    let source = "function use(frame: Frame): string {\n  switch (frame.kind) {\n    case \"binary\":\n      return \"\";\n    default:\n      return label(frame);\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let labels = taking(&mut table, shapes.text, text);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, text)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
            ("label", Answer::Known(labels)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(
        narrowed(&report.functions[0]).contains(&shapes.text),
        "the default holds what no label named"
    );
}

#[test]
fn a_case_with_several_labels_narrows_the_name_to_the_union_of_them() {
    let source = "function use(kind: Kind): number {\n  switch (kind) {\n    case \"a\":\n    case \"b\":\n      return early(kind);\n    default:\n      return 0;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let first = table.intern(TypeKind::StringLiteral("a".to_string()));
    let second = table.intern(TypeKind::StringLiteral("b".to_string()));
    let third = table.intern(TypeKind::StringLiteral("c".to_string()));
    let any_of_three = table.union(vec![first, second, third]);
    let first_two = table.union(vec![first, second]);
    let earlier = taking(&mut table, first_two, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("kind", Answer::Known(any_of_three)),
            ("early", Answer::Known(earlier)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        narrowed(function).contains(&first_two),
        "the body both labels reach holds either of them"
    );
}

#[test]
fn the_default_of_a_switch_over_a_name_keeps_only_the_label_nobody_wrote() {
    let source = "function use(kind: Kind): number {\n  switch (kind) {\n    case \"a\":\n      return 1;\n    case \"b\":\n      return 2;\n    default:\n      return late(kind);\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let first = table.intern(TypeKind::StringLiteral("a".to_string()));
    let second = table.intern(TypeKind::StringLiteral("b".to_string()));
    let third = table.intern(TypeKind::StringLiteral("c".to_string()));
    let any_of_three = table.union(vec![first, second, third]);
    let later = taking(&mut table, third, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("kind", Answer::Known(any_of_three)),
            ("late", Answer::Known(later)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(
        narrowed(&report.functions[0]).contains(&third),
        "the default holds the only label the switch left out"
    );
}

#[test]
fn a_case_whose_label_is_not_a_literal_narrows_nothing() {
    let source = "function use(kind: Kind, other: string): number {\n  switch (kind) {\n    case other:\n      return 1;\n    default:\n      return 0;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let first = table.intern(TypeKind::StringLiteral("a".to_string()));
    let second = table.intern(TypeKind::StringLiteral("b".to_string()));
    let either = table.union(vec![first, second]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("kind", Answer::Known(either)),
            ("other", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        narrowed(&report.functions[0]),
        Vec::new(),
        "a label the compiler cannot read decides nothing, in its case or in the default"
    );
}

#[test]
fn a_case_fallen_into_from_the_default_narrows_nothing_because_the_two_paths_disagree() {
    let source = "function use(kind: Kind): number {\n  switch (kind) {\n    default:\n    case \"a\":\n      return 1;\n    case \"b\":\n      return 2;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let first = table.intern(TypeKind::StringLiteral("a".to_string()));
    let second = table.intern(TypeKind::StringLiteral("b".to_string()));
    let third = table.intern(TypeKind::StringLiteral("c".to_string()));
    let any_of_three = table.union(vec![first, second, third]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("kind", Answer::Known(any_of_three)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let narrowings = narrowed(&report.functions[0]);
    assert!(
        narrowings.contains(&third) && narrowings.contains(&second),
        "the default and the case nobody runs into still narrow"
    );
    assert!(
        !narrowings.contains(&first),
        "the case the default runs into is reached by two disagreeing paths and stays wide"
    );
}

#[test]
fn a_name_assigned_inside_a_nested_for_of_is_carried_by_the_outer_loop() {
    let source = "function total(rows: number[], limit: number): number {\n  let sum = 0;\n  while (sum < limit) {\n    for (const row of rows) {\n      sum = sum + row;\n    }\n  }\n  return sum;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let rows = table.intern(TypeKind::Array(number));
    let provider = ByName::new(
        source,
        vec![
            ("rows", Answer::Known(rows)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("total.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(verify(&report.functions[0]), vec![]);
}

#[test]
fn a_name_assigned_in_the_initialiser_of_a_nested_for_is_carried_by_the_outer_loop() {
    let source = "function scan(limit: number, step: number, start: number): number {\n  let index = 0;\n  let counter = 0;\n  while (index < limit) {\n    for (index = start; counter < limit; counter = counter + step) {\n      counter = counter + step;\n    }\n  }\n  return index;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("limit", Answer::Known(number)),
            ("step", Answer::Known(number)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("scan.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(verify(&report.functions[0]), vec![]);
}

#[test]
fn a_boolean_literal_written_in_a_loop_body_still_fits_the_header_parameter() {
    let source = "function spin(flag: boolean, limit: number): boolean {\n  let done = flag;\n  let seen = 0;\n  while (seen < limit) {\n    done = false;\n    seen = seen + 1;\n  }\n  return done;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("spin", returning(&mut table, boolean)),
            ("flag", Answer::Known(boolean)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("spin.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    assert_eq!(carrying.len(), 1, "a `while` repeats at its own header");
    let header = function.block(carrying[0].0);
    let carried: Vec<&TypeKind> = header
        .parameters
        .iter()
        .map(|parameter| table.kind(function.type_of(*parameter)))
        .collect();
    assert_eq!(carried, vec![&TypeKind::Boolean, &TypeKind::Number]);
}

#[test]
fn a_name_declared_wider_than_its_value_takes_a_write_of_either_member() {
    let source = "function pick(flag: boolean, left: number, right: string): number | string {\n  let held: number | string = left;\n  if (flag) {\n    held = right;\n  }\n  return held;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let boolean = table.intern(TypeKind::Boolean);
    let either = table.union(vec![number, text]);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, either)),
            ("flag", Answer::Known(boolean)),
            ("left", Answer::Known(number)),
            ("right", Answer::Known(text)),
            ("held", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let carrying = parameterised(function);
    assert_eq!(carrying.len(), 1, "the two arms meet at one join");
    let merged = function.block(carrying[0].0).parameters[0];
    assert_eq!(
        function.type_of(merged),
        either,
        "the join unites the type the write left with the type the other arm carried"
    );
}

#[test]
fn a_write_leaves_the_name_holding_the_type_the_value_carried() {
    let source = "function pin(given: string | undefined): string {\n  let host: string | undefined = given;\n  host = \"localhost\";\n  return host;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let maybe = table.optional(text);
    let provider = ByName::new(
        source,
        vec![
            ("pin", returning(&mut table, text)),
            ("given", Answer::Known(maybe)),
            ("host", Answer::Known(maybe)),
        ],
    );
    let file = SourceFile::new("pin.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let entry = function.entry().expect("the function should have a block");
    let Terminator::Return(Some(returned)) = function.block(entry).terminator else {
        panic!(
            "expected a return, got {:?}",
            function.block(entry).terminator
        );
    };
    assert_eq!(
        table.kind(function.type_of(returned)),
        &TypeKind::String,
        "the name no longer carries the missing member the declared type admits"
    );
}

#[test]
fn a_name_narrowed_by_a_write_still_takes_a_write_of_another_member() {
    let source = "function swap(left: number, right: string): string {\n  let held: number | string = left;\n  held = right;\n  held = right;\n  return held;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![number, text]);
    let provider = ByName::new(
        source,
        vec![
            ("swap", returning(&mut table, text)),
            ("left", Answer::Known(number)),
            ("right", Answer::Known(text)),
            ("held", Answer::Known(either)),
        ],
    );
    let file = SourceFile::new("swap.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(verify(&report.functions[0]), vec![]);
}

#[test]
fn a_write_the_declared_type_does_not_admit_is_refused_even_after_a_narrowing_write() {
    let source = "function slip(flag: boolean, right: string): string {\n  let held: string | undefined = right;\n  held = right;\n  held = flag;\n  return held;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let boolean = table.intern(TypeKind::Boolean);
    let maybe = table.optional(text);
    let provider = ByName::new(
        source,
        vec![
            ("slip", returning(&mut table, text)),
            ("flag", Answer::Known(boolean)),
            ("right", Answer::Known(text)),
            ("held", Answer::Known(maybe)),
        ],
    );
    let file = SourceFile::new("slip.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-assignment");
}

#[test]
fn a_narrow_write_in_a_loop_body_still_fits_the_header_parameter_the_declared_type_sized() {
    let source = "function latest(values: readonly string[]): string | undefined {\n  let seen: string | undefined = undefined;\n  for (const value of values) {\n    seen = value;\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let maybe = table.optional(text);
    let values = table.intern(TypeKind::ReadonlyArray(text));
    let provider = ByName::new(
        source,
        vec![
            ("latest", returning(&mut table, maybe)),
            ("values", Answer::Known(values)),
            ("seen", Answer::Known(maybe)),
        ],
    );
    let file = SourceFile::new("latest.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(
        verify(function),
        vec![],
        "the header parameter keeps the declared type, and the narrow write widens on the edge"
    );
}

#[test]
fn a_declaration_without_an_initialiser_takes_the_annotated_type_and_waits_for_a_write() {
    let source = "function pick(flag: boolean, left: number, right: number): number {\n  let chosen: number;\n  if (flag) {\n    chosen = left;\n  } else {\n    chosen = right;\n  }\n  return chosen;\n}\n";
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
            ("chosen", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(verify(&report.functions[0]), vec![]);
}

#[test]
fn a_name_declared_and_never_written_is_refused_when_it_is_read() {
    let source = "function early(): number {\n  let chosen: number;\n  return chosen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("early", returning(&mut table, number)),
            ("chosen", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("early.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
    assert!(
        report.diagnostics[0].message.contains("never written"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_name_written_on_only_one_branch_is_still_refused_after_the_join() {
    let source = "function half(flag: boolean, left: number): number {\n  let chosen: number;\n  if (flag) {\n    chosen = left;\n  }\n  return chosen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("half", returning(&mut table, number)),
            ("flag", Answer::Known(boolean)),
            ("left", Answer::Known(number)),
            ("chosen", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("half.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
}

#[test]
fn a_declaration_with_neither_an_initialiser_nor_an_annotation_is_refused() {
    let source = "function loose(start: number): number {\n  let chosen;\n  chosen = start;\n  return chosen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("loose", returning(&mut table, number)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("loose.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-statement");
}

#[test]
fn a_compound_write_to_a_name_never_written_is_refused_because_it_reads_first() {
    let source =
        "function step(): number {\n  let chosen: number;\n  chosen += 1;\n  return chosen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("step", returning(&mut table, number)),
            ("chosen", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("step.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
}

#[test]
fn a_name_declared_before_a_loop_and_first_written_inside_it_is_refused() {
    let source = "function last(values: readonly number[]): number {\n  let chosen: number;\n  for (const value of values) {\n    chosen = value;\n  }\n  return chosen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let values = table.intern(TypeKind::ReadonlyArray(number));
    let provider = ByName::new(
        source,
        vec![
            ("last", returning(&mut table, number)),
            ("values", Answer::Known(values)),
            ("chosen", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("last.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-statement");
    assert!(
        report.diagnostics[0]
            .message
            .contains("no value to carry in"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_name_a_block_shadows_goes_back_to_the_outer_value_after_the_block() {
    let source = "function shade(start: number, limit: number): number {\n  let value = start;\n  {\n    let value = limit;\n    value = value + 1;\n  }\n  return value;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("shade", returning(&mut table, number)),
            ("start", Answer::Known(number)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("shade.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let entry = function.entry().expect("the function should have a block");
    let Terminator::Return(Some(returned)) = function.block(entry).terminator else {
        panic!(
            "expected a return, got {:?}",
            function.block(entry).terminator
        );
    };
    assert_eq!(
        returned,
        function.parameters()[0],
        "the block stepped its own `value`, and the outer one never moved"
    );
}

#[test]
fn the_right_operand_of_and_sees_the_name_the_left_operand_narrowed() {
    let source = "function use(frame: Frame, limit: number): number {\n  if (frame.kind === \"binary\" && frame.size < limit) {\n    return limit;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("limit", Answer::Known(number)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        narrowed(function).contains(&shapes.binary),
        "the right operand only runs when the left one held, so it reads the member the tag names"
    );
}

#[test]
fn the_right_operand_of_or_sees_the_name_the_left_operand_ruled_out() {
    let source = "function use(frame: Frame): number {\n  if (frame.kind === \"binary\" || frame.text === \"ping\") {\n    return 1;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        narrowed(function).contains(&shapes.text),
        "the right operand only runs when the left one failed, so the tag it tested is gone"
    );
}

#[test]
fn the_branch_after_a_short_circuit_test_inherits_no_narrowing_from_it() {
    let source = "function use(frame: Frame, limit: number): number {\n  if (frame.kind === \"binary\" && frame.size < limit) {\n    return limit;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("limit", Answer::Known(number)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        narrowed(&report.functions[0]).len(),
        1,
        "the narrowing lives inside the short circuit and does not reach the branch it guards"
    );
}

#[test]
fn the_right_operand_of_nullish_coalescing_keeps_the_name_whole_because_it_runs_when_it_is_missing()
{
    let source = "function use(value: string | undefined, fallback: string): string {\n  return value ?? fallback;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let maybe = table.optional(text);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, text)),
            ("value", Answer::Known(maybe)),
            ("fallback", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        narrowed(function),
        vec![text],
        "only the branch that keeps the left value narrows, and it drops the missing members"
    );
}

fn fields_read(function: &savitar_ir::Function, wanted: &str) -> Vec<savitar_types::TypeId> {
    function
        .blocks()
        .flat_map(|(_, block)| block.statements.iter())
        .filter_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::ReadField { name, .. },
                ..
            } if name == wanted => Some(function.type_of(*target)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_field_read_from_a_union_takes_the_union_of_that_field_in_every_member() {
    let source =
        "function use(holder: Small | Large): string | number {\n  return holder.amount;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![number, text]);
    let small = table.object(vec![named_field("amount", number, false)]);
    let large = table.object(vec![named_field("amount", text, false)]);
    let holder = table.union(vec![small, large]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, either)),
            ("holder", Answer::Known(holder)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(fields_read(function, "amount"), vec![either]);
}

#[test]
fn a_field_read_from_a_union_is_refused_when_one_member_declares_no_such_field() {
    let source = "function use(holder: Small | Plain): number {\n  return holder.amount;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let small = table.object(vec![named_field("amount", number, false)]);
    let plain = table.object(vec![named_field("label", text, false)]);
    let holder = table.union(vec![small, plain]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("holder", Answer::Known(holder)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
    assert!(
        report.diagnostics[0].message.contains("no field `amount`"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_field_that_one_member_of_a_union_marks_optional_is_read_carrying_undefined() {
    let source =
        "function use(holder: Small | Loose): number | undefined {\n  return holder.amount;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let maybe = table.optional(number);
    let small = table.object(vec![named_field("amount", number, false)]);
    let loose = table.object(vec![named_field("amount", number, true)]);
    let holder = table.union(vec![small, loose]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, maybe)),
            ("holder", Answer::Known(holder)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(fields_read(function, "amount"), vec![maybe]);
}

#[test]
fn a_field_read_from_a_union_ignores_a_narrower_answer_from_the_provider() {
    let source = "function use(frame: Frame): string {\n  return frame.kind;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, text)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(tags.0)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        fields_read(function, "kind"),
        vec![shapes.tags],
        "the provider answered the tag of one member, and Savitar kept its own union"
    );
}

#[test]
fn a_field_read_from_a_union_that_holds_undefined_is_refused_rather_than_narrowed_by_the_provider()
{
    let source = "function use(frame: Frame | undefined): string {\n  return frame.kind;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let maybe = table.optional(shapes.union);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, text)),
            ("frame", Answer::Known(maybe)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
}

#[test]
fn a_discriminant_tested_over_a_member_path_narrows_nothing_because_no_name_holds_the_path() {
    let source = "function use(holder: Holder): number {\n  if (holder.frame.kind === \"binary\") {\n    return holder.frame.size;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let holder = table.object(vec![named_field("frame", shapes.union, false)]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("holder", Answer::Known(holder)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
    assert!(
        report.diagnostics[0].message.contains("no field `size`"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_array_pattern_with_a_hole_reads_nothing_at_that_position() {
    let source = "function tail(parts: string[]): string[] {\n  const [, ...rest] = parts;\n  return rest;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let texts = table.intern(TypeKind::Array(text));
    let provider = ByName::new(
        source,
        vec![
            ("tail", returning(&mut table, texts)),
            ("parts", Answer::Known(texts)),
        ],
    );
    let file = SourceFile::new("tail.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        !only_block(function)
            .statements
            .iter()
            .any(|statement| matches!(
                statement,
                Statement::Assign {
                    value: Rvalue::ReadElement { .. },
                    ..
                }
            )),
        "a hole binds no name, so nothing is read for it"
    );
    let sliced = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                value: Rvalue::Slice { from, .. },
                ..
            } => Some(*from),
            _ => None,
        })
        .expect("the rest should be sliced");
    assert_eq!(sliced, 1, "the hole still takes up its position");
}

#[test]
fn an_assignment_to_a_pattern_that_writes_to_a_field_is_refused() {
    let source = "function take(point: Point): void {\n  [point.x] = [1];\n}\n";
    let mut table = TypeTable::new();
    let point = point_shape(&mut table);
    let provider = ByName::new(source, vec![("point", Answer::Known(point))]);
    let file = SourceFile::new("take.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

fn route_shape(table: &mut TypeTable) -> savitar_types::TypeId {
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    table.object(vec![
        Field {
            name: "handler".to_string(),
            type_id: text,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "timeout".to_string(),
            type_id: number,
            optional: true,
            method: false,
            readonly: false,
        },
    ])
}

fn read_fields_of(function: &savitar_ir::Function) -> Vec<String> {
    only_block(function)
        .statements
        .iter()
        .filter_map(|statement| match statement {
            Statement::Assign {
                value: Rvalue::ReadField { name, .. },
                ..
            } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_object_pattern_reads_each_field_the_shape_declares() {
    let source = "function pick(route: Route): string {\n  const { handler } = route;\n  return handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(read_fields_of(function), vec!["handler".to_string()]);
    let Terminator::Return(Some(returned)) = only_block(function).terminator else {
        panic!("expected a return");
    };
    assert_eq!(
        function.type_of(returned),
        text,
        "the bound name takes the type the field declares"
    );
}

#[test]
fn an_object_pattern_that_renames_binds_the_new_name_to_the_field_read() {
    let source = "function pick(route: Route): string {\n  const { handler: run } = route;\n  return run;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(read_fields_of(function), vec!["handler".to_string()]);
    let Terminator::Return(Some(returned)) = only_block(function).terminator else {
        panic!("expected a return");
    };
    assert_eq!(function.type_of(returned), text);
}

#[test]
fn an_object_pattern_over_an_optional_field_carries_undefined_in_the_bound_name() {
    let source = "function wait(route: Route): number | undefined {\n  const { timeout } = route;\n  return timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let maybe = table.optional(number);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("wait", returning(&mut table, maybe)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("wait.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let Terminator::Return(Some(returned)) = only_block(function).terminator else {
        panic!("expected a return");
    };
    assert!(
        table.admits_undefined(function.type_of(returned)),
        "an optional field is read as the field type with undefined"
    );
}

#[test]
fn an_object_pattern_over_a_field_the_shape_never_declares_is_refused() {
    let source = "function pick(route: Route): string {\n  const { missing } = route;\n  return missing;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
    assert!(
        report.diagnostics[0].message.contains("missing"),
        "{:?}",
        report.diagnostics[0]
    );
}

#[test]
fn an_object_pattern_over_something_with_no_declared_shape_is_refused() {
    let source =
        "function pick(count: number): number {\n  const { size } = count;\n  return size;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, number)),
            ("count", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

#[test]
fn an_object_pattern_with_a_computed_key_is_refused() {
    let source = "function pick(route: Route, key: string): string {\n  const { [key]: found } = route;\n  return found;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("route", Answer::Known(route)),
            ("key", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

fn returned_value(function: &savitar_ir::Function) -> savitar_ir::ValueId {
    function
        .blocks()
        .find_map(|(_, block)| match block.terminator {
            Terminator::Return(Some(value)) => Some(value),
            _ => None,
        })
        .expect("the function should return a value")
}

fn tests_undefined(function: &savitar_ir::Function) -> bool {
    function.blocks().any(|(_, block)| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                Statement::Assign {
                    value: Rvalue::Constant(Constant::Undefined),
                    ..
                }
            )
        }) && matches!(block.terminator, Terminator::Branch { .. })
    })
}

#[test]
fn a_default_in_an_object_pattern_takes_over_only_where_the_read_gives_undefined() {
    let source = "function wait(route: Route): number {\n  const { timeout = 30 } = route;\n  return timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("wait", returning(&mut table, number)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("wait.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        tests_undefined(function),
        "the default is reached by testing the read against undefined"
    );
    assert_eq!(
        function.type_of(returned_value(function)),
        number,
        "the bound name is the read without undefined, joined with the default"
    );
}

#[test]
fn a_default_in_an_array_pattern_covers_the_undefined_a_read_at_an_index_admits() {
    let source = "function first(counts: number[]): number {\n  const [head = 0] = counts;\n  return head;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let numbers = table.intern(TypeKind::Array(number));
    let provider = ByName::new(
        source,
        vec![
            ("first", returning(&mut table, number)),
            ("counts", Answer::Known(numbers)),
        ],
    );
    let file = SourceFile::new("first.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(tests_undefined(function));
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn a_default_never_takes_over_on_null_so_null_stays_in_the_bound_type() {
    let source = "function pull(box: Box): number | null {\n  const { held = 7 } = box;\n  return held;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let nothing = table.intern(TypeKind::Null);
    let either = table.union(vec![number, nothing]);
    let box_shape = table.object(vec![Field {
        name: "held".to_string(),
        type_id: either,
        optional: true,
        method: false,
        readonly: false,
    }]);
    let provider = ByName::new(
        source,
        vec![
            ("pull", returning(&mut table, either)),
            ("box", Answer::Known(box_shape)),
        ],
    );
    let file = SourceFile::new("pull.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let bound = function.type_of(returned_value(function));
    assert_eq!(
        bound, either,
        "only undefined is dropped by the default, so null is still held"
    );
    assert!(!table.admits_undefined(bound));
}

#[test]
fn a_default_on_a_read_that_never_gives_undefined_binds_the_read_with_no_branch() {
    let source = "function pick(route: Route): string {\n  const { handler = \"none\" } = route;\n  return handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        function.block_count(),
        1,
        "a field that is never undefined cannot reach the default"
    );
    assert_eq!(function.type_of(returned_value(function)), text);
}

#[test]
fn a_default_that_branches_on_its_own_still_joins_into_one_bound_name() {
    let source = "function wait(route: Route, slow: boolean): number {\n  const { timeout = slow ? 90 : 30 } = route;\n  return timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("wait", returning(&mut table, number)),
            ("route", Answer::Known(route)),
            ("slow", Answer::Known(boolean)),
        ],
    );
    let file = SourceFile::new("wait.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn a_default_that_does_not_fit_the_field_is_held_as_a_union_rather_than_coerced() {
    let source = "function wait(route: Route): number | string {\n  const { timeout = \"none\" } = route;\n  return timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![number, text]);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("wait", returning(&mut table, either)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("wait.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        function.type_of(returned_value(function)),
        either,
        "the bound name is the read without undefined, joined with the default"
    );
}

#[test]
fn a_default_in_a_pattern_that_assigns_to_a_name_is_refused() {
    let source = "function wait(route: Route): number {\n  let fallback = 30;\n  const { timeout = (fallback = 60) } = route;\n  return timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("wait", returning(&mut table, number)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("wait.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(!report.diagnostics.is_empty());
}

#[test]
fn a_hole_in_the_middle_of_an_array_pattern_keeps_the_later_positions_exact() {
    let source = "function second(pair: [number, string]): string {\n  const [, right] = pair;\n  return right;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![number, text], false);
    let provider = ByName::new(
        source,
        vec![
            ("second", returning(&mut table, text)),
            ("pair", Answer::Known(pair)),
        ],
    );
    let file = SourceFile::new("second.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        function.type_of(returned_value(function)),
        text,
        "the name after the hole reads position one, not position zero"
    );
}

#[test]
fn a_rest_in_an_object_pattern_keeps_the_fields_no_name_took() {
    let source = "function other(route: Route): string {\n  const { timeout, ...rest } = route;\n  return rest.handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("other", returning(&mut table, text)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("other.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let rest = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::Rest { taken, .. },
                ..
            } => Some((*target, taken.clone())),
            _ => None,
        })
        .expect("the rest should be taken from the object");
    assert_eq!(rest.1, vec!["timeout".to_string()]);
    let kept = table.object(vec![Field {
        name: "handler".to_string(),
        type_id: text,
        optional: false,
        method: false,
        readonly: false,
    }]);
    assert_eq!(
        function.type_of(rest.0),
        kept,
        "the rest holds every field the pattern left behind, and nothing else"
    );
}

#[test]
fn a_rest_in_an_object_pattern_keeps_an_optional_field_optional() {
    let source = "function other(route: Route): number | undefined {\n  const { handler, ...rest } = route;\n  return rest.timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let maybe = table.optional(number);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("other", returning(&mut table, maybe)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("other.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let kept = table.object(vec![Field {
        name: "timeout".to_string(),
        type_id: number,
        optional: true,
        method: false,
        readonly: false,
    }]);
    let rest = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::Rest { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the rest should be taken from the object");
    assert_eq!(function.type_of(rest), kept);
}

#[test]
fn a_rest_in_an_object_pattern_over_a_shape_with_an_index_signature_is_refused() {
    let source = "function other(counts: Record<string, number>): number {\n  const { total, ...rest } = counts;\n  return total;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let counts = table.object_with_indexes(
        Vec::new(),
        vec![IndexSignature {
            key: text,
            value: number,
            readonly: false,
        }],
    );
    let provider = ByName::new(
        source,
        vec![
            ("other", returning(&mut table, number)),
            ("counts", Answer::Known(counts)),
        ],
    );
    let file = SourceFile::new("other.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
    assert!(
        report.diagnostics[0].message.contains("index signature"),
        "the rest refuses, and the read of the named key no longer does: {}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_rest_in_an_object_pattern_that_would_carry_a_method_is_refused() {
    let source = "function other(clock: Clock): number {\n  const { now, ...rest } = clock;\n  return now;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let reader = table.function(Vec::new(), number);
    let clock = table.object(vec![
        Field {
            name: "now".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "tick".to_string(),
            type_id: reader,
            optional: false,
            method: true,
            readonly: false,
        },
    ]);
    let provider = ByName::new(
        source,
        vec![
            ("other", returning(&mut table, number)),
            ("clock", Answer::Known(clock)),
        ],
    );
    let file = SourceFile::new("other.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

fn wrapper_shape(table: &mut TypeTable) -> savitar_types::TypeId {
    let route = route_shape(table);
    table.object(vec![Field {
        name: "route".to_string(),
        type_id: route,
        optional: false,
        method: false,
        readonly: false,
    }])
}

#[test]
fn an_object_pattern_nested_in_an_object_pattern_reads_a_field_of_a_field() {
    let source = "function pick(hop: Hop): string {\n  const { route: { handler } } = hop;\n  return handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let hop = wrapper_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("hop", Answer::Known(hop)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        read_fields_of(function),
        vec!["route".to_string(), "handler".to_string()],
        "the outer field is read first, and the inner one is read from it"
    );
    assert_eq!(function.type_of(returned_value(function)), text);
}

#[test]
fn an_array_pattern_nested_in_an_object_pattern_reads_a_position_of_a_field() {
    let source = "function pick(book: Book): string {\n  const { pair: [, second] } = book;\n  return second;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![number, text], false);
    let book = table.object(vec![Field {
        name: "pair".to_string(),
        type_id: pair,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("book", Answer::Known(book)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), text);
}

#[test]
fn an_object_pattern_nested_in_an_array_pattern_reads_a_field_of_a_position() {
    let source = "function pick(routes: Route[]): string | undefined {\n  const [{ handler }] = routes;\n  return handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let maybe = table.optional(text);
    let route = route_shape(&mut table);
    let routes = table.intern(TypeKind::Array(route));
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, maybe)),
            ("routes", Answer::Known(routes)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert_eq!(
        report.diagnostics.len(),
        1,
        "a read at an index admits undefined, and a field cannot be read from undefined"
    );
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

#[test]
fn an_object_pattern_nested_in_an_array_pattern_over_a_tuple_reads_the_exact_position() {
    let source = "function pick(pair: [Route, Route]): string {\n  const [{ handler }] = pair;\n  return handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let pair = table.tuple(vec![route, route], false);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("pair", Answer::Known(pair)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), text);
}

#[test]
fn a_default_inside_a_nested_pattern_joins_before_the_inner_names_are_bound() {
    let source = "function pick(hop: Hop): number {\n  const { route: { timeout = 30 } } = hop;\n  return timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let hop = wrapper_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, number)),
            ("hop", Answer::Known(hop)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(tests_undefined(function));
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn a_nested_pattern_three_levels_deep_still_reads_one_field_at_a_time() {
    let source = "function pick(deep: Deep): string {\n  const { outer: { route: { handler } } } = deep;\n  return handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let hop = wrapper_shape(&mut table);
    let deep = table.object(vec![Field {
        name: "outer".to_string(),
        type_id: hop,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("deep", Answer::Known(deep)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        read_fields_of(function),
        vec![
            "outer".to_string(),
            "route".to_string(),
            "handler".to_string()
        ]
    );
}

#[test]
fn a_nested_pattern_over_a_field_the_inner_shape_never_declares_is_refused() {
    let source = "function pick(hop: Hop): string {\n  const { route: { missing } } = hop;\n  return missing;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let hop = wrapper_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("hop", Answer::Known(hop)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

#[test]
fn a_rest_nested_in_an_object_pattern_takes_the_fields_of_the_inner_shape() {
    let source = "function pick(hop: Hop): string {\n  const { route: { timeout, ...rest } } = hop;\n  return rest.handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let hop = wrapper_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("hop", Answer::Known(hop)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let kept = table.object(vec![Field {
        name: "handler".to_string(),
        type_id: text,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let rest = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::Rest { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the inner rest should be taken");
    assert_eq!(function.type_of(rest), kept);
}

#[test]
fn an_assignment_to_an_array_pattern_writes_to_the_names_already_bound() {
    let source = "function swap(pair: [number, number]): number {\n  let left = 0;\n  let right = 0;\n  [left, right] = pair;\n  return left + right;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let pair = table.tuple(vec![number, number], false);
    let provider = ByName::new(
        source,
        vec![
            ("swap", returning(&mut table, number)),
            ("pair", Answer::Known(pair)),
        ],
    );
    let file = SourceFile::new("swap.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        function.type_of(returned_value(function)),
        number,
        "each name kept the type it was declared with"
    );
}

#[test]
fn an_assignment_to_an_object_pattern_writes_to_the_names_already_bound() {
    let source = "function reset(route: Route): string {\n  let handler = \"none\";\n  ({ handler } = route);\n  return handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("reset", returning(&mut table, text)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("reset.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(read_fields_of(function), vec!["handler".to_string()]);
    assert_eq!(function.type_of(returned_value(function)), text);
}

#[test]
fn an_assignment_to_an_object_pattern_that_renames_writes_to_the_renamed_name() {
    let source = "function reset(route: Route): string {\n  let run = \"none\";\n  ({ handler: run } = route);\n  return run;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("reset", returning(&mut table, text)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("reset.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), text);
}

#[test]
fn an_assignment_to_a_pattern_that_does_not_fit_the_type_the_name_holds_is_refused() {
    let source = "function reset(route: Route): number {\n  let timeout = 0;\n  ({ handler: timeout } = route);\n  return timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("reset", returning(&mut table, number)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("reset.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-assignment");
}

#[test]
fn an_assignment_to_a_pattern_whose_read_admits_undefined_is_refused_by_the_name_it_writes() {
    let source = "function reset(route: Route): number {\n  let timeout = 0;\n  ({ timeout } = route);\n  return timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("reset", returning(&mut table, number)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("reset.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-assignment");
}

#[test]
fn an_assignment_to_a_pattern_with_a_default_fits_the_name_once_undefined_is_gone() {
    let source = "function reset(route: Route): number {\n  let timeout = 0;\n  ({ timeout = 30 } = route);\n  return timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let route = route_shape(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("reset", returning(&mut table, number)),
            ("route", Answer::Known(route)),
        ],
    );
    let file = SourceFile::new("reset.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(tests_undefined(function));
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn an_assignment_to_a_pattern_over_a_name_no_one_bound_is_refused() {
    let source = "function reset(pair: [number, number]): number {\n  [first, second] = pair;\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let pair = table.tuple(vec![number, number], false);
    let provider = ByName::new(
        source,
        vec![
            ("reset", returning(&mut table, number)),
            ("pair", Answer::Known(pair)),
        ],
    );
    let file = SourceFile::new("reset.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-reference");
}

#[test]
fn an_assignment_to_a_pattern_inside_a_loop_carries_every_name_it_writes() {
    let source = "function drain(pairs: [number, number][]): number {\n  let left = 0;\n  let right = 0;\n  for (const pair of pairs) {\n    [left, right] = pair;\n  }\n  return left + right;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let pair = table.tuple(vec![number, number], false);
    let pairs = table.intern(TypeKind::Array(pair));
    let provider = ByName::new(
        source,
        vec![
            ("drain", returning(&mut table, number)),
            ("pairs", Answer::Known(pairs)),
        ],
    );
    let file = SourceFile::new("drain.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
}

#[test]
fn an_assignment_to_a_pattern_with_a_rest_writes_the_slice_to_the_name() {
    let source = "function split(parts: string[]): string[] {\n  let head = \"\";\n  let rest: string[] = [];\n  [head, ...rest] = parts;\n  return rest;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let texts = table.intern(TypeKind::Array(text));
    let provider = ByName::new(
        source,
        vec![
            ("split", returning(&mut table, texts)),
            ("parts", Answer::Known(texts)),
            ("rest", Answer::Known(texts)),
        ],
    );
    let file = SourceFile::new("split.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert_eq!(
        report.diagnostics.len(),
        1,
        "the head is read at an index, so it admits undefined and the name refuses it"
    );
    assert_eq!(report.diagnostics[0].rule, "unsupported-assignment");
}

fn signature_taking(
    table: &mut TypeTable,
    slots: Vec<savitar_types::TypeId>,
    returns: savitar_types::TypeId,
) -> Answer {
    let parameters = slots
        .into_iter()
        .map(|type_id| savitar_types::Parameter {
            type_id,
            rest: false,
            optional: false,
        })
        .collect();
    Answer::Known(table.function(parameters, returns))
}

#[test]
fn a_parameter_bound_by_an_object_pattern_reads_its_fields_from_the_whole_value() {
    let source = "function pick({ handler }: Route): string {\n  return handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let signature = signature_taking(&mut table, vec![route], text);
    let provider = ByName::new(source, vec![("pick", signature)]);
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        function.parameters().len(),
        1,
        "the pattern takes one incoming value, however many names it binds"
    );
    assert_eq!(function.type_of(function.parameters()[0]), route);
    assert_eq!(read_fields_of(function), vec!["handler".to_string()]);
    assert_eq!(function.type_of(returned_value(function)), text);
}

#[test]
fn a_parameter_bound_by_an_array_pattern_reads_its_positions_from_the_whole_value() {
    let source = "function pick([, second]: [number, string]): string {\n  return second;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![number, text], false);
    let signature = signature_taking(&mut table, vec![pair], text);
    let provider = ByName::new(source, vec![("pick", signature)]);
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.parameters().len(), 1);
    assert_eq!(function.type_of(returned_value(function)), text);
}

#[test]
fn a_patterned_parameter_beside_a_named_one_keeps_both_in_the_order_declared() {
    let source =
        "function pick(count: number, { handler }: Route): string {\n  return handler;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let signature = signature_taking(&mut table, vec![number, route], text);
    let provider = ByName::new(
        source,
        vec![("pick", signature), ("count", Answer::Known(number))],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.parameters().len(), 2);
    assert_eq!(function.type_of(function.parameters()[0]), number);
    assert_eq!(function.type_of(function.parameters()[1]), route);
}

#[test]
fn a_parameter_bound_by_a_pattern_whose_field_the_shape_never_declares_is_refused() {
    let source = "function pick({ missing }: Route): string {\n  return missing;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let signature = signature_taking(&mut table, vec![route], text);
    let provider = ByName::new(source, vec![("pick", signature)]);
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

#[test]
fn a_for_of_over_an_object_pattern_binds_the_fields_of_each_element() {
    let source = "function total(routes: readonly Route[]): string {\n  let last = \"\";\n  for (const { handler } of routes) {\n    last = handler;\n  }\n  return last;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let routes = table.intern(TypeKind::ReadonlyArray(route));
    let provider = ByName::new(
        source,
        vec![
            ("total", returning(&mut table, text)),
            ("routes", Answer::Known(routes)),
        ],
    );
    let file = SourceFile::new("total.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        function.type_of(returned_value(function)),
        text,
        "the name the loop writes keeps the type it was declared with"
    );
}

#[test]
fn a_for_of_over_an_array_pattern_binds_each_position_of_each_element() {
    let source = "function total(pairs: readonly [string, number][]): number {\n  let sum = 0;\n  for (const [, millis] of pairs) {\n    sum = sum + millis;\n  }\n  return sum;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let pair = table.tuple(vec![text, number], false);
    let pairs = table.intern(TypeKind::ReadonlyArray(pair));
    let provider = ByName::new(
        source,
        vec![
            ("total", returning(&mut table, number)),
            ("pairs", Answer::Known(pairs)),
        ],
    );
    let file = SourceFile::new("total.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn a_for_of_over_a_pattern_with_a_default_joins_inside_the_loop_body() {
    let source = "function total(routes: readonly Route[]): number {\n  let sum = 0;\n  for (const { timeout = 30 } of routes) {\n    sum = sum + timeout;\n  }\n  return sum;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let route = route_shape(&mut table);
    let routes = table.intern(TypeKind::ReadonlyArray(route));
    let provider = ByName::new(
        source,
        vec![
            ("total", returning(&mut table, number)),
            ("routes", Answer::Known(routes)),
        ],
    );
    let file = SourceFile::new("total.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(tests_undefined(function));
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn a_for_of_over_a_pattern_whose_field_the_element_never_declares_is_refused() {
    let source = "function total(routes: readonly Route[]): string {\n  let last = \"\";\n  for (const { missing } of routes) {\n    last = missing;\n  }\n  return last;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let routes = table.intern(TypeKind::ReadonlyArray(route));
    let provider = ByName::new(
        source,
        vec![
            ("total", returning(&mut table, text)),
            ("routes", Answer::Known(routes)),
        ],
    );
    let file = SourceFile::new("total.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

#[test]
fn a_default_over_a_read_typed_unknown_still_tests_undefined_because_unknown_admits_it() {
    let source = "function pull(envelope: Envelope): unknown {\n  const { payload = 0 } = envelope;\n  return payload;\n}\n";
    let mut table = TypeTable::new();
    let unknown = table.intern(TypeKind::Unknown);
    let envelope = table.object(vec![Field {
        name: "payload".to_string(),
        type_id: unknown,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let provider = ByName::new(
        source,
        vec![
            ("pull", returning(&mut table, unknown)),
            ("envelope", Answer::Known(envelope)),
        ],
    );
    let file = SourceFile::new("pull.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        tests_undefined(function),
        "unknown carries undefined, so the default has to be reached by a test"
    );
}

#[test]
fn a_default_over_a_read_typed_by_a_type_parameter_still_tests_undefined() {
    let source = "function pull(envelope: Envelope): unknown {\n  const { payload = 0 } = envelope;\n  return payload;\n}\n";
    let mut table = TypeTable::new();
    let unknown = table.intern(TypeKind::Unknown);
    let held = table.fresh_type_parameter();
    let envelope = table.object(vec![Field {
        name: "payload".to_string(),
        type_id: held,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let provider = ByName::new(
        source,
        vec![
            ("pull", returning(&mut table, unknown)),
            ("envelope", Answer::Known(envelope)),
        ],
    );
    let file = SourceFile::new("pull.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        tests_undefined(function),
        "nothing says a type parameter excludes undefined, so the test stays"
    );
}

#[test]
fn an_object_pattern_over_a_union_binds_the_union_of_that_field_in_every_member() {
    let source =
        "function pick(event: Event): string {\n  const { kind } = event;\n  return kind;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let started = table.intern(TypeKind::StringLiteral("started".to_string()));
    let stopped = table.intern(TypeKind::StringLiteral("stopped".to_string()));
    let first = table.object(vec![Field {
        name: "kind".to_string(),
        type_id: started,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let second = table.object(vec![Field {
        name: "kind".to_string(),
        type_id: stopped,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let event = table.union(vec![first, second]);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("event", Answer::Known(event)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let held = table.union(vec![started, stopped]);
    let read = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::ReadField { name, .. },
                ..
            } if name == "kind" => Some(*target),
            _ => None,
        })
        .expect("the pattern should read the field");
    assert_eq!(function.type_of(read), held);
}

#[test]
fn an_object_pattern_over_a_union_is_refused_when_one_member_declares_no_such_field() {
    let source =
        "function pick(event: Event): string {\n  const { kind } = event;\n  return kind;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let started = table.intern(TypeKind::StringLiteral("started".to_string()));
    let first = table.object(vec![Field {
        name: "kind".to_string(),
        type_id: started,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let second = table.object(vec![Field {
        name: "label".to_string(),
        type_id: text,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let event = table.union(vec![first, second]);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, text)),
            ("event", Answer::Known(event)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

#[test]
fn a_default_inside_a_patterned_parameter_branches_out_of_the_entry_block() {
    let source = "function wait({ timeout = 30 }: Route): number {\n  return timeout;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let route = route_shape(&mut table);
    let signature = signature_taking(&mut table, vec![route], number);
    let provider = ByName::new(source, vec![("wait", signature)]);
    let file = SourceFile::new("wait.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.parameters().len(), 1);
    assert!(tests_undefined(function));
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn a_patterned_parameter_after_one_that_branches_still_reads_from_its_own_value() {
    let source = "function wait({ timeout = 30 }: Route, { bytes }: RequestLine): number {\n  return timeout + bytes;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let route = route_shape(&mut table);
    let line = table.object(vec![Field {
        name: "bytes".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let signature = signature_taking(&mut table, vec![route, line], number);
    let provider = ByName::new(source, vec![("wait", signature)]);
    let file = SourceFile::new("wait.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.parameters().len(), 2);
    assert_eq!(function.type_of(function.parameters()[1]), line);
    assert_eq!(function.type_of(returned_value(function)), number);
}

fn counted_by_text(table: &mut TypeTable, readonly: bool) -> savitar_types::TypeId {
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    table.object_with_indexes(
        Vec::new(),
        vec![IndexSignature {
            key: text,
            value: number,
            readonly,
        }],
    )
}

#[test]
fn a_field_only_an_index_signature_declares_is_read_through_it_carrying_undefined() {
    let source = "function lookup(counts: Record<string, number>): number | undefined {\n  return counts.total;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let maybe = table.optional(number);
    let counts = counted_by_text(&mut table, false);
    let provider = ByName::new(
        source,
        vec![
            ("lookup", returning(&mut table, maybe)),
            ("counts", Answer::Known(counts)),
        ],
    );
    let file = SourceFile::new("lookup.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), maybe);
}

#[test]
fn an_object_pattern_over_an_index_signature_binds_the_value_with_undefined() {
    let source = "function lookup(counts: Record<string, number>): number | undefined {\n  const { total } = counts;\n  return total;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let maybe = table.optional(number);
    let counts = counted_by_text(&mut table, false);
    let provider = ByName::new(
        source,
        vec![
            ("lookup", returning(&mut table, maybe)),
            ("counts", Answer::Known(counts)),
        ],
    );
    let file = SourceFile::new("lookup.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let read = only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::ReadField { name, .. },
                ..
            } if name == "total" => Some(*target),
            _ => None,
        })
        .expect("the pattern should read the field through the index signature");
    assert_eq!(function.type_of(read), maybe);
}

#[test]
fn a_readonly_index_signature_answers_the_read_like_a_mutable_one() {
    let source = "function lookup(counts: Record<string, number>): number | undefined {\n  const { total } = counts;\n  return total;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let maybe = table.optional(number);
    let counts = counted_by_text(&mut table, true);
    let provider = ByName::new(
        source,
        vec![
            ("lookup", returning(&mut table, maybe)),
            ("counts", Answer::Known(counts)),
        ],
    );
    let file = SourceFile::new("lookup.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(function.type_of(returned_value(function)), maybe);
}

#[test]
fn a_named_field_wins_over_the_index_signature_that_also_keys_it() {
    let source = "function lookup(counts: Totals): number {\n  const { total } = counts;\n  return total;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let counts = table.object_with_indexes(
        vec![Field {
            name: "total".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        }],
        vec![IndexSignature {
            key: text,
            value: number,
            readonly: false,
        }],
    );
    let provider = ByName::new(
        source,
        vec![
            ("lookup", returning(&mut table, number)),
            ("counts", Answer::Known(counts)),
        ],
    );
    let file = SourceFile::new("lookup.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn a_pattern_key_that_is_a_name_never_reaches_a_numeric_index_signature() {
    let source = "function lookup(labels: Record<number, string>): string | undefined {\n  const { total } = labels;\n  return total;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let maybe = table.optional(text);
    let labels = table.object_with_indexes(
        Vec::new(),
        vec![IndexSignature {
            key: number,
            value: text,
            readonly: false,
        }],
    );
    let provider = ByName::new(
        source,
        vec![
            ("lookup", returning(&mut table, maybe)),
            ("labels", Answer::Known(labels)),
        ],
    );
    let file = SourceFile::new("lookup.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

#[test]
fn an_object_pattern_over_a_union_reads_the_index_signature_of_the_member_without_the_field() {
    let source = "function lookup(bag: Bag): number | undefined {\n  const { total } = bag;\n  return total;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let maybe = table.optional(number);
    let named = table.object(vec![Field {
        name: "total".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let counted = counted_by_text(&mut table, false);
    let bag = table.union(vec![named, counted]);
    let provider = ByName::new(
        source,
        vec![
            ("lookup", returning(&mut table, maybe)),
            ("bag", Answer::Known(bag)),
        ],
    );
    let file = SourceFile::new("lookup.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), maybe);
}

fn incident_union(table: &mut TypeTable) -> savitar_types::TypeId {
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let timeout = table.intern(TypeKind::StringLiteral("timeout".to_string()));
    let crash = table.intern(TypeKind::StringLiteral("crash".to_string()));
    let first = table.object(vec![
        Field {
            name: "kind".to_string(),
            type_id: timeout,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "after".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    let second = table.object(vec![
        Field {
            name: "kind".to_string(),
            type_id: crash,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "message".to_string(),
            type_id: text,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    table.union(vec![first, second])
}

fn rest_target(function: &savitar_ir::Function) -> savitar_ir::ValueId {
    only_block(function)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign {
                target,
                value: Rvalue::Rest { .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .expect("the rest should be taken from the object")
}

#[test]
fn a_rest_over_a_union_is_the_union_of_the_rest_of_every_member() {
    let source = "function body(incident: Incident): unknown {\n  const { kind, ...rest } = incident;\n  return rest;\n}\n";
    let mut table = TypeTable::new();
    let unknown = table.intern(TypeKind::Unknown);
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let incident = incident_union(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("body", returning(&mut table, unknown)),
            ("incident", Answer::Known(incident)),
        ],
    );
    let file = SourceFile::new("body.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    let after = table.object(vec![Field {
        name: "after".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let message = table.object(vec![Field {
        name: "message".to_string(),
        type_id: text,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let kept = table.union(vec![after, message]);
    assert_eq!(function.type_of(rest_target(function)), kept);
}

#[test]
fn a_rest_over_a_union_keeps_the_fields_the_pattern_left_in_each_member_apart() {
    let source = "function body(incident: Incident): unknown {\n  const { kind, ...rest } = incident;\n  return rest;\n}\n";
    let mut table = TypeTable::new();
    let unknown = table.intern(TypeKind::Unknown);
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let incident = incident_union(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("body", returning(&mut table, unknown)),
            ("incident", Answer::Known(incident)),
        ],
    );
    let file = SourceFile::new("body.ts", source);

    let report = lower(&file, &provider, &mut table);

    let function = &report.functions[0];
    let TypeKind::Union(members) = table.kind(function.type_of(rest_target(function))).clone()
    else {
        panic!("the rest over a union should hold a union");
    };
    assert_eq!(members.len(), 2);
    for member in members {
        let TypeKind::Object { fields, .. } = table.kind(member).clone() else {
            panic!("every member of the rest should hold a shape");
        };
        assert_eq!(fields.len(), 1);
        assert!(fields.iter().all(|field| field.name != "kind"));
        assert!(
            fields[0].type_id == number || fields[0].type_id == text,
            "each member keeps only what it declared"
        );
    }
}

#[test]
fn a_rest_over_a_union_is_refused_when_a_member_declares_no_field_the_pattern_took() {
    let source = "function body(incident: Incident): unknown {\n  const { kind, ...rest } = incident;\n  return rest;\n}\n";
    let mut table = TypeTable::new();
    let unknown = table.intern(TypeKind::Unknown);
    let number = table.intern(TypeKind::Number);
    let timeout = table.intern(TypeKind::StringLiteral("timeout".to_string()));
    let first = table.object(vec![
        Field {
            name: "kind".to_string(),
            type_id: timeout,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "after".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    let second = table.object(vec![Field {
        name: "after".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: false,
    }]);
    let incident = table.union(vec![first, second]);
    let provider = ByName::new(
        source,
        vec![
            ("body", returning(&mut table, unknown)),
            ("incident", Answer::Known(incident)),
        ],
    );
    let file = SourceFile::new("body.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
}

#[test]
fn a_rest_over_a_union_is_refused_when_one_member_carries_an_index_signature() {
    let source = "function body(bag: Bag): unknown {\n  const { total, ...rest } = bag;\n  return rest;\n}\n";
    let mut table = TypeTable::new();
    let unknown = table.intern(TypeKind::Unknown);
    let number = table.intern(TypeKind::Number);
    let named = table.object(vec![
        Field {
            name: "total".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "hits".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    let counted = counted_by_text(&mut table, false);
    let bag = table.union(vec![named, counted]);
    let provider = ByName::new(
        source,
        vec![
            ("body", returning(&mut table, unknown)),
            ("bag", Answer::Known(bag)),
        ],
    );
    let file = SourceFile::new("body.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
    assert!(
        report.diagnostics[0].message.contains("index signature"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_rest_over_a_union_is_refused_when_one_member_keeps_a_method() {
    let source = "function body(bag: Bag): unknown {\n  const { total, ...rest } = bag;\n  return rest;\n}\n";
    let mut table = TypeTable::new();
    let unknown = table.intern(TypeKind::Unknown);
    let number = table.intern(TypeKind::Number);
    let reader = table.function(Vec::new(), number);
    let plain = table.object(vec![
        Field {
            name: "total".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "hits".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    let ticking = table.object(vec![
        Field {
            name: "total".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "tick".to_string(),
            type_id: reader,
            optional: false,
            method: true,
            readonly: false,
        },
    ]);
    let bag = table.union(vec![plain, ticking]);
    let provider = ByName::new(
        source,
        vec![
            ("body", returning(&mut table, unknown)),
            ("bag", Answer::Known(bag)),
        ],
    );
    let file = SourceFile::new("body.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
    assert!(
        report.diagnostics[0].message.contains("method"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_read_over_a_union_of_an_array_and_a_readonly_array_is_refused_because_neither_member_carries_a_shape()
 {
    let source = "function size(items: Items): number {\n  return items.length;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let mutable = table.intern(TypeKind::Array(number));
    let frozen = table.intern(TypeKind::ReadonlyArray(number));
    let items = table.union(vec![mutable, frozen]);
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

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
    assert!(
        report.diagnostics[0].message.contains("no declared shape"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_read_over_one_array_alone_still_reaches_the_provider_so_only_the_union_is_measured() {
    let source = "function size(items: readonly number[]): number {\n  return items.length;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let frozen = table.intern(TypeKind::ReadonlyArray(number));
    let provider = ByName::new(
        source,
        vec![
            ("size", returning(&mut table, number)),
            ("items", Answer::Known(frozen)),
            ("length", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("size.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn a_parameter_bound_by_a_pattern_with_a_default_reads_the_pattern_from_the_joined_value() {
    let source = "function pick({ handler }: Route = { handler: \"none\" }): string {\n  return handler;\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let route = route_shape(&mut table);
    let omitted = table.optional(route);
    let signature = signature_taking(&mut table, vec![route], text);
    let provider = ByName::new(source, vec![("pick", signature)]);
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.parameters().len(), 1);
    assert_eq!(function.type_of(function.parameters()[0]), omitted);
    assert!(tests_undefined(function));
    assert_eq!(function.type_of(returned_value(function)), text);
}

fn readonly_field(name: &str, type_id: savitar_types::TypeId) -> Field {
    Field {
        name: name.to_string(),
        type_id,
        optional: false,
        method: false,
        readonly: true,
    }
}

#[test]
fn writing_a_field_every_union_member_types_the_same_way_lowers_to_one_field_write() {
    let source = "function set(holder: Small | Large): void {\n  holder.amount = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let small = table.object(vec![
        named_field("amount", number, false),
        named_field("label", text, false),
    ]);
    let large = table.object(vec![named_field("amount", number, false)]);
    let holder = table.union(vec![small, large]);
    let provider = ByName::new(source, vec![("holder", Answer::Known(holder))]);
    let file = SourceFile::new("set.ts", source);

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
    assert_eq!(name, "amount");
    assert_eq!(function.type_of(*value), number);
}

#[test]
fn writing_a_field_that_differs_between_union_members_is_refused_rather_than_taking_their_union() {
    let source = "function set(holder: Small | Large): void {\n  holder.amount = \"late\";\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let small = table.object(vec![named_field("amount", number, false)]);
    let large = table.object(vec![named_field("amount", text, false)]);
    let holder = table.union(vec![small, large]);
    let provider = ByName::new(source, vec![("holder", Answer::Known(holder))]);
    let file = SourceFile::new("set.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-assignment");
    assert!(
        report.diagnostics[0].message.contains("every member"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn writing_a_field_on_a_union_ignores_a_narrower_answer_from_the_provider() {
    let source = "function set(holder: Small | Large): void {\n  holder.amount = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let small = table.object(vec![named_field("amount", number, false)]);
    let large = table.object(vec![named_field("amount", text, false)]);
    let holder = table.union(vec![small, large]);
    let provider = ByName::new(
        source,
        vec![
            ("holder", Answer::Known(holder)),
            ("amount", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("set.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(
        report.functions.is_empty(),
        "the provider answered the field of one member, and the write must prove both"
    );
    assert_eq!(report.diagnostics[0].rule, "unsupported-assignment");
}

#[test]
fn writing_a_field_one_union_member_does_not_declare_is_refused() {
    let source = "function set(holder: Small | Plain): void {\n  holder.amount = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let small = table.object(vec![named_field("amount", number, false)]);
    let plain = table.object(vec![named_field("label", text, false)]);
    let holder = table.union(vec![small, plain]);
    let provider = ByName::new(
        source,
        vec![
            ("holder", Answer::Known(holder)),
            ("amount", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("set.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0].message.contains("no field `amount`"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn writing_a_field_one_union_member_declares_readonly_is_refused() {
    let source = "function set(holder: Small | Frozen): void {\n  holder.amount = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let small = table.object(vec![named_field("amount", number, false)]);
    let frozen = table.object(vec![readonly_field("amount", number)]);
    let holder = table.union(vec![small, frozen]);
    let provider = ByName::new(source, vec![("holder", Answer::Known(holder))]);
    let file = SourceFile::new("set.ts", source);

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
fn writing_a_field_on_a_union_with_a_member_that_has_no_shape_is_refused_without_the_provider() {
    let source = "function set(holder: Small | string): void {\n  holder.amount = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let small = table.object(vec![named_field("amount", number, false)]);
    let holder = table.union(vec![small, text]);
    let provider = ByName::new(
        source,
        vec![
            ("holder", Answer::Known(holder)),
            ("amount", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("set.ts", source);

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
fn writing_a_value_to_a_field_one_union_member_marks_optional_takes_the_field_type() {
    let source = "function set(holder: Small | Loose): void {\n  holder.amount = 2;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let small = table.object(vec![named_field("amount", number, false)]);
    let loose = table.object(vec![named_field("amount", number, true)]);
    let holder = table.union(vec![small, loose]);
    let provider = ByName::new(source, vec![("holder", Answer::Known(holder))]);
    let file = SourceFile::new("set.ts", source);

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
    assert_eq!(name, "amount");
    assert_eq!(function.type_of(*value), number);
    assert!(
        !table.admits_undefined(function.type_of(*value)),
        "the field is optional in one member, and a write still takes the type it declares"
    );
}

#[test]
fn writing_undefined_to_a_field_another_union_member_requires_is_refused() {
    let source = "function clear(holder: Small | Loose): void {\n  holder.amount = undefined;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let small = table.object(vec![named_field("amount", number, false)]);
    let loose = table.object(vec![named_field("amount", number, true)]);
    let holder = table.union(vec![small, loose]);
    let provider = ByName::new(source, vec![("holder", Answer::Known(holder))]);
    let file = SourceFile::new("clear.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-assignment");
}

#[test]
fn a_compound_write_to_a_field_of_a_union_proves_the_joined_value_against_every_member() {
    let source =
        "function add(holder: Small | Large, more: number): void {\n  holder.amount += more;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let small = table.object(vec![named_field("amount", number, false)]);
    let large = table.object(vec![named_field("amount", text, false)]);
    let holder = table.union(vec![small, large]);
    let provider = ByName::new(
        source,
        vec![
            ("holder", Answer::Known(holder)),
            ("more", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("add.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule == "unsupported-assignment"
                || diagnostic.rule == "unsupported-operator"),
        "{:?}",
        report.diagnostics
    );
}

#[test]
fn a_parameter_with_a_default_admits_undefined_at_the_edge_and_not_in_the_body() {
    let source = "function page(limit: number = 20): number {\n  return limit;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let omitted = table.optional(number);
    let provider = ByName::new(
        source,
        vec![
            ("page", returning(&mut table, number)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("page.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.parameters().len(), 1);
    assert_eq!(
        function.type_of(function.parameters()[0]),
        omitted,
        "the call may omit the argument, so the parameter admits undefined"
    );
    assert!(tests_undefined(function));
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn a_parameter_default_that_does_not_fit_the_annotation_is_held_as_a_union_rather_than_coerced() {
    let source = "function page(limit: number = \"ten\"): number | string {\n  return limit;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![number, text]);
    let provider = ByName::new(
        source,
        vec![
            ("page", returning(&mut table, either)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("page.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), either);
}

#[test]
fn a_parameter_after_one_with_a_default_still_reads_from_its_own_value() {
    let source = "function page(limit: number = 20, offset: number): number {\n  return limit + offset;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("page", returning(&mut table, number)),
            ("limit", Answer::Known(number)),
            ("offset", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("page.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.parameters().len(), 2);
    assert_eq!(function.type_of(function.parameters()[1]), number);
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn a_parameter_that_only_admits_undefined_through_its_default_is_narrowed_before_it_is_read() {
    let source = "function page(limit: number = 20): number {\n  return limit + 1;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("page", returning(&mut table, number)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("page.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(
        report.diagnostics.is_empty(),
        "adding to the parameter proves it is no longer optional: {:?}",
        report.diagnostics
    );
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), number);
}

fn taking_one_optional(
    table: &mut TypeTable,
    takes: savitar_types::TypeId,
    returns: savitar_types::TypeId,
) -> Answer {
    Answer::Known(table.function(
        vec![savitar_types::Parameter {
            type_id: takes,
            rest: false,
            optional: true,
        }],
        returns,
    ))
}

#[test]
fn a_call_that_omits_the_argument_a_default_fills_is_lowered() {
    let source = "function callIt(): number {\n  return given();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let given = taking_one_optional(&mut table, number, number);
    let provider = ByName::new(
        source,
        vec![("callIt", returning(&mut table, number)), ("given", given)],
    );
    let file = SourceFile::new("callIt.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn passing_undefined_where_a_default_waits_is_lowered_like_omitting_the_argument() {
    let source = "function callIt(): number {\n  return given(undefined);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let given = taking_one_optional(&mut table, number, number);
    let provider = ByName::new(
        source,
        vec![("callIt", returning(&mut table, number)), ("given", given)],
    );
    let file = SourceFile::new("callIt.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), number);
}

#[test]
fn passing_undefined_where_the_parameter_is_required_is_still_refused() {
    let source = "function callIt(): number {\n  return given(undefined);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let given = Answer::Known(table.function(
        vec![savitar_types::Parameter {
            type_id: number,
            rest: false,
            optional: false,
        }],
        number,
    ));
    let provider = ByName::new(
        source,
        vec![("callIt", returning(&mut table, number)), ("given", given)],
    );
    let file = SourceFile::new("callIt.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-argument");
}

#[test]
fn writing_undefined_to_an_optional_field_is_refused_because_a_write_takes_the_field_type() {
    let source = "function clear(route: Route): void {\n  route.timeout = undefined;\n}\n";
    let mut table = TypeTable::new();
    let route = route_shape(&mut table);
    let provider = ByName::new(source, vec![("route", Answer::Known(route))]);
    let file = SourceFile::new("clear.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-assignment");
}

#[test]
fn writing_a_value_to_an_optional_field_is_lowered_with_the_type_the_field_declares() {
    let source = "function set(route: Route): void {\n  route.timeout = 5;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let route = route_shape(&mut table);
    let provider = ByName::new(source, vec![("route", Answer::Known(route))]);
    let file = SourceFile::new("set.ts", source);

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
    assert_eq!(name, "timeout");
    assert_eq!(function.type_of(*value), number);
}

#[test]
fn a_case_that_runs_into_the_next_one_reaches_it_as_a_join() {
    let source = "function pick(kind: string, value: number): number {\n  switch (kind) {\n    case \"a\":\n      value = 1;\n    case \"b\":\n      return value;\n  }\n  return value;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, number)),
            ("kind", Answer::Known(text)),
            ("value", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    assert_eq!(
        carrying.len(),
        1,
        "only the case run into from the one before it carries a parameter"
    );
    let (second, parameters) = carrying[0];
    assert_eq!(
        parameters, 1,
        "`value` arrives as a parameter, because the two paths into the case disagree on it"
    );
    assert!(
        matches!(
            function.block(second).terminator,
            Terminator::Return(Some(_))
        ),
        "the parameter lands on the body of the second case, which returns"
    );
    assert_eq!(
        blocks_jumping_to(function, second),
        vec![1, 1],
        "the label of the second case and the fall through from the first both carry `value`"
    );
}

#[test]
fn a_default_run_into_from_the_case_before_it_narrows_nothing() {
    let source = "function size(frame: Frame, fallback: number): number {\n  let chosen = fallback;\n  switch (frame.kind) {\n    case \"text\":\n      chosen = fallback + 1;\n    default:\n      return chosen;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("size", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("fallback", Answer::Known(number)),
            ("kind", Answer::Known(shapes.tags)),
        ],
    );
    let file = SourceFile::new("size.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        !narrowed(function).contains(&shapes.binary),
        "the default is reached by a path that matched `text` as well, so the negative sense of the labels decides nothing"
    );
}

#[test]
fn an_empty_case_that_runs_into_the_next_one_still_grows_no_parameter() {
    let source = "function pick(kind: string, value: number): number {\n  switch (kind) {\n    case \"a\":\n    case \"b\":\n      return value;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("pick", returning(&mut table, number)),
            ("kind", Answer::Known(text)),
            ("value", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(
        parameterised(function),
        vec![],
        "nothing ran in the empty case, so the one it runs into needs no parameter"
    );
}

#[test]
fn a_for_with_no_test_runs_its_body_straight_from_the_header() {
    let source = "function spin(limit: number): number {\n  let sum = 0;\n  for (;;) {\n    sum = sum + limit;\n    break;\n  }\n  return sum;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("spin", returning(&mut table, number)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("spin.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let header = parameterised(function)[0].0;
    assert!(
        operators_in(function, header).contains(&BinaryOp::Add),
        "with no test to lower, the body starts in the header itself"
    );
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![1],
        "the body always breaks, so nothing comes back to the header"
    );
}

#[test]
fn a_continue_naming_a_label_nobody_wrote_is_refused() {
    let source = "function scan(limit: number): number {\n  let seen = 0;\n  while (seen < limit) {\n    seen = seen + 1;\n    continue outer;\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("limit", Answer::Known(number))]);
    let file = SourceFile::new("scan.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("`continue outer` names no label")),
        "{:?}",
        report.diagnostics
    );
}

#[test]
fn a_break_naming_a_label_nobody_wrote_is_refused() {
    let source = "function scan(limit: number): number {\n  let seen = 0;\n  while (seen < limit) {\n    break outer;\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("limit", Answer::Known(number))]);
    let file = SourceFile::new("scan.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("`break outer` names no label")),
        "{:?}",
        report.diagnostics
    );
}

#[test]
fn a_labelled_break_inside_the_inner_loop_leaves_the_outer_one() {
    let source = "function pairs(rows: readonly number[], limit: number): number {\n  let seen = 0;\n  outer: for (const row of rows) {\n    let inner = 0;\n    while (inner < limit) {\n      inner = inner + 1;\n      if (inner === row) {\n        seen = inner;\n        break outer;\n      }\n    }\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let rows = table.intern(TypeKind::ReadonlyArray(number));
    let provider = ByName::new(
        source,
        vec![
            ("rows", Answer::Known(rows)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pairs.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let (outer_exit, parameters) = function
        .blocks()
        .find(|(_, block)| matches!(block.terminator, Terminator::Return(_)))
        .map(|(id, block)| (id, block.parameters.len()))
        .expect("the function returns from the outer loop exit");
    assert_eq!(
        parameters, 1,
        "`seen` leaves the outer loop as a parameter, because the two ways out disagree"
    );
    assert_eq!(
        blocks_jumping_to(function, outer_exit),
        vec![1, 1],
        "the outer test falling false and the labelled `break` both carry `seen`"
    );
}

#[test]
fn a_labelled_continue_inside_the_inner_loop_repeats_the_outer_one() {
    let source = "function pairs(rows: readonly number[], limit: number): number {\n  let seen = 0;\n  outer: for (const row of rows) {\n    let inner = 0;\n    while (inner < limit) {\n      inner = inner + 1;\n      if (inner === row) {\n        seen = seen + 1;\n        continue outer;\n      }\n    }\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let rows = table.intern(TypeKind::ReadonlyArray(number));
    let provider = ByName::new(
        source,
        vec![
            ("rows", Answer::Known(rows)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("pairs.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let latch = parameterised(function)
        .into_iter()
        .find(|(_, count)| *count == 2)
        .map(|(id, _)| id)
        .expect("the outer latch carries the index and `seen`");
    assert_eq!(
        blocks_jumping_to(function, latch).len(),
        2,
        "the inner loop falling through and the labelled `continue` both reach the outer latch"
    );
}

#[test]
fn a_labelled_break_inside_a_switch_leaves_the_loop_the_label_names() {
    let source = "function classify(kind: string, limit: number): number {\n  let seen = 0;\n  outer: while (seen < limit) {\n    switch (kind) {\n      case \"stop\":\n        break outer;\n      default:\n        seen = seen + 1;\n    }\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let provider = ByName::new(
        source,
        vec![
            ("kind", Answer::Known(text)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("classify.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let header = parameterised(function)[0].0;
    assert_eq!(
        blocks_jumping_to(function, header).len(),
        2,
        "the entry and the back edge reach the header, and the labelled `break` does not"
    );
}

#[test]
fn a_break_naming_the_label_on_a_plain_block_leaves_that_block() {
    let source = "function pick(flag: boolean, left: number, right: number): number {\n  let chosen = left;\n  found: {\n    if (flag) {\n      chosen = right;\n      break found;\n    }\n    chosen = left + right;\n  }\n  return chosen;\n}\n";
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

    let joins = parameterised(function);
    assert_eq!(
        joins.len(),
        1,
        "only the end of the labelled block joins, and it carries `chosen`"
    );
    assert_eq!(
        blocks_jumping_to(function, joins[0].0),
        vec![1, 1],
        "the labelled `break` and the fall through both carry `chosen`"
    );
}

#[test]
fn a_bare_break_inside_a_labelled_block_still_leaves_the_loop_around_it() {
    let source = "function scan(limit: number): number {\n  let seen = 0;\n  while (seen < limit) {\n    step: {\n      seen = seen + 1;\n      break;\n    }\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("limit", Answer::Known(number))]);
    let file = SourceFile::new("scan.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let header = parameterised(function)[0].0;
    assert_eq!(
        blocks_jumping_to(function, header),
        vec![1],
        "the body always breaks out of the loop, so no back edge reaches the header"
    );
}

#[test]
fn a_continue_naming_a_label_on_a_plain_block_is_refused() {
    let source = "function pick(flag: boolean): number {\n  found: {\n    continue found;\n  }\n  return 1;\n}\n";
    let mut table = TypeTable::new();
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(source, vec![("flag", Answer::Known(boolean))]);
    let file = SourceFile::new("pick.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("is not on a loop")),
        "{:?}",
        report.diagnostics
    );
}

#[test]
fn a_label_written_again_inside_itself_is_refused() {
    let source = "function scan(limit: number): number {\n  let seen = 0;\n  outer: while (seen < limit) {\n    outer: while (seen < limit) {\n      break outer;\n    }\n  }\n  return seen;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("limit", Answer::Known(number))]);
    let file = SourceFile::new("scan.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(
        report.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("already inside one with that label")),
        "{:?}",
        report.diagnostics
    );
}

#[test]
fn a_break_out_of_a_block_that_shadows_a_name_leaves_the_outer_value_alone() {
    let source = "function shade(start: number, step: number, flag: boolean): number {\n  let value = start;\n  while (flag) {\n    {\n      let value = step;\n      value = value + 1;\n      break;\n    }\n  }\n  return value;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("shade", returning(&mut table, number)),
            ("start", Answer::Known(number)),
            ("step", Answer::Known(number)),
            ("flag", Answer::Known(boolean)),
        ],
    );
    let file = SourceFile::new("shade.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let carrying = parameterised(function);
    assert_eq!(
        carrying.len(),
        1,
        "the block stepped its own `value`, so the two ways out of the loop agree on the outer one"
    );
    let (header, _) = carrying[0];
    assert_eq!(
        returned_value(function),
        function.block(header).parameters[0],
        "the outer `value` leaves the loop as the header carried it in"
    );
}

#[test]
fn a_continue_from_a_block_that_shadows_a_name_carries_the_outer_value_back() {
    let source = "function shade(start: number, step: number, flag: boolean): number {\n  let value = start;\n  while (flag) {\n    {\n      let value = step;\n      value = value + 1;\n      continue;\n    }\n  }\n  return value;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let provider = ByName::new(
        source,
        vec![
            ("shade", returning(&mut table, number)),
            ("start", Answer::Known(number)),
            ("step", Answer::Known(number)),
            ("flag", Answer::Known(boolean)),
        ],
    );
    let file = SourceFile::new("shade.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let (header, _) = parameterised(function)[0];
    let carried = function.block(header).parameters[0];
    let back: Vec<Vec<savitar_ir::ValueId>> = function
        .blocks()
        .filter_map(|(_, block)| match &block.terminator {
            Terminator::Jump { target, arguments } if *target == header => Some(arguments.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        back.len(),
        2,
        "the entry and the `continue` reach the header"
    );
    assert_eq!(
        back[1],
        vec![carried],
        "the `continue` carries the outer `value` back untouched, not the one the block declared"
    );
}

#[test]
fn a_for_with_no_test_leaves_only_through_its_breaks() {
    let source = "function drainUntil(queue: readonly string[], stop: string): number {\n  let index = 0;\n  for (;;) {\n    const next = queue[index];\n    if (next === undefined) {\n      break;\n    }\n    index = index + 1;\n    if (next === stop) {\n      break;\n    }\n  }\n  return index;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let queue = table.intern(TypeKind::ReadonlyArray(text));
    let provider = ByName::new(
        source,
        vec![
            ("drainUntil", returning(&mut table, number)),
            ("queue", Answer::Known(queue)),
            ("stop", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("drain.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let (exit, parameters) = function
        .blocks()
        .find(|(_, block)| matches!(block.terminator, Terminator::Return(_)))
        .map(|(id, block)| (id, block.parameters.len()))
        .expect("the function returns from the loop exit");
    assert_eq!(
        parameters, 1,
        "the two `break`s disagree on `index`, so it leaves as a parameter"
    );
    assert_eq!(
        blocks_jumping_to(function, exit),
        vec![1, 1],
        "only the two `break`s reach the exit, because there is no test to fall false"
    );
}

#[test]
fn a_for_with_no_test_and_no_break_leaves_its_exit_unreachable() {
    let source = "function nextSlot(used: readonly boolean[], from: number): number {\n  let at = from;\n  for (;;) {\n    if (at >= used.length) {\n      return 0;\n    }\n    if (used[at] === false) {\n      return at;\n    }\n    at = at + 1;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let used = table.intern(TypeKind::ReadonlyArray(boolean));
    let provider = ByName::new(
        source,
        vec![
            ("nextSlot", returning(&mut table, number)),
            ("used", Answer::Known(used)),
            ("from", Answer::Known(number)),
            ("length", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("slot.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let dominance = savitar_ir::Dominance::of(function);
    let stranded: Vec<savitar_ir::BlockId> = function
        .blocks()
        .map(|(id, _)| id)
        .filter(|&id| !dominance.reachable(id))
        .collect();
    assert_eq!(
        stranded.len(),
        1,
        "the loop never ends, so the exit alone is stranded and the verifier stays clean"
    );
    assert!(
        function.block(stranded[0]).parameters.is_empty(),
        "the stranded exit takes no parameter, because no edge would carry one"
    );
}

#[test]
fn a_for_with_no_test_still_runs_its_update_through_the_latch() {
    let source = "function seek(limit: number): number {\n  let at = 0;\n  for (;; at = at + 1) {\n    if (at >= limit) {\n      break;\n    }\n  }\n  return at;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("seek", returning(&mut table, number)),
            ("limit", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("seek.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);

    let latch = parameterised(function)
        .into_iter()
        .find(|&(id, _)| operators_in(function, id).contains(&BinaryOp::Add))
        .map(|(id, _)| id)
        .expect("the update runs in the latch");
    assert!(
        matches!(function.block(latch).terminator, Terminator::Jump { .. }),
        "the latch jumps straight back to the header, because there is no test to branch on"
    );
}

fn constructing_shape(
    table: &mut TypeTable,
    parameters: Vec<savitar_types::TypeId>,
    instance: savitar_types::TypeId,
) -> savitar_types::TypeId {
    let taken = parameters
        .into_iter()
        .map(|type_id| savitar_types::Parameter {
            type_id,
            rest: false,
            optional: false,
        })
        .collect();
    let form = table.function(taken, instance);
    table.constructor(vec![form])
}

fn held_field(table: &mut TypeTable, readonly: bool) -> savitar_types::TypeId {
    let number = table.intern(TypeKind::Number);
    table.object(vec![Field {
        name: "held".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly,
    }])
}

#[test]
fn a_method_lowers_with_this_typed_by_the_shape_of_its_class() {
    let source = "class Counter {\n  reading(): number {\n    return this.held;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, false);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("reading", returning(&mut table, number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
    let function = &report.functions[0];
    assert_eq!(function.name(), "Counter.reading");
    assert_eq!(
        function.parameters().len(),
        1,
        "the receiver is the only parameter"
    );
    assert_eq!(function.type_of(function.parameters()[0]), shape);
    assert_eq!(verify(function), vec![]);
    let entry = function.entry().expect("the method should have a block");
    let Terminator::Return(Some(returned)) = function.block(entry).terminator else {
        panic!("expected a return");
    };
    assert_eq!(table.kind(function.type_of(returned)), &TypeKind::Number);
}

#[test]
fn a_constructor_lowers_and_writes_the_field_its_class_declares() {
    let source =
        "class Counter {\n  constructor(start: number) {\n    this.held = start;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, false);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
    let function = &report.functions[0];
    assert_eq!(function.name(), "Counter.constructor");
    assert_eq!(
        function.parameters().len(),
        2,
        "the receiver comes before the written parameters"
    );
    assert_eq!(verify(function), vec![]);
    let entry = function
        .entry()
        .expect("the constructor should have a block");
    let written = function
        .block(entry)
        .statements
        .iter()
        .filter(
            |statement| matches!(statement, Statement::WriteField { name, .. } if name == "held"),
        )
        .count();
    assert_eq!(written, 1);
    assert!(matches!(
        function.block(entry).terminator,
        Terminator::Return(None)
    ));
}

#[test]
fn writing_a_readonly_field_inside_the_constructor_of_its_class_is_allowed() {
    let source =
        "class Counter {\n  constructor(start: number) {\n    this.held = start;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, true);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
}

#[test]
fn writing_a_readonly_field_a_base_class_declares_is_refused_in_the_constructor_of_the_heir() {
    let source = "class Counter extends Base {\n  constructor(start: number) {\n    super();\n    this.held = start;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, true);
    let base = constructing_shape(&mut table, Vec::new(), shape);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("Base", Answer::Known(base)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0]
            .message
            .contains("declared readonly by a base class"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn writing_a_readonly_field_the_derived_class_declares_itself_lowers_in_its_constructor() {
    let source = "class Counter extends Base {\n  constructor(start: number) {\n    super();\n    this.held = start;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, true);
    let nothing = table.object(Vec::new());
    let base = constructing_shape(&mut table, Vec::new(), nothing);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("Base", Answer::Known(base)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
}

#[test]
fn writing_a_mutable_field_in_the_constructor_of_a_derived_class_still_lowers() {
    let source = "class Counter extends Base {\n  constructor(start: number) {\n    super();\n    this.held = start;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, false);
    let base = constructing_shape(&mut table, Vec::new(), shape);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("Base", Answer::Known(base)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
}

#[test]
fn the_receiver_keeps_its_identity_inside_a_loop_so_a_readonly_write_there_is_still_allowed() {
    let source = "class Counter {\n  constructor(start: number) {\n    while (start > 0) {\n      this.held = start;\n      start = start - 1;\n    }\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, true);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
    assert_eq!(verify(&report.functions[0]), vec![]);
}

#[test]
fn writing_a_readonly_field_of_another_object_inside_the_constructor_is_refused() {
    let source = "class Counter {\n  constructor(other: Counter, start: number) {\n    other.held = start;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, true);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("other", Answer::Known(shape)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
}

#[test]
fn writing_a_readonly_field_from_a_method_that_is_not_the_constructor_is_still_refused() {
    let source =
        "class Counter {\n  reset(start: number): void {\n    this.held = start;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let nothing = table.intern(TypeKind::Void);
    let shape = held_field(&mut table, true);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("reset", returning(&mut table, nothing)),
            ("start", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-write");
    assert!(
        report.diagnostics[0].message.contains("readonly"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_method_reached_through_this_is_called_because_the_shape_declares_it() {
    let source = "class Middleware {\n  describe(): string {\n    return this.named();\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let naming = table.function(Vec::new(), text);
    let shape = table.object(vec![Field {
        name: "named".to_string(),
        type_id: naming,
        optional: false,
        method: true,
        readonly: false,
    }]);
    let provider = ByName::new(
        source,
        vec![
            ("Middleware", Answer::Known(shape)),
            ("describe", returning(&mut table, text)),
        ],
    );
    let file = SourceFile::new("middleware.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(function.name(), "Middleware.describe");
    let entry = function.entry().expect("the method should have a block");
    assert!(
        function
            .block(entry)
            .statements
            .iter()
            .any(|statement| matches!(
                statement,
                Statement::Assign {
                    value: Rvalue::Call { .. },
                    ..
                }
            )),
        "the method call through `this` should lower to a call"
    );
}

#[test]
fn this_in_a_static_method_is_refused_because_nothing_binds_it() {
    let source = "class Counter {\n  static reset(): number {\n    return this.held;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("reset", returning(&mut table, number))]);
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert!(
        report.diagnostics[0]
            .message
            .contains("`this` is not bound"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_base_constructor_call_lowers_to_a_base_construction_on_the_receiver() {
    let source = "class Logger extends Middleware {\n  constructor() {\n    super();\n  }\n}\n";
    let mut table = TypeTable::new();
    let shape = table.object(Vec::new());
    let base = constructing_shape(&mut table, Vec::new(), shape);
    let provider = ByName::new(
        source,
        vec![
            ("Logger", Answer::Known(shape)),
            ("Middleware", Answer::Known(base)),
        ],
    );
    let file = SourceFile::new("logger.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
    let function = &report.functions[0];
    assert_eq!(function.name(), "Logger.constructor");
    assert_eq!(verify(function), vec![]);

    let receiver = function.parameters()[0];
    let entry = function.entry().expect("a block");
    let constructed = function
        .block(entry)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::ConstructBase {
                base_class,
                object,
                arguments,
                ..
            } => Some((base_class.clone(), *object, arguments.clone())),
            _ => None,
        })
        .expect("the base construction should be lowered");
    assert_eq!(constructed.0, "Middleware");
    assert_eq!(constructed.1, receiver);
    assert!(constructed.2.is_empty());
}

#[test]
fn an_argument_to_a_base_constructor_is_proved_against_the_parameters_of_the_base() {
    let source = "class Logger extends Middleware {\n  constructor(prefix: string) {\n    super(prefix);\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let shape = table.object(Vec::new());
    let base = constructing_shape(&mut table, vec![text], shape);
    let provider = ByName::new(
        source,
        vec![
            ("Logger", Answer::Known(shape)),
            ("Middleware", Answer::Known(base)),
            ("prefix", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("logger.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
    let function = &report.functions[0];
    let entry = function.entry().expect("a block");
    let passed = function
        .block(entry)
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::ConstructBase { arguments, .. } => Some(arguments.clone()),
            _ => None,
        })
        .expect("the base construction should be lowered");
    assert_eq!(passed.len(), 1);
    assert_eq!(table.kind(function.type_of(passed[0])), &TypeKind::String);
}

#[test]
fn an_argument_to_a_base_constructor_that_the_base_does_not_take_is_refused() {
    let source = "class Logger extends Middleware {\n  constructor(prefix: string) {\n    super(prefix);\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let number = table.intern(TypeKind::Number);
    let shape = table.object(Vec::new());
    let base = constructing_shape(&mut table, vec![number], shape);
    let provider = ByName::new(
        source,
        vec![
            ("Logger", Answer::Known(shape)),
            ("Middleware", Answer::Known(base)),
            ("prefix", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("logger.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    let at = file.line_col(report.diagnostics[0].span.start);
    assert_eq!((at.line, at.column), (3, 5));
}

#[test]
fn a_base_constructor_call_that_is_not_the_first_statement_is_refused() {
    let source = "class Logger extends Middleware {\n  constructor(prefix: string) {\n    const kept = prefix;\n    super();\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let shape = table.object(Vec::new());
    let base = constructing_shape(&mut table, Vec::new(), shape);
    let provider = ByName::new(
        source,
        vec![
            ("Logger", Answer::Known(shape)),
            ("Middleware", Answer::Known(base)),
            ("prefix", Answer::Known(text)),
            ("kept", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("logger.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-class-member");
    assert!(
        report.diagnostics[0].message.contains("first statement"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_derived_constructor_that_never_calls_the_base_constructor_is_refused() {
    let source = "class Logger extends Middleware {\n  constructor() {\n  }\n}\n";
    let mut table = TypeTable::new();
    let shape = table.object(Vec::new());
    let base = constructing_shape(&mut table, Vec::new(), shape);
    let provider = ByName::new(
        source,
        vec![
            ("Logger", Answer::Known(shape)),
            ("Middleware", Answer::Known(base)),
        ],
    );
    let file = SourceFile::new("logger.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-class-member");
}

#[test]
fn calling_the_base_constructor_twice_is_refused() {
    let source = "class Logger extends Middleware {\n  constructor() {\n    super();\n    super();\n  }\n}\n";
    let mut table = TypeTable::new();
    let shape = table.object(Vec::new());
    let base = constructing_shape(&mut table, Vec::new(), shape);
    let provider = ByName::new(
        source,
        vec![
            ("Logger", Answer::Known(shape)),
            ("Middleware", Answer::Known(base)),
        ],
    );
    let file = SourceFile::new("logger.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert!(
        report.diagnostics[0].message.contains("more than once"),
        "{}",
        report.diagnostics[0].message
    );
    let at = file.line_col(report.diagnostics[0].span.start);
    assert_eq!((at.line, at.column), (4, 5));
}

#[test]
fn a_base_constructor_call_in_a_method_is_refused() {
    let source = "class Logger extends Middleware {\n  reset(): void {\n    super();\n  }\n}\n";
    let mut table = TypeTable::new();
    let shape = table.object(Vec::new());
    let base = constructing_shape(&mut table, Vec::new(), shape);
    let nothing = table.intern(TypeKind::Void);
    let provider = ByName::new(
        source,
        vec![
            ("Logger", Answer::Known(shape)),
            ("Middleware", Answer::Known(base)),
            ("reset", returning(&mut table, nothing)),
        ],
    );
    let file = SourceFile::new("logger.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0]
            .message
            .contains("inside a constructor"),
        "{}",
        report.diagnostics[0].message
    );
    let at = file.line_col(report.diagnostics[0].span.start);
    assert_eq!((at.line, at.column), (3, 5));
}

#[test]
fn a_base_constructor_call_used_as_a_value_is_refused() {
    let source = "class Logger extends Middleware {\n  constructor() {\n    super();\n    const made = super();\n  }\n}\n";
    let mut table = TypeTable::new();
    let shape = table.object(Vec::new());
    let base = constructing_shape(&mut table, Vec::new(), shape);
    let provider = ByName::new(
        source,
        vec![
            ("Logger", Answer::Known(shape)),
            ("Middleware", Answer::Known(base)),
        ],
    );
    let file = SourceFile::new("logger.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0].message.contains("yields no value"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_base_class_that_is_not_a_name_is_refused_rather_than_constructed_by_guess() {
    let source =
        "class Logger extends mixin(Middleware) {\n  constructor() {\n    super();\n  }\n}\n";
    let mut table = TypeTable::new();
    let shape = table.object(Vec::new());
    let provider = ByName::new(source, vec![("Logger", Answer::Known(shape))]);
    let file = SourceFile::new("logger.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-class");
    assert!(
        report.diagnostics[0].message.contains("not a name"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_base_name_that_answers_no_construct_signature_is_refused() {
    let source = "class Logger extends Middleware {\n  constructor() {\n    super();\n  }\n}\n";
    let mut table = TypeTable::new();
    let shape = table.object(Vec::new());
    let provider = ByName::new(
        source,
        vec![
            ("Logger", Answer::Known(shape)),
            ("Middleware", Answer::Known(shape)),
        ],
    );
    let file = SourceFile::new("logger.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-class-member");
    assert!(
        report.diagnostics[0]
            .message
            .contains("no way to construct"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_parameter_property_is_refused_because_it_declares_a_field_as_well() {
    let source = "class Counter {\n  constructor(private readonly held: number) {}\n}\n";
    let mut table = TypeTable::new();
    let shape = held_field(&mut table, true);
    let provider = ByName::new(source, vec![("Counter", Answer::Known(shape))]);
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
    assert!(
        report.diagnostics[0].message.contains("parameter property"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_destructuring_parameter_of_a_constructor_is_refused_because_no_position_answers_it() {
    let source =
        "class Counter {\n  constructor({ start }: Spot) {\n    this.held = start;\n  }\n}\n";
    let mut table = TypeTable::new();
    let shape = held_field(&mut table, false);
    let provider = ByName::new(source, vec![("Counter", Answer::Known(shape))]);
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
    assert!(
        report.diagnostics[0]
            .message
            .contains("construct signature"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_written_this_parameter_is_refused_rather_than_ignored() {
    let source = "function run(this: Counter, by: number): number {\n  return by;\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("run.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-function");
    assert!(
        report.diagnostics[0].message.contains("`this` parameter"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_class_whose_name_answers_no_shape_refuses_its_methods_instead_of_guessing() {
    let source = "class Counter {\n  reading(): number {\n    return 1;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(number)),
            ("reading", returning(&mut table, number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-class-member");
    assert!(
        report.diagnostics[0].message.contains("no shape"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_abstract_method_signature_is_no_body_to_lower() {
    let source = "abstract class Middleware {\n  abstract name(): string;\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("abstract.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn an_overload_signature_in_a_class_is_no_body_to_lower() {
    let source = "class Reader {\n  read(at: string): string;\n  read(at: string): string;\n  read(at: string): string {\n    return at;\n  }\n}\n";
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let shape = table.object(Vec::new());
    let provider = ByName::new(
        source,
        vec![
            ("Reader", Answer::Known(shape)),
            ("read", returning(&mut table, text)),
            ("at", Answer::Known(text)),
        ],
    );
    let file = SourceFile::new("reader.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        report.functions.len(),
        1,
        "only the member that carries a body is lowered"
    );
    assert_eq!(report.functions[0].name(), "Reader.read");
}

#[test]
fn a_field_with_an_initialiser_is_refused_rather_than_skipped() {
    let source = "class Counter {\n  value: number = 0;\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("field.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-class-member");
    assert!(
        report.diagnostics[0].message.contains("field initialiser"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_field_with_no_initialiser_is_no_body_to_lower() {
    let source = "class Counter {\n  private value: number;\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("declared.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn a_static_initialisation_block_is_refused_rather_than_skipped() {
    let source = "class Counter {\n  static {\n    const at = 1;\n  }\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("block.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert_eq!(report.diagnostics.len(), 1);
    assert!(
        report.diagnostics[0]
            .message
            .contains("static initialisation block"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_accessor_is_refused_rather_than_skipped() {
    let source = "class Counter {\n  get value(): number {\n    return 1;\n  }\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("accessor.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert_eq!(report.diagnostics.len(), 1);
    assert!(
        report.diagnostics[0].message.contains("accessor"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_computed_member_name_is_refused_rather_than_skipped() {
    let source =
        "const key = \"run\";\nclass Counter {\n  [key](): number {\n    return 1;\n  }\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("computed.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert_eq!(report.diagnostics.len(), 1);
    assert!(
        report.diagnostics[0]
            .message
            .contains("computed or private"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_class_with_no_name_is_refused_because_its_shape_is_asked_for_by_name() {
    let source = "export default class {\n  run(): number {\n    return 1;\n  }\n}\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("nameless.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-class");
}

#[test]
fn a_default_exported_function_with_a_name_lowers_like_any_other() {
    let source = "export default function handle(at: number): number {\n  return at;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(
        source,
        vec![
            ("handle", returning(&mut table, number)),
            ("at", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("handle.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
    assert_eq!(report.functions[0].name(), "handle");
    assert_eq!(verify(&report.functions[0]), vec![]);
}

#[test]
fn a_default_exported_function_with_no_name_is_refused_rather_than_skipped() {
    let source = "export default function (at: number): number {\n  return at;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("at", Answer::Known(number))]);
    let file = SourceFile::new("nameless.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-function");
    assert!(
        report.diagnostics[0].message.contains("no name"),
        "{}",
        report.diagnostics[0].message
    );
    let at = file.line_col(report.diagnostics[0].span.start);
    assert_eq!((at.line, at.column), (1, 16));
}

#[test]
fn a_default_exported_class_with_a_name_has_its_members_reached() {
    let source =
        "export default class Counter {\n  reading(): number {\n    return this.held;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, false);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("reading", returning(&mut table, number)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 1);
    assert_eq!(report.functions[0].name(), "Counter.reading");
}

#[test]
fn a_default_exported_arrow_function_stays_the_refusal_it_already_was() {
    let source = "export default (at: number): number => at;\n";
    let mut table = TypeTable::new();
    let provider = ByName::new(source, vec![]);
    let file = SourceFile::new("arrow.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn a_rest_parameter_is_refused_rather_than_dropped() {
    let source = "function many(...values: number[]): number {\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let numbers = table.intern(TypeKind::Array(number));
    let rest = savitar_types::Parameter {
        type_id: numbers,
        rest: true,
        optional: false,
    };
    let signature = Answer::Known(table.function(vec![rest], number));
    let provider = ByName::new(
        source,
        vec![("many", signature), ("values", Answer::Known(numbers))],
    );
    let file = SourceFile::new("many.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
    assert!(
        report.diagnostics[0].message.contains("rest parameter"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_discriminant_narrowing_is_forgotten_after_a_call_that_could_write_the_field() {
    let source = "function use(frame: Frame): number {\n  if (frame.kind === \"binary\") {\n    audit();\n    return frame.size;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
            ("audit", returning(&mut table, number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
    assert!(
        report.diagnostics[0].message.contains("size"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn a_discriminant_narrowing_still_types_the_argument_of_the_call_that_forgets_it() {
    let source = "function use(frame: Frame): number {\n  if (frame.kind === \"binary\") {\n    return sizeOf(frame);\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let size_of = taking(&mut table, shapes.binary, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
            ("sizeOf", Answer::Known(size_of)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        narrowed(function).contains(&shapes.binary),
        "the argument is proved against the member the test named, before the call forgets it"
    );
}

#[test]
fn a_discriminant_narrowing_in_a_switch_case_is_forgotten_after_a_call() {
    let source = "function use(frame: Frame): number {\n  switch (frame.kind) {\n    case \"binary\":\n      audit();\n      return frame.size;\n    default:\n      return 0;\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
            ("audit", returning(&mut table, number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
}

#[test]
fn an_in_narrowing_is_forgotten_after_a_call_that_could_write_the_field() {
    let source = "function use(frame: Frame): number {\n  if (\"size\" in frame) {\n    audit();\n    return frame.size;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("audit", returning(&mut table, number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
}

#[test]
fn an_await_forgets_field_narrowing_because_other_code_runs_while_it_is_suspended() {
    let source = "async function use(frame: Frame): Promise<number> {\n  if (frame.kind === \"binary\") {\n    const ready = await later();\n    return frame.size;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let promised = table.intern(TypeKind::Promise(number));
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, promised)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
            ("later", returning(&mut table, promised)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-read");
}

#[test]
fn a_typeof_narrowing_survives_a_call_because_no_call_can_rebind_a_name() {
    let source = "function use(value: string | number, extra: number): number {\n  if (typeof value === \"number\") {\n    audit();\n    return value + extra;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let (number, either) = text_or_number(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("value", Answer::Known(either)),
            ("extra", Answer::Known(number)),
            ("audit", returning(&mut table, number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert!(
        narrowed(function).contains(&number),
        "a fact about the value a name holds outlives the call"
    );
}

#[test]
fn a_nullish_narrowing_survives_a_call_because_no_call_can_rebind_a_name() {
    let source = "function use(host: string | undefined): number {\n  if (host !== undefined) {\n    audit();\n    return host.length;\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let undefined = table.intern(TypeKind::Undefined);
    let maybe = table.union(vec![text, undefined]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("host", Answer::Known(maybe)),
            ("length", Answer::Known(number)),
            ("audit", returning(&mut table, number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(narrowed(&report.functions[0]).contains(&text));
}

fn resolver(table: &mut TypeTable) -> savitar_types::TypeId {
    let number = table.intern(TypeKind::Number);
    table.function(Vec::new(), number)
}

fn made_closures(function: &savitar_ir::Function) -> Vec<(String, Vec<savitar_ir::ValueId>)> {
    function
        .blocks()
        .flat_map(|(_, block)| block.statements.iter())
        .filter_map(|statement| match statement {
            Statement::Assign {
                value:
                    Rvalue::Closure {
                        function: made,
                        captured,
                    },
                ..
            } => Some((made.clone(), captured.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn an_arrow_declared_by_an_annotated_name_lowers_to_a_function_of_its_own() {
    let source =
        "function use(): number {\n  const resolve: Resolver = () => 1;\n  return resolve();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("resolve", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 2);
    assert_eq!(report.functions[0].name(), "use");
    assert_eq!(report.functions[1].name(), "use#0");
    assert_eq!(verify(&report.functions[0]), vec![]);
    assert_eq!(verify(&report.functions[1]), vec![]);
    assert_eq!(
        made_closures(&report.functions[0]),
        vec![("use#0".to_string(), Vec::new())]
    );
    assert!(report.functions[1].captures().is_empty());
}

#[test]
fn an_arrow_takes_its_signature_from_the_name_it_is_bound_to_without_an_annotation() {
    let source = "function use(): number {\n  const resolve = () => 1;\n  return resolve();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("resolve", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 2);
    assert_eq!(report.functions[1].name(), "use#0");
}

#[test]
fn an_arrow_whose_type_no_position_declares_is_refused_rather_than_guessed() {
    let source = "function use(): number {\n  const resolve = () => 1;\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let provider = ByName::new(source, vec![("use", returning(&mut table, number))]);
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule, "unsupported-expression");
    assert!(
        report.diagnostics[0]
            .message
            .contains("nothing at this position declares the type"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_arrow_passed_as_an_argument_takes_the_signature_the_parameter_declares() {
    let source = "function use(): number {\n  return resolved(() => 1);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let resolved = taking(&mut table, signature, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("resolved", Answer::Known(resolved)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 2);
    assert_eq!(report.functions[1].name(), "use#0");
}

#[test]
fn an_arrow_with_a_block_body_lowers_its_statements() {
    let source = "function use(): number {\n  const resolve: Resolver = () => {\n    const held = 2;\n    return held;\n  };\n  return resolve();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("resolve", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 2);
    let made = &report.functions[1];
    let entry = made.entry().expect("the closure should have a block");
    let Terminator::Return(Some(returned)) = made.block(entry).terminator else {
        panic!("expected a return, got {:?}", made.block(entry).terminator);
    };
    assert_eq!(table.kind(made.type_of(returned)), &TypeKind::Number);
}

#[test]
fn a_function_expression_lowers_like_an_arrow() {
    let source = "function use(): number {\n  const resolve: Resolver = function () {\n    return 1;\n  };\n  return resolve();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("resolve", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 2);
    assert_eq!(report.functions[1].name(), "use#0");
}

#[test]
fn a_function_expression_with_a_name_of_its_own_is_refused() {
    let source = "function use(): number {\n  const resolve: Resolver = function again() {\n    return 1;\n  };\n  return resolve();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("resolve", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-expression");
    assert!(
        report.diagnostics[0].message.contains("name of its own"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_arrow_parameter_takes_its_type_from_the_position_not_from_the_provider() {
    let source = "function use(): number {\n  const measure: Measure = (text) => text.length;\n  return measure(\"ab\");\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let signature = taking(&mut table, text, number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("measure", Answer::Known(signature)),
            ("length", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let made = &report.functions[1];
    assert_eq!(made.parameters().len(), 1);
    assert_eq!(made.type_of(made.parameters()[0]), text);
}

#[test]
fn an_arrow_with_more_parameters_than_its_position_declares_is_refused() {
    let source = "function use(): number {\n  const resolve: Resolver = (extra) => 1;\n  return resolve();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("resolve", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-pattern");
    assert!(
        report.diagnostics[0]
            .message
            .contains("declares no type for this parameter"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_arrow_captures_a_parameter_of_the_function_around_it() {
    let source = "function use(base: number): number {\n  const add: Resolver = () => base + 1;\n  return add();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("base", Answer::Known(number)),
            ("add", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 2);
    let holder = &report.functions[0];
    let made = &report.functions[1];
    assert_eq!(made.captures().len(), 1);
    assert_eq!(made.captures()[0].name, "base");
    assert_eq!(made.type_of(made.captures()[0].value), number);
    assert_eq!(
        made_closures(holder),
        vec![("use#0".to_string(), vec![holder.parameters()[0]])],
        "the creation site passes the value the name holds outside"
    );
    assert_eq!(verify(made), vec![]);
}

#[test]
fn an_arrow_captures_the_narrowed_type_a_name_holds_where_it_is_made() {
    let source = "function use(host: string | undefined): number {\n  if (host !== undefined) {\n    const size: Resolver = () => host.length;\n    return size();\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let undefined = table.intern(TypeKind::Undefined);
    let maybe = table.union(vec![text, undefined]);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("host", Answer::Known(maybe)),
            ("size", Answer::Known(signature)),
            ("length", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let made = &report.functions[1];
    assert_eq!(made.captures().len(), 1);
    assert_eq!(
        made.type_of(made.captures()[0].value),
        text,
        "the closure captures the value the test narrowed, not the declared union"
    );
}

#[test]
fn an_arrow_that_writes_a_name_it_captured_is_refused() {
    let source = "function use(base: number): number {\n  const bump: Resolver = () => {\n    base = base + 1;\n    return base;\n  };\n  return bump();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("base", Answer::Known(number)),
            ("bump", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-capture");
    assert!(
        report.diagnostics[0]
            .message
            .contains("writing to a captured name"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_arrow_that_captures_a_name_the_function_writes_later_is_refused() {
    let source = "function use(limit: number): number {\n  let seen = 0;\n  const read: Resolver = () => seen;\n  seen = seen + 1;\n  return read() + limit;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("limit", Answer::Known(number)),
            ("read", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-capture");
    assert!(
        report.diagnostics[0]
            .message
            .contains("written somewhere in this function"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_arrow_inside_an_arrow_is_named_after_the_one_around_it() {
    let source = "function use(): number {\n  const outer: Resolver = () => {\n    const inner: Resolver = () => 1;\n    return inner();\n  };\n  return outer();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("outer", Answer::Known(signature)),
            ("inner", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let names: Vec<&str> = report
        .functions
        .iter()
        .map(savitar_ir::Function::name)
        .collect();
    assert_eq!(names, vec!["use", "use#0", "use#0#0"]);
}

#[test]
fn capturing_a_name_from_two_functions_out_is_refused_rather_than_read_as_a_global() {
    let source = "function use(base: number): number {\n  const outer: Resolver = () => {\n    const inner: Resolver = () => base + 1;\n    return inner();\n  };\n  return outer();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("base", Answer::Known(number)),
            ("outer", Answer::Known(signature)),
            ("inner", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-capture");
    assert!(
        report.diagnostics[0].message.contains("further out"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_arrow_in_a_method_captures_the_receiver_because_an_arrow_inherits_this() {
    let source = "class Counter {\n  reading(): number {\n    const reader: Resolver = () => this.held;\n    return reader();\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, false);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("reading", returning(&mut table, number)),
            ("reader", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 2);
    let made = &report.functions[1];
    assert_eq!(made.name(), "Counter.reading#0");
    assert_eq!(made.captures().len(), 1);
    assert_eq!(made.captures()[0].name, "this");
    assert_eq!(made.type_of(made.captures()[0].value), shape);
}

#[test]
fn a_function_expression_in_a_method_cannot_see_this_because_its_own_is_dynamic() {
    let source = "class Counter {\n  reading(): number {\n    const reader: Resolver = function () {\n      return this.held;\n    };\n    return reader();\n  }\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let shape = held_field(&mut table, false);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("Counter", Answer::Known(shape)),
            ("reading", returning(&mut table, number)),
            ("reader", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("counter.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-expression");
    assert!(
        report.diagnostics[0]
            .message
            .contains("`this` is not bound"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_arrow_reads_a_module_level_name_as_a_global_and_not_as_a_capture() {
    let source = "function use(): number {\n  const resolve: Resolver = () => shared;\n  return resolve();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("resolve", Answer::Known(signature)),
            ("shared", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let made = &report.functions[1];
    assert!(made.captures().is_empty());
    assert!(
        made.blocks()
            .flat_map(|(_, block)| block.statements.iter())
            .any(|statement| matches!(
                statement,
                Statement::Assign {
                    value: Rvalue::Global(_),
                    ..
                }
            )),
        "a name no enclosing function holds stays a global read"
    );
}

#[test]
fn an_arrow_whose_declared_type_takes_type_arguments_is_refused() {
    let source = "function use(): number {\n  const resolve: Generic = () => 1;\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let parameter = table.intern(TypeKind::TypeParameter(0));
    let signature = table.generic_function(vec![parameter], Vec::new(), number);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("resolve", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-expression");
    assert!(
        report.diagnostics[0].message.contains("type arguments"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_arrow_that_does_not_return_what_its_position_declares_is_refused() {
    let source = "function use(): number {\n  const resolve: Resolver = () => \"late\";\n  return resolve();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("resolve", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-return");
}

#[test]
fn an_arrow_whose_position_declares_void_drops_the_value_of_its_body() {
    let source = "function use(): number {\n  const note: Logger = () => record();\n  note();\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let void = table.intern(TypeKind::Void);
    let signature = table.function(Vec::new(), void);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("note", Answer::Known(signature)),
            ("record", returning(&mut table, number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let made = &report.functions[1];
    let entry = made.entry().expect("the closure should have a block");
    assert_eq!(made.block(entry).terminator, Terminator::Return(None));
}

#[test]
fn a_name_read_on_two_paths_of_a_closure_is_captured_once() {
    let source = "function use(base: number, flag: boolean): number {\n  const pick: Resolver = () => {\n    if (flag) {\n      return base;\n    }\n    return base + 1;\n  };\n  return pick();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("base", Answer::Known(number)),
            ("flag", Answer::Known(boolean)),
            ("pick", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let made = &report.functions[1];
    let names: Vec<&str> = made
        .captures()
        .iter()
        .map(|capture| capture.name.as_str())
        .collect();
    assert_eq!(names, vec!["flag", "base"]);
    assert_eq!(verify(made), vec![]);
    assert_eq!(
        made_closures(&report.functions[0])[0].1.len(),
        2,
        "the creation site passes one value per capture"
    );
}

#[test]
fn an_arrow_captures_a_const_of_the_function_around_it() {
    let source = "function use(): number {\n  const base = 2;\n  const add: Resolver = () => base + 1;\n  return add();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("add", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let made = &report.functions[1];
    assert_eq!(made.captures().len(), 1);
    assert_eq!(made.captures()[0].name, "base");
}

#[test]
fn an_arrow_made_in_a_loop_captures_the_name_the_loop_binds() {
    let source = "function use(items: readonly number[]): number {\n  let total = 0;\n  for (const item of items) {\n    const read: Resolver = () => item;\n    total = total + read();\n  }\n  return total;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let items = table.intern(TypeKind::ReadonlyArray(number));
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("items", Answer::Known(items)),
            ("read", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 2);
    let made = &report.functions[1];
    assert_eq!(made.captures().len(), 1);
    assert_eq!(made.captures()[0].name, "item");
    assert_eq!(verify(&report.functions[0]), vec![]);
    assert_eq!(verify(made), vec![]);
}

#[test]
fn an_arrow_that_captures_a_name_a_loop_writes_is_refused() {
    let source = "function use(items: readonly number[]): number {\n  let total = 0;\n  for (const item of items) {\n    total = total + item;\n  }\n  const read: Resolver = () => total;\n  return read();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let items = table.intern(TypeKind::ReadonlyArray(number));
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("items", Answer::Known(items)),
            ("read", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-capture");
    assert!(
        report.diagnostics[0]
            .message
            .contains("written somewhere in this function"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_arrow_returned_from_the_function_that_made_it_lowers_because_its_capture_is_by_value() {
    let source = "function use(base: number): Resolver {\n  return () => base + 1;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, signature)),
            ("base", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 2);
    let made = &report.functions[1];
    assert_eq!(made.captures().len(), 1);
    assert_eq!(made.captures()[0].name, "base");
    let entry = report.functions[0]
        .entry()
        .expect("the maker should have a block");
    let Terminator::Return(Some(returned)) = report.functions[0].block(entry).terminator else {
        panic!("expected the closure to be returned");
    };
    assert_eq!(report.functions[0].type_of(returned), signature);
}

#[test]
fn capturing_a_name_narrowed_by_a_field_fact_is_refused_because_the_closure_outlives_the_call() {
    let source = "function use(frame: Frame): number {\n  if (frame.kind === \"binary\") {\n    const read: Resolver = () => frame.size;\n    return read();\n  }\n  return 0;\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let tags = frame_tags(&mut table);
    let shapes = frames(&mut table, tags, false);
    let signature = resolver(&mut table);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, number)),
            ("frame", Answer::Known(shapes.union)),
            ("kind", Answer::Known(shapes.tags)),
            ("read", Answer::Known(signature)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-capture");
    assert!(
        report.diagnostics[0]
            .message
            .contains("fact about one of its fields"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn an_arrow_in_an_object_literal_takes_the_signature_the_field_declares() {
    let source = "function use(base: number): Handlers {\n  return { answer: () => base };\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let signature = resolver(&mut table);
    let handlers = table.object(vec![named_field("answer", signature, false)]);
    let provider = ByName::new(
        source,
        vec![
            ("use", returning(&mut table, handlers)),
            ("base", Answer::Known(number)),
        ],
    );
    let file = SourceFile::new("use.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.functions.len(), 2);
    let made = &report.functions[1];
    assert_eq!(made.captures().len(), 1);
    assert_eq!(made.captures()[0].name, "base");
}

fn two_optional_overloads(table: &mut TypeTable) -> Answer {
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let counting = table.function(
        vec![savitar_types::Parameter {
            type_id: number,
            rest: false,
            optional: true,
        }],
        number,
    );
    let naming = table.function(
        vec![savitar_types::Parameter {
            type_id: text,
            rest: false,
            optional: true,
        }],
        text,
    );
    Answer::Known(table.overloaded(vec![counting, naming]))
}

#[test]
fn omitting_the_argument_of_two_optional_overloads_that_disagree_is_refused_as_ambiguous() {
    let source = "function callIt(): number {\n  return given();\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let given = two_optional_overloads(&mut table);
    let provider = ByName::new(
        source,
        vec![("callIt", returning(&mut table, number)), ("given", given)],
    );
    let file = SourceFile::new("callIt.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0]
            .message
            .contains("more than one of the declared forms"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn passing_undefined_to_two_optional_overloads_that_disagree_is_refused_where_omitting_it_is() {
    let source = "function callIt(): number {\n  return given(undefined);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let given = two_optional_overloads(&mut table);
    let provider = ByName::new(
        source,
        vec![("callIt", returning(&mut table, number)), ("given", given)],
    );
    let file = SourceFile::new("callIt.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.functions.is_empty());
    assert_eq!(report.diagnostics[0].rule, "unsupported-call");
    assert!(
        report.diagnostics[0]
            .message
            .contains("more than one of the declared forms"),
        "{}",
        report.diagnostics[0].message
    );
}

#[test]
fn passing_undefined_picks_the_only_optional_overload_that_takes_it() {
    let source = "function callIt(): number {\n  return given(undefined);\n}\n";
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let counting = table.function(
        vec![savitar_types::Parameter {
            type_id: number,
            rest: false,
            optional: true,
        }],
        number,
    );
    let naming = table.function(
        vec![savitar_types::Parameter {
            type_id: text,
            rest: false,
            optional: false,
        }],
        text,
    );
    let given = Answer::Known(table.overloaded(vec![counting, naming]));
    let provider = ByName::new(
        source,
        vec![("callIt", returning(&mut table, number)), ("given", given)],
    );
    let file = SourceFile::new("callIt.ts", source);

    let report = lower(&file, &provider, &mut table);

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let function = &report.functions[0];
    assert_eq!(verify(function), vec![]);
    assert_eq!(function.type_of(returned_value(function)), number);
}
