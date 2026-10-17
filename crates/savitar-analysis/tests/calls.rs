use savitar_analysis::{CallGraph, CallSite, Callee, ModuleTouch, Opaque, Reach};
use savitar_diagnostics::Span;
use savitar_ir::{Block, Constant, Function, Rvalue, Statement, Terminator, ValueId};
use savitar_types::{TypeId, TypeKind, TypeTable};

fn at(start: u32) -> Span {
    Span::new(start, start + 1)
}

fn block(statements: Vec<Statement>, terminator: Terminator) -> Block {
    Block {
        parameters: Vec::new(),
        statements,
        terminator,
    }
}

struct Builder {
    function: Function,
    statements: Vec<Statement>,
    any: TypeId,
    next_span: u32,
}

impl Builder {
    fn named(name: &str, table: &mut TypeTable) -> Self {
        Self {
            function: Function::new(name),
            statements: Vec::new(),
            any: table.intern(TypeKind::Unknown),
            next_span: 1,
        }
    }

    fn assign(&mut self, value: Rvalue) -> ValueId {
        let target = self.function.fresh_value(self.any);
        let span = at(self.next_span);
        self.next_span += 1;
        self.statements.push(Statement::Assign {
            target,
            value,
            span,
        });
        target
    }

    fn span_of_the_last(&self) -> Span {
        at(self.next_span - 1)
    }

    fn bind_module(&mut self, name: &str, value: ValueId) {
        let span = at(self.next_span);
        self.next_span += 1;
        self.statements.push(Statement::BindModule {
            name: name.to_string(),
            value,
            span,
        });
    }

    fn done(mut self) -> Function {
        let statements = std::mem::take(&mut self.statements);
        self.function
            .add_block(block(statements, Terminator::Return(None)));
        self.function
    }
}

fn a_leaf_that_only_computes(table: &mut TypeTable) -> Function {
    let mut leaf = Builder::named("leaf", table);
    leaf.assign(Rvalue::Constant(Constant::Number(8_u64)));
    leaf.done()
}

fn calling(callee: Rvalue, name: &str, table: &mut TypeTable) -> (Function, CallSite) {
    let mut caller = Builder::named(name, table);
    let target = caller.assign(callee);
    let called = caller.assign(Rvalue::Call {
        callee: target,
        arguments: Vec::new(),
    });
    let _ = called;
    let span = caller.span_of_the_last();
    let function = caller.done();
    let sites = savitar_analysis::calls_in(&function);
    assert_eq!(sites.len(), 1, "the body should hold one call");
    assert_eq!(sites[0].at, span);
    (function, sites[0].clone())
}

#[test]
fn a_call_to_a_module_name_resolves_to_the_function_of_that_name() {
    let mut table = TypeTable::new();
    let leaf = a_leaf_that_only_computes(&mut table);
    let (caller, site) = calling(
        Rvalue::ModuleBinding("leaf".to_string()),
        "caller",
        &mut table,
    );

    assert_eq!(site.callee, Callee::Named("leaf".to_string()));
    let graph = CallGraph::over([&leaf, &caller]);
    assert_eq!(
        graph.what_a_call_reaches(&site),
        Reach::NoModuleNameIsTouched
    );
}

#[test]
fn a_call_to_a_closure_made_here_resolves_to_the_body_of_that_closure() {
    let mut table = TypeTable::new();
    let mut inner = Builder::named("caller#0", &mut table);
    inner.assign(Rvalue::Constant(Constant::Boolean(true)));
    let inner = inner.done();

    let (caller, site) = calling(
        Rvalue::Closure {
            function: "caller#0".to_string(),
            captured: Vec::new(),
        },
        "caller",
        &mut table,
    );

    assert_eq!(site.callee, Callee::Named("caller#0".to_string()));
    let graph = CallGraph::over([&inner, &caller]);
    assert_eq!(
        graph.what_a_call_reaches(&site),
        Reach::NoModuleNameIsTouched
    );
}

#[test]
fn a_call_that_reaches_a_module_name_two_hops_away_names_the_whole_path() {
    let mut table = TypeTable::new();

    let mut deeper = Builder::named("middle#0", &mut table);
    deeper.assign(Rvalue::ModuleBinding("cache".to_string()));
    let deeper = deeper.done();

    let mut middle = Builder::named("middle", &mut table);
    let target = middle.assign(Rvalue::Closure {
        function: "middle#0".to_string(),
        captured: Vec::new(),
    });
    middle.assign(Rvalue::Call {
        callee: target,
        arguments: Vec::new(),
    });
    let middle = middle.done();

    let (caller, site) = calling(
        Rvalue::ModuleBinding("middle".to_string()),
        "caller",
        &mut table,
    );

    let graph = CallGraph::over([&deeper, &middle, &caller]);
    let Reach::AModuleNameIsTouched { touch, through, .. } = graph.what_a_call_reaches(&site)
    else {
        panic!("a reachable read of a module name must be reported");
    };
    assert_eq!(
        touch,
        ModuleTouch::ANameOfThisModuleIsRead("cache".to_string())
    );
    assert_eq!(
        through,
        vec!["middle".to_string(), "middle#0".to_string()],
        "the path exists so the refusal can say how the name is reached"
    );
}

