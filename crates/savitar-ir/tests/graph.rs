use savitar_diagnostics::Span;
use savitar_ir::{
    BinaryOp, Block, Constant, Dominance, Function, Malformation, Rvalue, Statement, Terminator,
    verify,
};
use savitar_types::{TypeKind, TypeTable};

fn nowhere() -> Span {
    Span::new(0, 0)
}

fn block(statements: Vec<Statement>, terminator: Terminator) -> Block {
    Block {
        parameters: Vec::new(),
        statements,
        terminator,
    }
}

#[test]
fn an_empty_function_has_no_entry_block() {
    let function = Function::new("empty");
    assert_eq!(verify(&function), vec![Malformation::NoEntryBlock]);
    assert!(Dominance::of(&function).reachable_blocks().is_empty());
}

#[test]
fn a_straight_line_function_verifies() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let mut function = Function::new("double");

    let input = function.declare_parameter(number);
    let two = function.fresh_value(number);
    let doubled = function.fresh_value(number);
    function.add_block(block(
        vec![
            Statement::Assign {
                target: two,
                value: Rvalue::Constant(Constant::Number(2.0f64.to_bits())),
                span: nowhere(),
            },
            Statement::Assign {
                target: doubled,
                value: Rvalue::Binary {
                    op: BinaryOp::Multiply,
                    left: input,
                    right: two,
                },
                span: nowhere(),
            },
        ],
        Terminator::Return(Some(doubled)),
    ));

    assert_eq!(verify(&function), vec![]);
}

#[test]
fn using_a_value_before_its_own_assignment_is_caught() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let mut function = Function::new("backwards");

    let later = function.fresh_value(number);
    let early = function.fresh_value(number);
    function.add_block(block(
        vec![
            Statement::Assign {
                target: early,
                value: Rvalue::Unary {
                    op: savitar_ir::UnaryOp::Negate,
                    operand: later,
                },
                span: nowhere(),
            },
            Statement::Assign {
                target: later,
                value: Rvalue::Constant(Constant::Number(1.0f64.to_bits())),
                span: nowhere(),
            },
        ],
        Terminator::Return(Some(early)),
    ));

    let found = verify(&function);
    assert!(
        found
            .iter()
            .any(|problem| matches!(problem, Malformation::UseBeforeDefinition { .. })),
        "{found:?}"
    );
}

#[test]
fn a_value_defined_on_one_arm_does_not_dominate_the_join() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let mut function = Function::new("lopsided");

    let flag = function.declare_parameter(boolean);
    let only_on_one_arm = function.fresh_value(number);

    let entry = function.add_block(block(vec![], Terminator::Unreachable));
    let left = function.add_block(block(
        vec![Statement::Assign {
            target: only_on_one_arm,
            value: Rvalue::Constant(Constant::Number(1.0f64.to_bits())),
            span: nowhere(),
        }],
        Terminator::Unreachable,
    ));
    let right = function.add_block(block(vec![], Terminator::Unreachable));
    let join = function.add_block(block(vec![], Terminator::Return(Some(only_on_one_arm))));

    function.block_mut(entry).terminator = Terminator::Branch {
        condition: flag,
        then_block: left,
        then_arguments: Vec::new(),
        else_block: right,
        else_arguments: Vec::new(),
    };
    function.block_mut(left).terminator = Terminator::Jump {
        target: join,
        arguments: Vec::new(),
    };
    function.block_mut(right).terminator = Terminator::Jump {
        target: join,
        arguments: Vec::new(),
    };

    let found = verify(&function);
    assert_eq!(
        found,
        vec![Malformation::UseBeforeDefinition {
            value: only_on_one_arm,
            block: join
        }],
        "a join may only use what every path defines"
    );

    let dominance = Dominance::of(&function);
    assert!(dominance.dominates(entry, join));
    assert!(!dominance.dominates(left, join));
    assert_eq!(dominance.immediate_dominator(join), Some(entry));
}

#[test]
fn a_block_parameter_carries_the_value_across_the_join() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let boolean = table.intern(TypeKind::Boolean);
    let mut function = Function::new("joined");

    let flag = function.declare_parameter(boolean);
    let one = function.fresh_value(number);
    let two = function.fresh_value(number);
    let merged = function.fresh_value(number);

    let entry = function.add_block(block(vec![], Terminator::Unreachable));
    let left = function.add_block(block(
        vec![Statement::Assign {
            target: one,
            value: Rvalue::Constant(Constant::Number(1.0f64.to_bits())),
            span: nowhere(),
        }],
        Terminator::Unreachable,
    ));
    let right = function.add_block(block(
        vec![Statement::Assign {
            target: two,
            value: Rvalue::Constant(Constant::Number(2.0f64.to_bits())),
            span: nowhere(),
        }],
        Terminator::Unreachable,
    ));
    let join = function.add_block(Block {
        parameters: vec![merged],
        statements: Vec::new(),
        terminator: Terminator::Return(Some(merged)),
    });

    function.block_mut(entry).terminator = Terminator::Branch {
        condition: flag,
        then_block: left,
        then_arguments: Vec::new(),
        else_block: right,
        else_arguments: Vec::new(),
    };
    function.block_mut(left).terminator = Terminator::Jump {
        target: join,
        arguments: vec![one],
    };
    function.block_mut(right).terminator = Terminator::Jump {
        target: join,
        arguments: vec![two],
    };

    assert_eq!(verify(&function), vec![]);
}

#[test]
fn a_jump_must_match_the_arity_and_types_of_the_block_it_enters() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let mut function = Function::new("mismatched");

    let value = function.declare_parameter(text);
    let parameter = function.fresh_value(number);

    let entry = function.add_block(block(vec![], Terminator::Unreachable));
    let target = function.add_block(Block {
        parameters: vec![parameter],
        statements: Vec::new(),
        terminator: Terminator::Return(Some(parameter)),
    });

    function.block_mut(entry).terminator = Terminator::Jump {
        target,
        arguments: Vec::new(),
    };
    assert_eq!(
        verify(&function),
        vec![Malformation::JumpArgumentCount {
            from: entry,
            to: target,
            given: 0,
            expected: 1
        }]
    );

    function.block_mut(entry).terminator = Terminator::Jump {
        target,
        arguments: vec![value],
    };
    assert_eq!(
        verify(&function),
        vec![Malformation::JumpArgumentType {
            from: entry,
            to: target,
            position: 0
        }]
    );
}

#[test]
fn a_loop_is_dominated_by_its_header() {
    let mut table = TypeTable::new();
    let boolean = table.intern(TypeKind::Boolean);
    let mut function = Function::new("spin");

    let flag = function.declare_parameter(boolean);
    let entry = function.add_block(block(vec![], Terminator::Unreachable));
    let header = function.add_block(block(vec![], Terminator::Unreachable));
    let body = function.add_block(block(vec![], Terminator::Unreachable));
    let exit = function.add_block(block(vec![], Terminator::Return(None)));

    function.block_mut(entry).terminator = Terminator::Jump {
        target: header,
        arguments: Vec::new(),
    };
    function.block_mut(header).terminator = Terminator::Branch {
        condition: flag,
        then_block: body,
        then_arguments: Vec::new(),
        else_block: exit,
        else_arguments: Vec::new(),
    };
    function.block_mut(body).terminator = Terminator::Jump {
        target: header,
        arguments: Vec::new(),
    };

    assert_eq!(verify(&function), vec![]);
    let dominance = Dominance::of(&function);
    assert!(dominance.dominates(header, body));
    assert!(dominance.dominates(header, exit));
    assert!(!dominance.dominates(body, exit));
    assert_eq!(dominance.immediate_dominator(body), Some(header));
    assert_eq!(dominance.reachable_blocks().len(), 4);
}

#[test]
fn an_unreachable_block_is_not_verified_but_is_reported_as_unreachable() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let mut function = Function::new("stranded");

    let never_defined = function.fresh_value(number);
    let entry = function.add_block(block(vec![], Terminator::Return(None)));
    let stranded = function.add_block(block(vec![], Terminator::Return(Some(never_defined))));

    assert_eq!(verify(&function), vec![]);
    let dominance = Dominance::of(&function);
    assert!(dominance.reachable(entry));
    assert!(!dominance.reachable(stranded));
}

#[test]
fn a_jump_to_a_block_that_does_not_exist_is_caught() {
    let mut function = Function::new("dangling");
    let missing = savitar_ir::BlockId::from_index(7);
    function.add_block(block(
        vec![],
        Terminator::Jump {
            target: missing,
            arguments: Vec::new(),
        },
    ));
    assert_eq!(verify(&function), vec![Malformation::MissingBlock(missing)]);
}

#[test]
fn a_captured_value_counts_as_defined_on_entry() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let mut function = Function::new("outer#0");

    let captured = function.declare_capture("base", number);
    let one = function.fresh_value(number);
    let raised = function.fresh_value(number);
    function.add_block(block(
        vec![
            Statement::Assign {
                target: one,
                value: Rvalue::Constant(Constant::Number(1.0f64.to_bits())),
                span: nowhere(),
            },
            Statement::Assign {
                target: raised,
                value: Rvalue::Binary {
                    op: BinaryOp::Add,
                    left: captured,
                    right: one,
                },
                span: nowhere(),
            },
        ],
        Terminator::Return(Some(raised)),
    ));

    assert_eq!(verify(&function), vec![]);
    assert_eq!(function.captures().len(), 1);
    assert_eq!(function.captures()[0].name, "base");
    assert_eq!(function.captures()[0].value, captured);
}

#[test]
fn making_a_closure_uses_the_values_it_captures() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let mut function = Function::new("outer");

    let base = function.declare_parameter(number);
    let made = function.fresh_value(number);
    function.add_block(block(
        vec![Statement::Assign {
            target: made,
            value: Rvalue::Closure {
                function: "outer#0".to_string(),
                captured: vec![base],
            },
            span: nowhere(),
        }],
        Terminator::Return(Some(made)),
    ));

    assert_eq!(verify(&function), vec![]);
}

#[test]
fn making_a_closure_over_a_value_that_is_not_defined_is_caught() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let mut function = Function::new("outer");

    let made = function.fresh_value(number);
    let missing = function.fresh_value(number);
    function.add_block(block(
        vec![Statement::Assign {
            target: made,
            value: Rvalue::Closure {
                function: "outer#0".to_string(),
                captured: vec![missing],
            },
            span: nowhere(),
        }],
        Terminator::Return(Some(made)),
    ));

    assert_eq!(
        verify(&function),
        vec![Malformation::ValueNeverDefined(missing)]
    );
}