#[test]
fn reading_a_module_name_to_find_the_next_callee_counts_as_touching_it() {
    let mut table = TypeTable::new();
    let leaf = a_leaf_that_only_computes(&mut table);

    let mut middle = Builder::named("middle", &mut table);
    let target = middle.assign(Rvalue::ModuleBinding("leaf".to_string()));
    middle.assign(Rvalue::Call {
        callee: target,
        arguments: Vec::new(),
    });
    let middle = middle.done();

    let (caller, site) = calling(
        Rvalue::ModuleBinding("middle".to_string()),
        "caller",
        &mut table,
    );

    let graph = CallGraph::over([&leaf, &middle, &caller]);
    let Reach::AModuleNameIsTouched { touch, through, .. } = graph.what_a_call_reaches(&site)
    else {
        panic!("the read that finds the next callee is a read like any other");
    };
    assert_eq!(
        touch,
        ModuleTouch::ANameOfThisModuleIsRead("leaf".to_string())
    );
    assert_eq!(through, vec!["middle".to_string()]);
}

#[test]
fn a_read_of_a_module_name_counts_the_same_when_it_comes_from_another_module() {
    let mut table = TypeTable::new();

    let mut reader = Builder::named("reader", &mut table);
    reader.assign(Rvalue::Imported {
        specifier: "./shared.ts".to_string(),
        name: "limit".to_string(),
    });
    let reader = reader.done();

    let (caller, site) = calling(
        Rvalue::ModuleBinding("reader".to_string()),
        "caller",
        &mut table,
    );

    let graph = CallGraph::over([&reader, &caller]);
    let Reach::AModuleNameIsTouched { touch, .. } = graph.what_a_call_reaches(&site) else {
        panic!("reading a name another module binds must be reported");
    };
    assert_eq!(
        touch,
        ModuleTouch::ANameOfAnotherModuleIsRead {
            specifier: "./shared.ts".to_string(),
            name: "limit".to_string(),
        }
    );
}

#[test]
fn binding_a_module_name_inside_a_reachable_body_counts_as_touching_it() {
    let mut table = TypeTable::new();

    let mut binder = Builder::named("binder", &mut table);
    let value = binder.assign(Rvalue::Constant(Constant::Number(1_u64)));
    binder.bind_module("port", value);
    let binder = binder.done();

    let (caller, site) = calling(
        Rvalue::ModuleBinding("binder".to_string()),
        "caller",
        &mut table,
    );

    let graph = CallGraph::over([&binder, &caller]);
    let Reach::AModuleNameIsTouched { touch, .. } = graph.what_a_call_reaches(&site) else {
        panic!("binding a module name must be reported too");
    };
    assert_eq!(
        touch,
        ModuleTouch::ANameOfThisModuleIsBound("port".to_string())
    );
}

#[test]
fn a_call_to_a_value_that_arrives_as_a_parameter_is_not_known_and_is_not_a_missing_edge() {
    let mut table = TypeTable::new();
    let mut caller = Builder::named("caller", &mut table);
    let incoming = caller.function.declare_parameter(caller.any);
    caller.assign(Rvalue::Call {
        callee: incoming,
        arguments: Vec::new(),
    });
    let caller = caller.done();

    let sites = savitar_analysis::calls_in(&caller);
    assert_eq!(sites[0].callee, Callee::NotAName(Opaque::AParameter));

    let graph = CallGraph::over([&caller]);
    let reach = graph.what_a_call_reaches(&sites[0]);
    assert!(
        !reach.proves_no_module_name_is_touched(),
        "a target the graph cannot resolve is the worst case, never an absent edge"
    );
    assert!(matches!(
        reach,
        Reach::ACalleeIsNotKnown {
            callee: Opaque::AParameter,
            ..
        }
    ));
}

#[test]
fn a_call_to_a_captured_name_is_not_known_and_names_the_capture() {
    let mut table = TypeTable::new();
    let mut caller = Builder::named("caller", &mut table);
    let held = caller.function.declare_capture("handler", caller.any);
    caller.assign(Rvalue::Call {
        callee: held,
        arguments: Vec::new(),
    });
    let caller = caller.done();

    let sites = savitar_analysis::calls_in(&caller);
    assert_eq!(
        sites[0].callee,
        Callee::NotAName(Opaque::ACapturedName("handler".to_string()))
    );
}

#[test]
fn a_call_to_a_field_of_a_value_is_not_known_and_names_the_field() {
    let mut table = TypeTable::new();
    let mut caller = Builder::named("caller", &mut table);
    let object = caller.assign(Rvalue::Global("console".to_string()));
    let method = caller.assign(Rvalue::ReadField {
        object,
        name: "log".to_string(),
    });
    caller.assign(Rvalue::Call {
        callee: method,
        arguments: Vec::new(),
    });
    let caller = caller.done();

    let sites = savitar_analysis::calls_in(&caller);
    assert_eq!(
        sites[0].callee,
        Callee::NotAName(Opaque::AFieldOfAValue("log".to_string()))
    );
}

#[test]
fn a_call_to_a_global_name_is_not_known_because_nothing_in_the_program_declares_it() {
    let mut table = TypeTable::new();
    let (_caller, site) = calling(Rvalue::Global("fetch".to_string()), "caller", &mut table);
    assert_eq!(
        site.callee,
        Callee::NotAName(Opaque::AGlobalName("fetch".to_string()))
    );
}

#[test]
fn a_call_to_a_name_imported_from_another_module_is_not_known_yet() {
    let mut table = TypeTable::new();
    let (_caller, site) = calling(
        Rvalue::Imported {
            specifier: "./helpers.ts".to_string(),
            name: "readPort".to_string(),
        },
        "caller",
        &mut table,
    );
    assert_eq!(
        site.callee,
        Callee::NotAName(Opaque::AnImportedName {
            specifier: "./helpers.ts".to_string(),
            name: "readPort".to_string(),
        })
    );
}

#[test]
fn a_resolved_name_whose_body_this_run_does_not_hold_is_not_known() {
    let mut table = TypeTable::new();
    let (caller, site) = calling(
        Rvalue::ModuleBinding("refused".to_string()),
        "caller",
        &mut table,
    );

    let graph = CallGraph::over([&caller]);
    let Reach::ACalleeIsNotKnown {
        callee: Opaque::ABodyThisRunDoesNotHold(name),
        through,
        ..
    } = graph.what_a_call_reaches(&site)
    else {
        panic!("a name with no body must be the worst case and not an absent edge");
    };
    assert_eq!(name, "refused");
    assert_eq!(through, vec!["refused".to_string()]);
}

#[test]
fn a_widened_callee_is_followed_back_to_the_name_it_holds() {
    let mut table = TypeTable::new();
    let leaf = a_leaf_that_only_computes(&mut table);

    let mut caller = Builder::named("caller", &mut table);
    let named = caller.assign(Rvalue::ModuleBinding("leaf".to_string()));
    let widened = caller.assign(Rvalue::Widen(named));
    caller.assign(Rvalue::Call {
        callee: widened,
        arguments: Vec::new(),
    });
    let caller = caller.done();

    let sites = savitar_analysis::calls_in(&caller);
    assert_eq!(sites[0].callee, Callee::Named("leaf".to_string()));
    let graph = CallGraph::over([&leaf, &caller]);
    assert_eq!(
        graph.what_a_call_reaches(&sites[0]),
        Reach::NoModuleNameIsTouched
    );
}

#[test]
fn a_body_that_reaches_itself_does_not_keep_the_graph_walking() {
    let mut table = TypeTable::new();

    let mut looping = Builder::named("looping", &mut table);
    let itself = looping.assign(Rvalue::Closure {
        function: "looping".to_string(),
        captured: Vec::new(),
    });
    looping.assign(Rvalue::Call {
        callee: itself,
        arguments: Vec::new(),
    });
    let looping = looping.done();

    let (caller, site) = calling(
        Rvalue::ModuleBinding("looping".to_string()),
        "caller",
        &mut table,
    );
    let graph = CallGraph::over([&looping, &caller]);
    assert_eq!(
        graph.what_a_call_reaches(&site),
        Reach::NoModuleNameIsTouched
    );
}

#[test]
fn a_reachable_read_is_reported_even_when_another_branch_is_unresolved() {
    let mut table = TypeTable::new();

    let mut reader = Builder::named("both#1", &mut table);
    reader.assign(Rvalue::ModuleBinding("cache".to_string()));
    let reader = reader.done();

    let mut both = Builder::named("both", &mut table);
    let unknown = both.assign(Rvalue::Global("fetch".to_string()));
    both.assign(Rvalue::Call {
        callee: unknown,
        arguments: Vec::new(),
    });
    let known = both.assign(Rvalue::Closure {
        function: "both#1".to_string(),
        captured: Vec::new(),
    });
    both.assign(Rvalue::Call {
        callee: known,
        arguments: Vec::new(),
    });
    let both = both.done();

    let (caller, site) = calling(
        Rvalue::ModuleBinding("both".to_string()),
        "caller",
        &mut table,
    );
    let graph = CallGraph::over([&reader, &both, &caller]);
    let reach = graph.what_a_call_reaches(&site);
    assert!(matches!(reach, Reach::AModuleNameIsTouched { .. }));
    assert_eq!(reach.path(), ["both".to_string(), "both#1".to_string()]);
}

#[test]
fn calls_made_by_a_body_the_graph_does_not_hold_are_no_calls_at_all() {
    let graph = CallGraph::default();
    assert!(graph.calls_made_by("nothing").is_empty());
    assert!(!graph.holds("nothing"));
}
