mod assigned;
mod names;
mod standing;
mod types;

use std::collections::{BTreeSet, HashMap};

use oxc_allocator::Allocator;
use oxc_ast::ast::VariableDeclarationKind;
use oxc_ast::ast::{
    Argument, AssignmentTarget, BindingPattern, Class, ClassElement, Declaration,
    ExportDefaultDeclarationKind, Expression, ForStatementInit, ForStatementLeft, FunctionBody,
    ImportOrExportKind, MethodDefinition, MethodDefinitionKind, ObjectPropertyKind, PropertyKey,
    SimpleAssignmentTarget, Statement as AstStatement, VariableDeclaration, VariableDeclarator,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use oxc_syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use savitar_analysis::{CallGraph, Reach, said_as_a_path};
use savitar_diagnostics::{Diagnostic, SourceFile, Span};
use savitar_ir::{
    BinaryOp, Block, BlockId, Constant, Function, Rvalue, Statement, Terminator, UnaryOp, ValueId,
    verify,
};
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_types::{Field, StaticSide, TypeId, TypeKind, TypeTable};

use names::{ModuleName, ModuleNames, WhatTheFileHolds, is_a_package_specifier, names_bound_by};
pub use standing::{ModuleStanding, Ran, Standing, TwoModulesForOneSpecifier};
pub use types::{binary_result, element_result, field_result, unary_result};

#[derive(Debug)]
pub struct LowerReport {
    pub functions: Vec<Function>,
    pub initializer: Option<ModuleInitializer>,
    pub diagnostics: Vec<Diagnostic>,
    pub published: BTreeSet<String>,
}

#[derive(Debug)]
pub struct ModuleInitializer {
    pub function: Function,
    pub closures: Vec<Function>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Binding {
    value: Option<ValueId>,
    declared: TypeId,
    narrowed_by_a_field: bool,
}

impl Binding {
    const fn written(value: ValueId, declared: TypeId) -> Self {
        Self {
            value: Some(value),
            declared,
            narrowed_by_a_field: false,
        }
    }

    const fn awaiting_a_write(declared: TypeId) -> Self {
        Self {
            value: None,
            declared,
            narrowed_by_a_field: false,
        }
    }

    const fn narrowed(value: ValueId, declared: TypeId, by_a_field: bool) -> Self {
        Self {
            value: Some(value),
            declared,
            narrowed_by_a_field: by_a_field,
        }
    }

    fn value_or_refuse(self, name: &str, span: oxc_span::Span) -> Result<ValueId, Vec<Diagnostic>> {
        self.value.ok_or_else(|| {
            vec![
                Diagnostic::unsupported(
                    "flow-invented-a-path",
                    span_of(span),
                    format!("`{name}` is read on a path where it was declared and never written"),
                )
                .with_help(
                    "strict TypeScript refuses that program, so reaching this means the path \
                     analysis of Savitar walked a path the source does not have",
                ),
            ]
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Key {
    name: String,
    depth: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Scope {
    standing: HashMap<String, Vec<(usize, Binding)>>,
}

impl Scope {
    fn new() -> Self {
        Self::default()
    }

    fn get(&self, name: &str) -> Option<Binding> {
        self.standing
            .get(name)
            .and_then(|held| held.last())
            .map(|&(_, binding)| binding)
    }

    fn holds(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    fn innermost(&self, name: &str) -> Option<Key> {
        let &(depth, _) = self.standing.get(name).and_then(|held| held.last())?;
        Some(Key {
            name: name.to_string(),
            depth,
        })
    }

    fn innermost_entry(&self, name: &str) -> Option<(Key, Binding)> {
        let &(depth, binding) = self.standing.get(name).and_then(|held| held.last())?;
        Some((
            Key {
                name: name.to_string(),
                depth,
            },
            binding,
        ))
    }

    fn at(&self, key: &Key) -> Option<Binding> {
        self.standing
            .get(&key.name)?
            .iter()
            .find(|&&(depth, _)| depth == key.depth)
            .map(|&(_, binding)| binding)
    }

    fn held_at(&self, key: &Key, span: oxc_span::Span) -> Result<Binding, Vec<Diagnostic>> {
        self.at(key).ok_or_else(|| {
            let name = &key.name;
            vec![refuse(
                "malformed-ir",
                span,
                format!(
                    "`{name}` stands where this path starts and does not stand on every path from there"
                ),
            )]
        })
    }

    fn put(&mut self, key: &Key, binding: Binding) {
        let held = self.standing.entry(key.name.clone()).or_default();
        match held.iter_mut().find(|slot| slot.0 == key.depth) {
            Some(slot) => slot.1 = binding,
            None => {
                held.push((key.depth, binding));
                held.sort_by_key(|slot| slot.0);
            }
        }
    }

    fn store(
        &mut self,
        name: &str,
        binding: Binding,
        span: oxc_span::Span,
    ) -> Result<(), Vec<Diagnostic>> {
        let Some(slot) = self.standing.get_mut(name).and_then(|held| held.last_mut()) else {
            return Err(vec![refuse(
                "malformed-ir",
                span,
                format!("`{name}` is written where no declaration of it stands"),
            )]);
        };
        slot.1 = binding;
        Ok(())
    }

    fn drop_one(&mut self, key: &Key) {
        let Some(held) = self.standing.get_mut(&key.name) else {
            return;
        };
        held.retain(|slot| slot.0 != key.depth);
        if held.is_empty() {
            self.standing.remove(&key.name);
        }
    }

    fn keys(&self) -> Vec<Key> {
        let mut keys: Vec<Key> = self
            .standing
            .iter()
            .flat_map(|(name, held)| {
                held.iter().map(move |&(depth, _)| Key {
                    name: name.clone(),
                    depth,
                })
            })
            .collect();
        keys.sort();
        keys
    }

    fn entries(&self) -> Vec<(Key, Binding)> {
        let mut entries: Vec<(Key, Binding)> = self
            .standing
            .iter()
            .flat_map(|(name, held)| {
                held.iter().map(move |&(depth, binding)| {
                    (
                        Key {
                            name: name.clone(),
                            depth,
                        },
                        binding,
                    )
                })
            })
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continues,
    Returned,
    Broke,
    Continued,
}

impl Flow {
    const fn continues(self) -> bool {
        matches!(self, Self::Continues)
    }
}

const fn jumped_out_as(then_flow: Flow, else_flow: Flow) -> Flow {
    match (then_flow, else_flow) {
        (Flow::Returned, other) | (other, Flow::Returned) => other,
        (taken, _) => taken,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Switch(BlockId),
    Loop { exit: BlockId, repeat: BlockId },
    Labelled(BlockId),
}

impl Target {
    const fn exit(self) -> BlockId {
        match self {
            Self::Switch(join) | Self::Labelled(join) => join,
            Self::Loop { exit, .. } => exit,
        }
    }

    const fn repeat(self) -> Option<BlockId> {
        match self {
            Self::Loop { repeat, .. } => Some(repeat),
            Self::Switch(_) | Self::Labelled(_) => None,
        }
    }

    const fn caught_by_a_bare_break(self) -> bool {
        matches!(self, Self::Switch(_) | Self::Loop { .. })
    }
}

struct Breakable {
    labels: Vec<String>,
    target: Target,
    breaks: Vec<(BlockId, Scope)>,
    continues: Vec<(BlockId, Scope)>,
}

impl Breakable {
    fn around(labels: Vec<String>, target: Target) -> Self {
        Self {
            labels,
            target,
            breaks: Vec::new(),
            continues: Vec::new(),
        }
    }

    fn answers_to(&self, label: &str) -> bool {
        self.labels.iter().any(|held| held == label)
    }
}

enum Repeating {
    AtHeader,
    ThroughLatch,
}

struct Carried {
    key: Key,
    entering: ValueId,
    declared: TypeId,
}

struct Looping {
    header: BlockId,
    repeat: BlockId,
    exit: BlockId,
    leading: Vec<ValueId>,
    landing: Vec<ValueId>,
    carried: Vec<Carried>,
    at_header: Scope,
}

struct Jumped {
    flow: Flow,
    breaks: Vec<(BlockId, Scope)>,
    continues: Vec<(BlockId, Scope)>,
}

impl Jumped {
    fn repeats(&self) -> bool {
        self.flow.continues() || !self.continues.is_empty()
    }
}

fn carried_value(
    scope: &Scope,
    carried: &Carried,
    span: oxc_span::Span,
) -> Result<ValueId, Vec<Diagnostic>> {
    let binding = scope.held_at(&carried.key, span)?;
    binding.value.ok_or_else(|| {
        let name = &carried.key.name;
        vec![refuse(
            "malformed-ir",
            span,
            format!("`{name}` carries a header parameter and holds no value on this edge back"),
        )]
    })
}

fn every_change_is_carried(
    looping: &Looping,
    scope: &Scope,
    span: oxc_span::Span,
) -> Result<(), Vec<Diagnostic>> {
    for (key, binding) in scope.entries() {
        let Some(held) = looping.at_header.at(&key) else {
            continue;
        };
        if held.value != binding.value && !looping.carried.iter().any(|carried| carried.key == key)
        {
            let name = &key.name;
            return Err(vec![refuse(
                "malformed-ir",
                span,
                format!("`{name}` changes in the loop but carries no header parameter"),
            )]);
        }
    }
    Ok(())
}

struct Tested {
    name: String,
    fact: Fact,
    equal: bool,
}

enum Fact {
    Nullish(TypeKind),
    Primitive(String),
    Truth,
    Declares(String),
    Discriminant {
        field: String,
        literals: Vec<TypeKind>,
    },
    Among(Vec<TypeKind>),
}

impl Fact {
    const fn is_about_a_field(&self) -> bool {
        matches!(self, Self::Declares(_) | Self::Discriminant { .. })
    }
}

struct Subject {
    name: String,
    field: Option<String>,
}

impl Subject {
    fn fact_about(&self, literals: Vec<TypeKind>) -> Fact {
        match &self.field {
            Some(field) => Fact::Discriminant {
                field: field.clone(),
                literals,
            },
            None => Fact::Among(literals),
        }
    }
}

enum CaseEntry {
    Matched(Vec<TypeKind>),
    Unmatched,
    Undecided,
}

impl CaseEntry {
    fn fallen_into_from(self, earlier: &CaseEntry) -> CaseEntry {
        match (self, earlier) {
            (CaseEntry::Matched(mut held), CaseEntry::Matched(more)) => {
                held.extend(more.iter().cloned());
                CaseEntry::Matched(held)
            }
            _ => CaseEntry::Undecided,
        }
    }
}

fn retarget(function: &mut Function, from: BlockId, old: BlockId, new: BlockId) {
    match &mut function.block_mut(from).terminator {
        Terminator::Jump { target, .. } if *target == old => *target = new,
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => {
            if *then_block == old {
                *then_block = new;
            } else if *else_block == old {
                *else_block = new;
            }
        }
        _ => {}
    }
}

enum Written<'target, 'source> {
    Value(&'target Expression<'source>),
    Combined(BinaryOp, &'target Expression<'source>),
    Stepped(BinaryOp),
    Held(ValueId),
}

const THIS: &str = "this";

struct Captured {
    name: String,
    outer: ValueId,
    inner: ValueId,
}

struct Capturing<'a> {
    scope: &'a Scope,
    holder: &'a Function,
    names_further_out: Vec<String>,
    inherits_this: bool,
    taken: Vec<Captured>,
}

struct Context<'a> {
    file: &'a SourceFile,
    provider: &'a dyn TypeProvider,
    module_names: &'a ModuleNames,
    standing: &'a ModuleStanding,
    initialising_the_module: bool,
    declarations_bound_so_far: BTreeSet<String>,
    returns: TypeId,
    scope: Scope,
    statements: Vec<Statement>,
    current: BlockId,
    enclosing: Vec<Breakable>,
    shadowed: Vec<Key>,
    constructing: Option<Constructing>,
    written_names: &'a BTreeSet<String>,
    capturing: Option<Capturing<'a>>,
    nested: Vec<Function>,
    closures: usize,
}

fn span_of(span: oxc_span::Span) -> Span {
    Span::new(span.start, span.end)
}

fn refuse(rule: &'static str, span: oxc_span::Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::unsupported(rule, span_of(span), message)
        .with_help("Savitar lowers only what it can type without guessing")
}

fn unordered_read(
    standing: &ModuleStanding,
    specifier: &str,
    exported: &str,
    span: oxc_span::Span,
) -> Option<Diagnostic> {
    let at = span_of(span);
    match standing.of(specifier, exported) {
        Ran::BeforeAndBoundTheName => None,
        Ran::NotBefore => Some(
            Diagnostic::unsupported(
                "unsupported-import-order",
                at,
                format!(
                    "`{exported}` comes from `{specifier}`, which the evaluation order does not place before this module"
                ),
            )
            .with_help(
                "the initializer of this module would read a name that nothing has bound yet",
            ),
        ),
        Ran::WithoutBindingTheName => Some(
            Diagnostic::unsupported(
                "unsupported-import-unpublished",
                at,
                format!(
                    "`{specifier}` runs before this module and binds no `{exported}`, so this read points where nothing is bound"
                ),
            )
            .with_help(
                "either the module-level code that would bind it is refused there, or that module forwards the name instead of binding it",
            ),
        ),
        Ran::Unchecked => Some(
            Diagnostic::unsupported(
                "unsupported-import-unchecked",
                at,
                format!(
                    "`{exported}` comes from `{specifier}`, which this run does not lower, so nothing shows the name is bound when this initializer reads it"
                ),
            )
            .with_help("pass that file to Savitar too, so the order it runs in is checked and not assumed"),
        ),
    }
}

#[must_use]
pub fn lower(file: &SourceFile, provider: &dyn TypeProvider, table: &mut TypeTable) -> LowerReport {
    lower_in_order(file, provider, table, &ModuleStanding::alone())
}

#[must_use]
pub fn lower_in_order(
    file: &SourceFile,
    provider: &dyn TypeProvider,
    table: &mut TypeTable,
    standing: &ModuleStanding,
) -> LowerReport {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(file.name()).unwrap_or_else(|_| SourceType::ts());
    let parsed = Parser::new(&allocator, file.text(), source_type).parse();
    if parsed.fatal_error {
        return LowerReport {
            functions: Vec::new(),
            initializer: None,
            diagnostics: Vec::new(),
            published: BTreeSet::new(),
        };
    }

    let mut functions = Vec::new();
    let mut diagnostics = Vec::new();
    let holds = if source_type.is_typescript_definition() {
        WhatTheFileHolds::DeclarationsWithNoCodeBehindThem
    } else {
        WhatTheFileHolds::CheckedCode
    };
    let module_names = ModuleNames::of(&parsed.program.body, holds);
    let declares = WhatTheModuleDeclares {
        names: &module_names,
        holds,
    };
    let surroundings = Surroundings {
        file,
        provider,
        module_names: &module_names,
        standing,
    };
    let mut hoisted: Vec<Hoisted> = Vec::new();
    let mut binds: Vec<Binds<'_>> = Vec::new();
    let mut refused_before: Option<usize> = None;
    let mut rebound_by_refused_code: BTreeSet<String> = BTreeSet::new();

    for statement in &parsed.program.body {
        match at_module_level(statement, declares) {
            AtModuleLevel::Lowered(declared) => {
                note_what_a_declaration_binds(declared, &mut hoisted, &mut binds);
                for outcome in lower_declared(surroundings, table, declared) {
                    match outcome {
                        Ok(function) => functions.push(function),
                        Err(problems) => diagnostics.extend(problems),
                    }
                }
            }
            AtModuleLevel::Initializes(declaration) => binds.push(Binds::Variables(declaration)),
            AtModuleLevel::CarriesNoCode => {}
            AtModuleLevel::Forwards(form) => {
                if refused_before.is_none() {
                    refused_before = Some(binds.len());
                }
                diagnostics.push(refuse_a_forwarded_name(statement_span(statement), form));
            }
            AtModuleLevel::NotLowered(form) => {
                if refused_before.is_none() {
                    refused_before = Some(binds.len());
                }
                rebound_by_refused_code.extend(names_a_refused_statement_could_rebind(statement));
                diagnostics.push(refuse_at_module_level(statement_span(statement), form));
            }
            AtModuleLevel::Declares(form) => {
                if refused_before.is_none() {
                    refused_before = Some(binds.len());
                }
                rebound_by_refused_code.extend(names_a_refused_statement_could_rebind(statement));
                diagnostics.push(refuse_a_declaration_with_nothing_behind_it(
                    statement_span(statement),
                    form,
                ));
            }
        }
    }

    let initializer = match lower_module_initializer(
        surroundings,
        table,
        &parsed.program.body,
        &hoisted,
        &binds,
        &functions,
    ) {
        None => None,
        Some(Ok(initializer)) => Some(initializer),
        Some(Err(problems)) => {
            diagnostics.extend(problems);
            None
        }
    };
    let published = if initializer.is_some() {
        names_the_module_publishes(&hoisted, &binds, refused_before, &rebound_by_refused_code)
    } else {
        BTreeSet::new()
    };

    diagnostics.sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.rule));
    LowerReport {
        functions,
        initializer,
        diagnostics,
        published,
    }
}

fn names_a_refused_statement_could_rebind(statement: &AstStatement<'_>) -> BTreeSet<String> {
    let mut found = assigned::names_written_through_closures(std::slice::from_ref(statement));
    let declares = match statement {
        AstStatement::VariableDeclaration(declaration) => Some(&**declaration),
        AstStatement::ExportDeclaration(export) => match &export.declaration {
            Declaration::VariableDeclaration(declaration) => Some(&**declaration),
            _ => None,
        },
        _ => None,
    };
    if let Some(declaration) = declares {
        for declarator in &declaration.declarations {
            found.extend(names_bound_by(&declarator.id));
        }
    }
    found
}

fn names_the_module_publishes(
    hoisted: &[Hoisted],
    binds: &[Binds<'_>],
    refused_before: Option<usize>,
    rebound_by_refused_code: &BTreeSet<String>,
) -> BTreeSet<String> {
    let bound_before_any_refusal = refused_before.unwrap_or(binds.len());
    let mut published: BTreeSet<String> = hoisted.iter().map(|one| one.name.clone()).collect();
    for binding in &binds[..bound_before_any_refusal] {
        match binding {
            Binds::AClass { name, .. } => {
                published.insert(name.clone());
            }
            Binds::Variables(declaration) => {
                for declarator in &declaration.declarations {
                    published.extend(names_bound_by(&declarator.id));
                }
            }
        }
    }
    for name in rebound_by_refused_code {
        published.remove(name);
    }
    published
}

#[derive(Clone, Copy)]
enum Declared<'a> {
    Standalone(&'a oxc_ast::ast::Function<'a>),
    Class(&'a Class<'a>),
}

struct Hoisted {
    name: String,
    at: oxc_span::Span,
}

enum Binds<'a> {
    Variables(&'a VariableDeclaration<'a>),
    AClass { name: String, at: oxc_span::Span },
}

fn note_what_a_declaration_binds<'a>(
    declared: Declared<'a>,
    hoisted: &mut Vec<Hoisted>,
    binds: &mut Vec<Binds<'a>>,
) {
    match declared {
        Declared::Standalone(declaration) => {
            if let Some(id) = declaration.id.as_ref()
                && declaration.body.is_some()
            {
                hoisted.push(Hoisted {
                    name: id.name.to_string(),
                    at: id.span,
                });
            }
        }
        Declared::Class(class) => {
            if let Some(id) = class.id.as_ref()
                && !class.declare
            {
                binds.push(Binds::AClass {
                    name: id.name.to_string(),
                    at: id.span,
                });
            }
        }
    }
}

enum AtModuleLevel<'a> {
    Lowered(Declared<'a>),
    Initializes(&'a VariableDeclaration<'a>),
    CarriesNoCode,
    NotLowered(&'static str),
    Forwards(&'static str),
    Declares(&'static str),
}

#[derive(Clone, Copy)]
struct WhatTheModuleDeclares<'a> {
    names: &'a ModuleNames,
    holds: WhatTheFileHolds,
}

impl WhatTheModuleDeclares<'_> {
    const fn every_declaration_is_ambient(self) -> bool {
        matches!(
            self.holds,
            WhatTheFileHolds::DeclarationsWithNoCodeBehindThem
        )
    }

    fn nothing_is_behind_this_function(self, declaration: &oxc_ast::ast::Function<'_>) -> bool {
        if declaration.body.is_some() {
            return false;
        }
        declaration.id.as_ref().is_none_or(|id| {
            self.names.get(id.name.as_str()) != Some(&ModuleName::AHoistedFunction)
        })
    }

    fn nothing_is_behind_this_class(self, class: &Class<'_>) -> bool {
        class.declare || self.every_declaration_is_ambient()
    }

    fn nothing_is_behind_these_variables(self, declaration: &VariableDeclaration<'_>) -> bool {
        declaration.declare || self.every_declaration_is_ambient()
    }
}

fn initializes_or_refuses<'a>(
    declaration: &'a VariableDeclaration<'a>,
    declares: WhatTheModuleDeclares<'_>,
) -> AtModuleLevel<'a> {
    if declares.nothing_is_behind_these_variables(declaration) {
        return AtModuleLevel::Declares("a variable declared with no initialiser behind it");
    }
    if declaration.kind == VariableDeclarationKind::Var {
        return AtModuleLevel::NotLowered("a `var` declaration");
    }
    AtModuleLevel::Initializes(declaration)
}

fn lowered_or_declares<'a>(
    declaration: &'a oxc_ast::ast::Function<'a>,
    declares: WhatTheModuleDeclares<'_>,
) -> AtModuleLevel<'a> {
    if declaration.body.is_some() {
        return AtModuleLevel::Lowered(Declared::Standalone(declaration));
    }
    if declares.nothing_is_behind_this_function(declaration) {
        return AtModuleLevel::Declares("a function declared with no body");
    }
    AtModuleLevel::CarriesNoCode
}

fn lowered_or_declares_a_class<'a>(
    class: &'a Class<'a>,
    declares: WhatTheModuleDeclares<'_>,
) -> AtModuleLevel<'a> {
    if declares.nothing_is_behind_this_class(class) {
        return AtModuleLevel::Declares("a class declared with no code behind it");
    }
    AtModuleLevel::Lowered(Declared::Class(class))
}

fn every_forwarded_name_is_a_type(declaration: &oxc_ast::ast::ExportFromDeclaration<'_>) -> bool {
    !declaration.specifiers.is_empty()
        && declaration
            .specifiers
            .iter()
            .all(|specifier| specifier.export_kind == ImportOrExportKind::Type)
}

fn refuse_a_forwarded_name(span: oxc_span::Span, form: &'static str) -> Diagnostic {
    Diagnostic::unsupported(
        "unsupported-reexport",
        span_of(span),
        format!("{form} forwards a name this module never binds"),
    )
    .with_help(
        "the name table of a module holds what the module binds, so a module that reads a \
         forwarded name from here is refused by that name",
    )
}

fn refuse_a_declaration_with_nothing_behind_it(
    span: oxc_span::Span,
    form: &'static str,
) -> Diagnostic {
    Diagnostic::unsupported(
        "unsupported-ambient-declaration",
        span_of(span),
        format!("{form} binds no name, because nothing this module emits stands behind it"),
    )
    .with_help(
        "a declared boundary is not a verified boundary: Savitar takes a name only where \
         checked code binds it, so the declaration is refused where it is written",
    )
}

fn refuse_at_module_level(span: oxc_span::Span, form: &'static str) -> Diagnostic {
    Diagnostic::unsupported(
        "unsupported-module-statement",
        span_of(span),
        format!("{form} at module level is not lowered yet"),
    )
    .with_help(
        "Savitar lowers function bodies; module-level code needs a module initializer, \
         which does not exist yet",
    )
}

fn at_module_level<'a>(
    statement: &'a AstStatement<'a>,
    declares: WhatTheModuleDeclares<'_>,
) -> AtModuleLevel<'a> {
    match statement {
        AstStatement::FunctionDeclaration(declaration) => {
            lowered_or_declares(declaration, declares)
        }
        AstStatement::ClassDeclaration(class) => lowered_or_declares_a_class(class, declares),
        AstStatement::ExportDeclaration(export) => {
            declared_by_export(&export.declaration, declares)
        }
        AstStatement::ExportDefaultDeclaration(export) => match &export.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(declaration) => {
                lowered_or_declares(declaration, declares)
            }
            ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                lowered_or_declares_a_class(class, declares)
            }
            ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => AtModuleLevel::CarriesNoCode,
            _ => AtModuleLevel::NotLowered("an `export default` of a value"),
        },
        AstStatement::TSTypeAliasDeclaration(_) | AstStatement::TSInterfaceDeclaration(_) => {
            AtModuleLevel::CarriesNoCode
        }
        AstStatement::ImportDeclaration(_)
        | AstStatement::ExportNamedDeclaration(_)
        | AstStatement::EmptyStatement(_) => AtModuleLevel::CarriesNoCode,
        AstStatement::ExportFromDeclaration(declaration) => {
            if declaration.export_kind == ImportOrExportKind::Type
                || every_forwarded_name_is_a_type(declaration)
            {
                AtModuleLevel::CarriesNoCode
            } else {
                AtModuleLevel::Forwards("a re-export")
            }
        }
        AstStatement::ExportAllDeclaration(declaration) => {
            if declaration.export_kind == ImportOrExportKind::Type {
                AtModuleLevel::CarriesNoCode
            } else {
                AtModuleLevel::Forwards("a re-export of everything another module exports")
            }
        }
        AstStatement::VariableDeclaration(declaration) => {
            initializes_or_refuses(declaration, declares)
        }
        other => AtModuleLevel::NotLowered(module_level_form(other)),
    }
}

fn declared_by_export<'a>(
    declaration: &'a Declaration<'a>,
    declares: WhatTheModuleDeclares<'_>,
) -> AtModuleLevel<'a> {
    match declaration {
        Declaration::FunctionDeclaration(declaration) => lowered_or_declares(declaration, declares),
        Declaration::ClassDeclaration(class) => lowered_or_declares_a_class(class, declares),
        Declaration::TSTypeAliasDeclaration(_) | Declaration::TSInterfaceDeclaration(_) => {
            AtModuleLevel::CarriesNoCode
        }
        Declaration::VariableDeclaration(declaration) => {
            initializes_or_refuses(declaration, declares)
        }
        Declaration::TSEnumDeclaration(_) => AtModuleLevel::NotLowered("an enum declaration"),
        Declaration::TSExternalModuleDeclaration(_) => {
            AtModuleLevel::NotLowered("a `declare module` declaration")
        }
        Declaration::TSNamespaceDeclaration(_) => {
            AtModuleLevel::NotLowered("a namespace declaration")
        }
        Declaration::TSGlobalDeclaration(_) => {
            AtModuleLevel::NotLowered("a `declare global` declaration")
        }
        Declaration::TSImportEqualsDeclaration(_) => {
            AtModuleLevel::NotLowered("an `import =` declaration")
        }
    }
}

fn module_level_form(statement: &AstStatement<'_>) -> &'static str {
    match statement {
        AstStatement::ExpressionStatement(_) => "an expression statement",
        AstStatement::BlockStatement(_) => "a block",
        AstStatement::IfStatement(_) => "an `if`",
        AstStatement::ForStatement(_) => "a `for`",
        AstStatement::ForInStatement(_) => "a `for in`",
        AstStatement::ForOfStatement(_) => "a `for of`",
        AstStatement::WhileStatement(_) => "a `while`",
        AstStatement::DoWhileStatement(_) => "a `do while`",
        AstStatement::SwitchStatement(_) => "a `switch`",
        AstStatement::TryStatement(_) => "a `try`",
        AstStatement::ThrowStatement(_) => "a `throw`",
        AstStatement::LabeledStatement(_) => "a labelled statement",
        AstStatement::WithStatement(_) => "a `with`",
        AstStatement::DebuggerStatement(_) => "a `debugger`",
        AstStatement::ReturnStatement(_) => "a `return`",
        AstStatement::BreakStatement(_) => "a `break`",
        AstStatement::ContinueStatement(_) => "a `continue`",
        AstStatement::TSEnumDeclaration(_) => "an enum declaration",
        AstStatement::TSExternalModuleDeclaration(_) => "a `declare module` declaration",
        AstStatement::TSNamespaceDeclaration(_) => "a namespace declaration",
        AstStatement::TSGlobalDeclaration(_) => "a `declare global` declaration",
        AstStatement::TSImportEqualsDeclaration(_) => "an `import =` declaration",
        AstStatement::TSExportAssignment(_) => "an `export =` assignment",
        AstStatement::TSNamespaceExportDeclaration(_) => "a namespace export declaration",
        _ => "this statement",
    }
}

type Outcome = Result<Function, Vec<Diagnostic>>;

#[derive(Clone, Copy)]
struct Surroundings<'a> {
    file: &'a SourceFile,
    provider: &'a dyn TypeProvider,
    module_names: &'a ModuleNames,
    standing: &'a ModuleStanding,
}

fn lower_module_initializer(
    surroundings: Surroundings<'_>,
    table: &mut TypeTable,
    body: &[AstStatement<'_>],
    hoisted: &[Hoisted],
    binds: &[Binds<'_>],
    lowered_alongside: &[Function],
) -> Option<Result<ModuleInitializer, Vec<Diagnostic>>> {
    if hoisted.is_empty() && binds.is_empty() {
        return None;
    }
    let written = assigned::names_written_through_closures(body);
    Some(build_module_initializer(
        surroundings,
        table,
        &written,
        hoisted,
        binds,
        lowered_alongside,
    ))
}

fn where_the_module_starts_binding(hoisted: &[Hoisted], binds: &[Binds<'_>]) -> oxc_span::Span {
    let from_a_declaration = hoisted.first().map(|one| one.at);
    let from_the_body = binds.first().map(|one| match one {
        Binds::Variables(declaration) => declaration.span,
        Binds::AClass { at, .. } => *at,
    });
    from_a_declaration
        .or(from_the_body)
        .unwrap_or_else(|| oxc_span::Span::new(0, 0))
}

fn build_module_initializer<'a>(
    surroundings: Surroundings<'a>,
    table: &mut TypeTable,
    written_names: &'a BTreeSet<String>,
    hoisted: &[Hoisted],
    binds: &[Binds<'_>],
    lowered_alongside: &[Function],
) -> Result<ModuleInitializer, Vec<Diagnostic>> {
    let mut function = Function::new(format!("{}@module", surroundings.file.name()));
    let entry = function.add_block(empty_block(where_the_module_starts_binding(hoisted, binds)));
    let void = table.intern(TypeKind::Void);
    let mut context = Context {
        file: surroundings.file,
        provider: surroundings.provider,
        module_names: surroundings.module_names,
        standing: surroundings.standing,
        initialising_the_module: true,
        declarations_bound_so_far: BTreeSet::new(),
        returns: void,
        scope: Scope::new(),
        statements: Vec::new(),
        current: entry,
        enclosing: Vec::new(),
        shadowed: Vec::new(),
        constructing: None,
        written_names,
        capturing: None,
        nested: Vec::new(),
        closures: 0,
    };

    for one in hoisted {
        context.statements.push(Statement::BindDeclared {
            name: one.name.clone(),
            span: span_of(one.at),
        });
    }

    for binding in binds {
        match binding {
            Binds::AClass { name, at } => {
                context.statements.push(Statement::BindDeclared {
                    name: name.clone(),
                    span: span_of(*at),
                });
                context.declarations_bound_so_far.insert(name.clone());
            }
            Binds::Variables(declaration) => {
                let widening = declaration.kind != VariableDeclarationKind::Const;
                for declarator in &declaration.declarations {
                    if declarator.init.is_none() {
                        return Err(vec![refuse(
                            "unsupported-module-statement",
                            declarator.span,
                            "a module-level declaration with no initialiser is not lowered yet, because the module initializer would leave the name unbound",
                        )]);
                    }
                    context.lower_declarator(table, &mut function, declarator, widening)?;
                    for name in names_bound_by(&declarator.id) {
                        let held = context.bound(&name, declarator.id.span())?;
                        let value = held.value_or_refuse(&name, declarator.span)?;
                        context.statements.push(Statement::BindModule {
                            name,
                            value,
                            span: span_of(declarator.span),
                        });
                    }
                }
            }
        }
    }
    context.seal(&mut function, Terminator::Return(None));

    if let Some(problem) =
        a_call_the_graph_cannot_clear(&function, &context.nested, lowered_alongside)
    {
        return Err(vec![problem]);
    }

    let malformations = verify(&function);
    if !malformations.is_empty() {
        return Err(vec![refuse(
            "malformed-ir",
            where_the_module_starts_binding(hoisted, binds),
            format!("lowering produced invalid code: {malformations:?}"),
        )]);
    }
    Ok(ModuleInitializer {
        function,
        closures: context.nested,
    })
}

fn a_call_the_graph_cannot_clear(
    initializer: &Function,
    closures: &[Function],
    lowered_alongside: &[Function],
) -> Option<Diagnostic> {
    let graph = CallGraph::over(
        std::iter::once(initializer)
            .chain(closures)
            .chain(lowered_alongside),
    );
    for site in graph.calls_made_by(initializer.name()) {
        match graph.what_a_call_reaches(&site) {
            Reach::NoModuleNameIsTouched => {}
            Reach::AModuleNameIsTouched { touch, through, .. } => {
                return Some(
                    Diagnostic::unsupported(
                        "unsupported-module-initializer",
                        site.at,
                        format!("a call while this module initialises reaches code where {touch}"),
                    )
                    .with_help(format!(
                        "the call graph reaches that read through {}, and a name read while the module initialises may hold memory nothing has written",
                        said_as_a_path(&through)
                    )),
                );
            }
            Reach::ACalleeIsNotKnown {
                callee, through, ..
            } => {
                return Some(
                    Diagnostic::unsupported(
                        "unsupported-module-initializer",
                        site.at,
                        if through.is_empty() {
                            format!(
                                "a call while this module initialises goes to {callee}, which the call graph does not resolve"
                            )
                        } else {
                            format!(
                                "a call while this module initialises reaches a call to {callee}, which the call graph does not resolve"
                            )
                        },
                    )
                    .with_help(if through.is_empty() {
                        "a target the call graph cannot resolve is treated as the worst case, so nothing shows the call stays away from module bindings".to_string()
                    } else {
                        format!(
                            "the call graph reaches that call through {}, and a target it cannot resolve is treated as the worst case",
                            said_as_a_path(&through)
                        )
                    }),
                );
            }
        }
    }
    None
}

fn lower_declared(
    surroundings: Surroundings<'_>,
    table: &mut TypeTable,
    declared: Declared<'_>,
) -> Vec<Outcome> {
    match declared {
        Declared::Standalone(declaration) => {
            let Some(body) = declaration.body.as_ref() else {
                return Vec::new();
            };
            let name = declaration
                .id
                .as_ref()
                .map_or_else(|| "anonymous".to_string(), |id| id.name.to_string());
            let lowering = Lowering {
                name,
                constructs: None,
                signature: declaration
                    .id
                    .as_ref()
                    .map_or(Signature::NoPositionAnswersOne, |id| {
                        Signature::AtTheName(id.span)
                    }),
                receiver: Receiver::Detached,
                code: Code::AFunction(declaration),
                body: Body::Statements(body),
            };
            let written = assigned::names_written_through_closures(&body.statements);
            outcomes_of(lower_function(
                surroundings,
                table,
                &written,
                &lowering,
                None,
            ))
        }
        Declared::Class(class) => lower_class(surroundings, table, class),
    }
}

fn lower_class(
    surroundings: Surroundings<'_>,
    table: &mut TypeTable,
    class: &Class<'_>,
) -> Vec<Outcome> {
    let Some(id) = class.id.as_ref() else {
        return vec![Err(vec![refuse(
            "unsupported-class",
            class.span,
            "a class with no name is not lowered yet, because its shape is asked for by name",
        )])];
    };
    let declared_base = match class.heritage.as_ref() {
        None => None,
        Some(heritage) => match &heritage.expression {
            Expression::Identifier(named) => Some((named.name.as_str(), named.span)),
            other => {
                return vec![Err(vec![refuse(
                    "unsupported-class",
                    expression_span(other),
                    "a base class that is not a name is not lowered yet, because the base construction is named in the code Savitar emits",
                )])];
            }
        },
    };
    let constructible = match declared_base {
        None => None,
        Some((_, at)) => match base_construct_signatures(surroundings, table, at) {
            Ok(constructible) => Some(constructible),
            Err(refusals) => return vec![Err(refusals)],
        },
    };
    let base = match (declared_base, constructible.as_ref()) {
        (Some((name, at)), Some(constructible)) => Some(BaseClass {
            name,
            at,
            forms: constructible.forms.as_slice(),
            statics: &constructible.statics,
        }),
        _ => None,
    };
    if let Some(declared) = base {
        if let Err(refusals) =
            inherited_members_fit_the_base(surroundings, table, id.span, declared)
        {
            return vec![Err(refusals)];
        }
        if let Err(refusals) = static_members_fit_the_base(surroundings, table, class, declared) {
            return vec![Err(refusals)];
        }
    }
    let mut outcomes = Vec::new();
    for element in &class.body.body {
        match element {
            ClassElement::MethodDefinition(method) => {
                outcomes.extend(lower_method(
                    surroundings,
                    table,
                    id.name.as_str(),
                    id.span,
                    base,
                    method,
                ));
            }
            ClassElement::StaticBlock(block) => outcomes.push(Err(vec![refuse(
                "unsupported-class-member",
                block.span,
                "a static initialisation block is not lowered yet",
            )])),
            ClassElement::AccessorProperty(accessor) => outcomes.push(Err(vec![refuse(
                "unsupported-class-member",
                accessor.span,
                "an `accessor` field is not lowered yet",
            )])),
            ClassElement::PropertyDefinition(property) => {
                if property.value.is_some() {
                    outcomes.push(Err(vec![refuse(
                        "unsupported-class-member",
                        property.span,
                        "a field initialiser outside the constructor is not lowered yet",
                    )]));
                }
            }
            ClassElement::TSIndexSignature(_) => {}
        }
    }
    outcomes
}

fn lower_method<'a>(
    surroundings: Surroundings<'_>,
    table: &mut TypeTable,
    class: &str,
    class_at: oxc_span::Span,
    base: Option<BaseClass<'a>>,
    method: &'a MethodDefinition<'a>,
) -> Vec<Outcome> {
    let Some(body) = method.value.body.as_ref() else {
        return Vec::new();
    };
    let member = match &method.key {
        PropertyKey::StaticIdentifier(id) => id,
        _ => {
            return vec![Err(vec![refuse(
                "unsupported-class-member",
                method.span,
                "a computed or private member name is not lowered yet",
            )])];
        }
    };
    if matches!(
        method.kind,
        MethodDefinitionKind::Get | MethodDefinitionKind::Set
    ) {
        return vec![Err(vec![refuse(
            "unsupported-class-member",
            method.span,
            "an accessor body is not lowered yet",
        )])];
    }
    let name = format!("{class}.{}", member.name);
    let constructing = method.kind == MethodDefinitionKind::Constructor;
    let lowering = Lowering {
        name,
        constructs: constructing.then(|| class.to_string()),
        signature: if constructing {
            Signature::NoPositionAnswersOne
        } else {
            Signature::AtTheName(member.span)
        },
        receiver: match (constructing, method.r#static) {
            (true, _) => Receiver::UnderConstruction { class_at, base },
            (false, true) => Receiver::Detached,
            (false, false) => Receiver::Instance(class_at),
        },
        code: Code::AFunction(&method.value),
        body: Body::Statements(body),
    };
    let written = assigned::names_written_through_closures(&body.statements);
    outcomes_of(lower_function(
        surroundings,
        table,
        &written,
        &lowering,
        None,
    ))
}

#[derive(Clone, Copy)]
struct BaseClass<'a> {
    name: &'a str,
    at: oxc_span::Span,
    forms: &'a [TypeId],
    statics: &'a StaticSide,
}

struct Constructible {
    forms: Vec<TypeId>,
    statics: StaticSide,
}

fn base_construct_signatures(
    surroundings: Surroundings<'_>,
    table: &mut TypeTable,
    at: oxc_span::Span,
) -> Result<Constructible, Vec<Diagnostic>> {
    let constructor = type_at_the_name(surroundings, table, at)?;
    let forms = types::constructible_forms(table, constructor)
        .map_err(|reason| vec![refuse("unsupported-class-member", at, reason)])?;
    let statics = table
        .static_side_of(constructor)
        .cloned()
        .unwrap_or_else(|| StaticSide::NotModelled("this value is not constructible".to_string()));
    Ok(Constructible { forms, statics })
}

fn type_at_the_name(
    surroundings: Surroundings<'_>,
    table: &mut TypeTable,
    span: oxc_span::Span,
) -> Result<TypeId, Vec<Diagnostic>> {
    let query = Span::new(span.end.saturating_sub(1), span.end);
    match surroundings
        .provider
        .type_at(table, surroundings.file.name(), query)
    {
        Answer::Known(id) => Ok(id),
        Answer::Unsupported(reason) => Err(vec![refuse("unsupported-type", span, reason)]),
        Answer::OutOfScope => Err(vec![refuse(
            "no-type-answer",
            span,
            "the provider has no type here",
        )]),
    }
}

fn inherited_members_fit_the_base(
    surroundings: Surroundings<'_>,
    table: &mut TypeTable,
    class_at: oxc_span::Span,
    base: BaseClass<'_>,
) -> Result<(), Vec<Diagnostic>> {
    let derived = type_at_the_name(surroundings, table, class_at)?;
    if !types::is_shaped(table, derived) {
        return Err(vec![refuse(
            "unsupported-class",
            class_at,
            "the provider answers no shape at the name of this class, so what it inherits cannot be related to its base",
        )]);
    }
    let inherited = types::shapes_of_what_is_constructed(table, base.forms)
        .map_err(|reason| vec![refuse("unsupported-class-member", base.at, reason)])?;
    for shape in inherited {
        if savitar_types::mentions_a_type_parameter(table, shape) {
            return Err(vec![refuse(
                "unsupported-class",
                base.at,
                format!(
                    "`{}` is a generic base, and the position of its name answers the \
                     uninstantiated shape, so what this class inherits cannot be related to it",
                    base.name
                ),
            )]);
        }
        if !savitar_types::assignable(table, derived, shape) {
            return Err(vec![refuse_unsound_inheritance(class_at, base.name)]);
        }
    }
    Ok(())
}

struct DeclaredStatic<'a> {
    name: &'a str,
    at: oxc_span::Span,
    optional: bool,
    method: bool,
    readonly: bool,
}

fn statics_declared_by<'a>(class: &'a Class<'_>) -> Vec<DeclaredStatic<'a>> {
    let mut declared = Vec::new();
    for element in &class.body.body {
        match element {
            ClassElement::PropertyDefinition(property) if property.r#static => {
                if let PropertyKey::StaticIdentifier(key) = &property.key {
                    declared.push(DeclaredStatic {
                        name: key.name.as_str(),
                        at: key.span,
                        optional: property.optional,
                        method: false,
                        readonly: property.readonly,
                    });
                }
            }
            ClassElement::MethodDefinition(method)
                if method.r#static && method.kind == MethodDefinitionKind::Method =>
            {
                if let PropertyKey::StaticIdentifier(key) = &method.key {
                    declared.push(DeclaredStatic {
                        name: key.name.as_str(),
                        at: key.span,
                        optional: method.optional,
                        method: true,
                        readonly: false,
                    });
                }
            }
            _ => {}
        }
    }
    declared
}

fn static_members_fit_the_base(
    surroundings: Surroundings<'_>,
    table: &mut TypeTable,
    class: &Class<'_>,
    base: BaseClass<'_>,
) -> Result<(), Vec<Diagnostic>> {
    let declared = statics_declared_by(class);
    if declared.is_empty() {
        return Ok(());
    }

    let inherited = match base.statics {
        StaticSide::Shaped(shape) => *shape,
        StaticSide::NotModelled(reason) => {
            return Err(vec![refuse(
                "unsupported-class",
                base.at,
                format!(
                    "`{}` declares static members Savitar does not model, so what this class \
                     redeclares static cannot be related to them: {reason}",
                    base.name
                ),
            )]);
        }
    };
    let TypeKind::Object { fields, .. } = table.kind(inherited).clone() else {
        return Err(vec![refuse(
            "unsupported-class",
            base.at,
            format!(
                "the static side of `{}` is not a shape, so what this class redeclares static \
                 cannot be related to it",
                base.name
            ),
        )]);
    };

    let mut redeclared: Vec<(oxc_span::Span, Field, Field)> = Vec::new();
    for member in &declared {
        let Some(inherited_field) = fields.iter().find(|field| field.name == member.name) else {
            continue;
        };
        let type_id = type_at_the_name(surroundings, table, member.at)?;
        redeclared.push((
            member.at,
            Field {
                name: member.name.to_string(),
                type_id,
                optional: member.optional,
                method: member.method,
                readonly: member.readonly,
            },
            inherited_field.clone(),
        ));
    }
    if redeclared.is_empty() {
        return Ok(());
    }

    let offered = table.object(
        redeclared
            .iter()
            .map(|(_, offered, _)| offered.clone())
            .collect(),
    );
    let promised = table.object(
        redeclared
            .iter()
            .map(|(_, _, promised)| promised.clone())
            .collect(),
    );
    if savitar_types::assignable(table, offered, promised) {
        return Ok(());
    }
    if let Some((at, offered, _)) = redeclared.iter().find(|(_, offered, promised)| {
        savitar_types::mentions_a_type_parameter(table, offered.type_id)
            || savitar_types::mentions_a_type_parameter(table, promised.type_id)
    }) {
        return Err(vec![refuse(
            "unsupported-class-member",
            *at,
            format!(
                "`{}` is a static member whose type mentions a type parameter, and Savitar \
                 cannot relate it to the one `{}` declares",
                offered.name, base.name
            ),
        )]);
    }
    Err(vec![refuse_a_narrowed_static(
        class.id.as_ref().map_or(class.span, |id| id.span),
        base.name,
    )])
}

fn refuse_a_narrowed_static(span: oxc_span::Span, base: &str) -> Diagnostic {
    Diagnostic::unsupported(
        "unsound-inheritance",
        span_of(span),
        format!(
            "a static member this class redeclares does not fit the one `{base}` declares, so a \
             static method of `{base}` could write through the wider view a value this class \
             promised could not be there"
        ),
    )
    .with_help(
        "a mutable static member a derived class redeclares has to be invariant, and a static \
         method it overrides has to take at least what the base takes; `savitar divergences` \
         lists this as `inherited-member-must-fit-the-base`",
    )
}

fn refuse_unsound_inheritance(span: oxc_span::Span, base: &str) -> Diagnostic {
    Diagnostic::unsupported(
        "unsound-inheritance",
        span_of(span),
        format!(
            "this class does not fit the shape of `{base}`, so a method of `{base}` running over \
             an instance of this class could write a member this class declared narrower"
        ),
    )
    .with_help(
        "a mutable member a derived class redeclares has to be invariant, and a method it \
         overrides has to take at least what the base takes; `savitar divergences` lists this as \
         `inherited-member-must-fit-the-base`",
    )
}

#[derive(Clone, Copy)]
enum Receiver<'a> {
    Detached,
    Instance(oxc_span::Span),
    UnderConstruction {
        class_at: oxc_span::Span,
        base: Option<BaseClass<'a>>,
    },
}

impl<'a> Receiver<'a> {
    const fn shape_at(self) -> Option<oxc_span::Span> {
        match self {
            Self::Detached => None,
            Self::Instance(at) | Self::UnderConstruction { class_at: at, .. } => Some(at),
        }
    }

    const fn constructs(self) -> bool {
        matches!(self, Self::UnderConstruction { .. })
    }

    const fn base(self) -> Option<BaseClass<'a>> {
        match self {
            Self::Detached | Self::Instance(_) => None,
            Self::UnderConstruction { base, .. } => base,
        }
    }
}

struct Constructing {
    receiver: ValueId,
    inherited: Vec<String>,
    base: Option<ConstructedBase>,
}

struct ConstructedBase {
    name: String,
    forms: Vec<TypeId>,
    standing: bool,
}

enum Signature {
    AtTheName(oxc_span::Span),
    Declared(TypeId),
    NoPositionAnswersOne,
}

#[derive(Clone, Copy)]
enum Code<'a> {
    AFunction(&'a oxc_ast::ast::Function<'a>),
    AnArrow(&'a oxc_ast::ast::ArrowFunctionExpression<'a>),
}

impl<'a> Code<'a> {
    const fn span(self) -> oxc_span::Span {
        match self {
            Self::AFunction(declaration) => declaration.span,
            Self::AnArrow(arrow) => arrow.span,
        }
    }

    const fn is_a_generator(self) -> bool {
        match self {
            Self::AFunction(declaration) => declaration.generator,
            Self::AnArrow(_) => false,
        }
    }

    const fn writes_its_own_this(self) -> bool {
        match self {
            Self::AFunction(declaration) => declaration.this_param.is_some(),
            Self::AnArrow(_) => false,
        }
    }

    const fn is_async(self) -> bool {
        match self {
            Self::AFunction(declaration) => declaration.r#async,
            Self::AnArrow(arrow) => arrow.r#async,
        }
    }

    const fn inherits_this(self) -> bool {
        matches!(self, Self::AnArrow(_))
    }

    fn params(self) -> &'a oxc_ast::ast::FormalParameters<'a> {
        match self {
            Self::AFunction(declaration) => &declaration.params,
            Self::AnArrow(arrow) => &arrow.params,
        }
    }
}

#[derive(Clone, Copy)]
enum Body<'a> {
    Statements(&'a FunctionBody<'a>),
    OneExpression(&'a Expression<'a>),
}

impl Body<'_> {
    fn span(self) -> oxc_span::Span {
        match self {
            Self::Statements(body) => body.span,
            Self::OneExpression(expression) => expression_span(expression),
        }
    }
}

struct Lowering<'a> {
    name: String,
    constructs: Option<String>,
    signature: Signature,
    receiver: Receiver<'a>,
    code: Code<'a>,
    body: Body<'a>,
}

struct Lowered {
    function: Function,
    nested: Vec<Function>,
    taken: Vec<Captured>,
}

fn outcomes_of(lowered: Result<Lowered, Vec<Diagnostic>>) -> Vec<Outcome> {
    match lowered {
        Ok(lowered) => std::iter::once(lowered.function)
            .chain(lowered.nested)
            .map(Ok)
            .collect(),
        Err(problems) => vec![Err(problems)],
    }
}

fn empty_block(at: oxc_span::Span) -> Block {
    Block {
        parameters: Vec::new(),
        statements: Vec::new(),
        terminator: Terminator::Unreachable(span_of(at)),
    }
}

fn lower_function<'a>(
    surroundings: Surroundings<'a>,
    table: &mut TypeTable,
    written_names: &'a BTreeSet<String>,
    lowering: &Lowering<'_>,
    capturing: Option<Capturing<'a>>,
) -> Result<Lowered, Vec<Diagnostic>> {
    let code = lowering.code;
    let body = lowering.body;
    let at = code.span();
    if code.is_a_generator() {
        return Err(vec![refuse(
            "unsupported-function",
            at,
            "a generator function is not lowered yet",
        )]);
    }
    if code.writes_its_own_this() {
        return Err(vec![refuse(
            "unsupported-function",
            at,
            "a written `this` parameter is not lowered yet",
        )]);
    }
    if let Some(rest) = code.params().rest.as_ref() {
        return Err(vec![refuse(
            "unsupported-pattern",
            rest.span,
            "a rest parameter is not lowered yet, and lowering the function without it would hide every argument it takes",
        )]);
    }
    if matches!(lowering.signature, Signature::NoPositionAnswersOne)
        && !lowering.receiver.constructs()
    {
        return Err(vec![refuse(
            "unsupported-function",
            at,
            "a function with no name is not lowered yet, because its signature is asked for by name and no position of it answers one",
        )]);
    }

    let mut function = Function::new(&lowering.name);
    if let Some(class) = lowering.constructs.as_deref() {
        function.constructs(class);
    }
    let entry = function.add_block(empty_block(at));
    let mut context = Context {
        file: surroundings.file,
        provider: surroundings.provider,
        module_names: surroundings.module_names,
        standing: surroundings.standing,
        initialising_the_module: false,
        declarations_bound_so_far: BTreeSet::new(),
        returns: table.intern(TypeKind::Unknown),
        scope: Scope::new(),
        statements: Vec::new(),
        current: entry,
        enclosing: Vec::new(),
        shadowed: Vec::new(),
        constructing: None,
        written_names,
        capturing,
        nested: Vec::new(),
        closures: 0,
    };

    if let Some(class_at) = lowering.receiver.shape_at() {
        let shape = context.type_at(table, class_at)?;
        if !types::is_shaped(table, shape) {
            return Err(vec![refuse(
                "unsupported-class-member",
                at,
                "the provider answers no shape at the name of this class, so `this` has no type",
            )]);
        }
        let held = function.declare_parameter(shape);
        context.declare(THIS, Binding::written(held, shape));
        if lowering.receiver.constructs() {
            let mut inherited = Vec::new();
            let mut base = None;
            if let Some(declared_base) = lowering.receiver.base() {
                inherited = types::fields_of_what_is_constructed(table, declared_base.forms)
                    .map_err(|reason| {
                        vec![refuse("unsupported-class-member", declared_base.at, reason)]
                    })?;
                base = Some(ConstructedBase {
                    name: declared_base.name.to_string(),
                    forms: declared_base.forms.to_vec(),
                    standing: false,
                });
            }
            context.constructing = Some(Constructing {
                receiver: held,
                inherited,
                base,
            });
        }
    }

    let signature = match lowering.signature {
        Signature::AtTheName(name_at) => Some(context.type_at(table, name_at)?),
        Signature::Declared(declared) => Some(declared),
        Signature::NoPositionAnswersOne => None,
    };
    let declared = match signature {
        Some(signature) => types::declared_return(table, signature)
            .map_err(|reason| vec![refuse("unsupported-function", at, reason)])?,
        None => table.intern(TypeKind::Void),
    };
    context.returns = if code.is_async() {
        types::awaited(table, declared)
            .map_err(|reason| vec![refuse("unsupported-function", at, reason)])?
    } else {
        declared
    };

    let takes_from_the_position = matches!(lowering.signature, Signature::Declared(_));
    let mut entering = Vec::new();
    for (position, parameter) in code.params().items.iter().enumerate() {
        if parameter.accessibility.is_some() || parameter.readonly || parameter.r#override {
            return Err(vec![refuse(
                "unsupported-pattern",
                parameter.span,
                "a parameter property declares a field as well, and that is not lowered yet",
            )]);
        }
        let default = parameter.initializer.as_deref();
        let declared = match &parameter.pattern {
            BindingPattern::BindingIdentifier(id) if !takes_from_the_position => {
                context.type_at(table, id.span)?
            }
            _ => {
                let Some(signature) = signature else {
                    return Err(vec![refuse(
                        "unsupported-pattern",
                        parameter.span,
                        "a destructuring parameter of a constructor is not lowered yet, because no position answers the construct signature",
                    )]);
                };
                types::declared_parameter(table, signature, position, default.is_some())
                    .map_err(|reason| vec![refuse("unsupported-pattern", parameter.span, reason)])?
            }
        };
        let Some(fallback) = default else {
            let value = function.declare_parameter(declared);
            match &parameter.pattern {
                BindingPattern::BindingIdentifier(id) => {
                    context.declare(id.name.as_str(), Binding::written(value, declared));
                }
                pattern => entering.push((pattern, None, value, parameter.span)),
            }
            continue;
        };
        let omitted = table.optional(declared);
        let value = function.declare_parameter(omitted);
        entering.push((&parameter.pattern, Some(fallback), value, parameter.span));
    }
    for (pattern, fallback, value, span) in entering {
        let value = match fallback {
            Some(fallback) => context.defaulted(table, &mut function, value, fallback, span)?,
            None => value,
        };
        context.bind_pattern(table, &mut function, pattern, value)?;
    }

    if lowering.receiver.base().is_some() {
        let begins_with_super = match body {
            Body::Statements(statements) => statements
                .statements
                .first()
                .is_some_and(statement_is_a_base_construction),
            Body::OneExpression(_) => false,
        };
        if !begins_with_super {
            return Err(vec![refuse(
                "unsupported-class-member",
                body.span(),
                "the constructor of a derived class is lowered only when `super()` is its first statement, because Savitar cannot yet prove that an earlier statement reads no field the base constructor writes",
            )]);
        }
    }

    let flow = match body {
        Body::Statements(statements) => {
            context.lower_statements(table, &mut function, &statements.statements)?
        }
        Body::OneExpression(returned) => {
            context.lower_one_expression_body(table, &mut function, returned)?
        }
    };
    if flow == Flow::Continues {
        if !types::leaving_without_a_value_fits(table, context.returns) {
            return Err(vec![refuse(
                "unsupported-return",
                body.span(),
                "this function can reach its end without returning a value the declared type admits",
            )]);
        }
        context.seal(&mut function, Terminator::Return(None));
    }

    let malformations = verify(&function);
    if !malformations.is_empty() {
        return Err(vec![refuse(
            "malformed-ir",
            body.span(),
            format!("lowering produced invalid code: {malformations:?}"),
        )]);
    }
    Ok(Lowered {
        function,
        nested: context.nested,
        taken: context
            .capturing
            .map(|capturing| capturing.taken)
            .unwrap_or_default(),
    })
}

fn statement_is_a_base_construction(statement: &AstStatement<'_>) -> bool {
    let AstStatement::ExpressionStatement(held) = statement else {
        return false;
    };
    base_construction_call(&held.expression).is_some()
}

fn base_construction_call<'a, 'source>(
    expression: &'a Expression<'source>,
) -> Option<&'a oxc_ast::ast::CallExpression<'source>> {
    match expression {
        Expression::CallExpression(call) => {
            matches!(call.callee, Expression::Super(_)).then_some(&**call)
        }
        Expression::ParenthesizedExpression(inner) => base_construction_call(&inner.expression),
        _ => None,
    }
}

fn statement_span(statement: &AstStatement<'_>) -> oxc_span::Span {
    use oxc_span::GetSpan;
    statement.span()
}

fn expression_span(expression: &Expression<'_>) -> oxc_span::Span {
    use oxc_span::GetSpan;
    expression.span()
}

impl Context<'_> {
    fn answer_at(&self, table: &mut TypeTable, span: oxc_span::Span) -> Answer {
        let query = Span::new(span.end.saturating_sub(1), span.end);
        self.provider.type_at(table, self.file.name(), query)
    }

    fn type_at(
        &self,
        table: &mut TypeTable,
        span: oxc_span::Span,
    ) -> Result<TypeId, Vec<Diagnostic>> {
        match self.answer_at(table, span) {
            Answer::Known(id) => Ok(id),
            Answer::Unsupported(reason) => Err(vec![refuse("unsupported-type", span, reason)]),
            Answer::OutOfScope => Err(vec![refuse(
                "no-type-answer",
                span,
                "the provider has no type here",
            )]),
        }
    }

    fn member_type(
        &self,
        table: &mut TypeTable,
        object: TypeId,
        name: &str,
        member: &oxc_ast::ast::StaticMemberExpression<'_>,
    ) -> Result<TypeId, Vec<Diagnostic>> {
        let shaped = match types::field_result(table, object, name) {
            Ok(id) => return Ok(id),
            Err(reason) => reason,
        };
        if types::is_union(table, object) {
            return Err(vec![refuse("unsupported-read", member.span, shaped)]);
        }
        match self.answer_at(table, member.property.span) {
            Answer::Known(id) => Ok(id),
            Answer::Unsupported(detail) => {
                Err(vec![refuse("unsupported-type", member.span, detail)])
            }
            Answer::OutOfScope => Err(vec![refuse("unsupported-read", member.span, shaped)]),
        }
    }

    fn assign(
        &mut self,
        function: &mut Function,
        ty: TypeId,
        value: Rvalue,
        span: oxc_span::Span,
    ) -> ValueId {
        let target = function.fresh_value(ty);
        self.statements.push(Statement::Assign {
            target,
            value,
            span: span_of(span),
        });
        target
    }

    fn seal(&mut self, function: &mut Function, terminator: Terminator) {
        let block = function.block_mut(self.current);
        block.statements = std::mem::take(&mut self.statements);
        block.terminator = terminator;
    }

    fn lower_one_expression_body(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        returned: &Expression<'_>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let at = expression_span(returned);
        if matches!(table.kind(self.returns), TypeKind::Void) {
            self.lower_effect(table, function, returned)?;
            self.seal(function, Terminator::Return(None));
            return Ok(Flow::Returned);
        }
        let wanted = self.returns;
        let value = self.lower_expression_expecting(table, function, returned, Some(wanted))?;
        if !savitar_types::assignable(table, function.type_of(value), wanted) {
            return Err(vec![refuse(
                "unsupported-return",
                at,
                "Savitar cannot show this value fits the declared return type",
            )]);
        }
        let carried = self.coerced(function, table, value, wanted, at);
        self.seal(function, Terminator::Return(Some(carried)));
        Ok(Flow::Returned)
    }

    fn captured(
        &mut self,
        function: &mut Function,
        name: &str,
        span: oxc_span::Span,
    ) -> Result<Option<ValueId>, Vec<Diagnostic>> {
        let Some(capturing) = self.capturing.as_ref() else {
            return Ok(None);
        };
        if let Some(held) = capturing.taken.iter().find(|held| held.name == name) {
            let inner = held.inner;
            let declared = function.type_of(inner);
            self.hold_a_capture(name, inner, declared);
            return Ok(Some(inner));
        }
        let Some(binding) = capturing.scope.get(name) else {
            if capturing.names_further_out.iter().any(|held| held == name) {
                return Err(vec![refuse(
                    "unsupported-capture",
                    span,
                    format!(
                        "`{name}` is a local of a function further out than the one around this closure, and capture reaches only one function out yet"
                    ),
                )]);
            }
            return Ok(None);
        };
        if self.written_names.contains(name) {
            return Err(vec![refuse(
                "unsupported-capture",
                span,
                format!(
                    "`{name}` is written somewhere in this function, and a closure that captured it by value would not see the write"
                ),
            )]);
        }
        if binding.narrowed_by_a_field {
            return Err(vec![refuse(
                "unsupported-capture",
                span,
                format!(
                    "`{name}` is narrowed here by a fact about one of its fields, and a call could falsify that fact while this closure still holds the narrowed value"
                ),
            )]);
        }
        let outer = binding.value_or_refuse(name, span)?;
        let declared = capturing.holder.type_of(outer);
        let inner = function.declare_capture(name, declared);
        if let Some(capturing) = self.capturing.as_mut() {
            capturing.taken.push(Captured {
                name: name.to_string(),
                outer,
                inner,
            });
        }
        self.hold_a_capture(name, inner, declared);
        Ok(Some(inner))
    }

    fn hold_a_capture(&mut self, name: &str, value: ValueId, declared: TypeId) {
        self.scope.put(
            &Key {
                name: name.to_string(),
                depth: 0,
            },
            Binding::written(value, declared),
        );
    }

    fn holds_a_capture_of(&self, name: &str) -> bool {
        self.capturing.as_ref().is_some_and(|capturing| {
            capturing.taken.iter().any(|held| held.name == name)
                && self.scope.innermost(name).is_some_and(|key| key.depth == 0)
        })
    }

    fn reaches_a_name_further_out(&self, name: &str) -> bool {
        self.capturing.as_ref().is_some_and(|capturing| {
            capturing.scope.holds(name)
                || capturing.names_further_out.iter().any(|held| held == name)
        })
    }

    fn lower_closure(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        code: Code<'_>,
        body: Body<'_>,
        expected: Option<TypeId>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let at = code.span();
        let Some(expected) = expected else {
            return Err(vec![refuse(
                "unsupported-expression",
                at,
                "nothing at this position declares the type of this function, and a signature is asked for by name",
            )]);
        };
        let forms = types::callable_forms(table, expected)
            .map_err(|reason| vec![refuse("unsupported-expression", at, reason)])?;
        let [signature] = forms.as_slice() else {
            return Err(vec![refuse(
                "unsupported-expression",
                at,
                "the type declared at this position holds more than one call form, and Savitar does not choose between them",
            )]);
        };
        let signature = *signature;
        if types::takes_type_arguments(table, signature) {
            return Err(vec![refuse(
                "unsupported-expression",
                at,
                "a function whose declared type takes type arguments of its own is not lowered yet",
            )]);
        }

        let name = format!("{}#{}", function.name(), self.closures);
        self.closures += 1;
        let mut names_further_out = Vec::new();
        if let Some(capturing) = self.capturing.as_ref() {
            names_further_out.extend(capturing.scope.keys().into_iter().map(|key| key.name));
            names_further_out.extend(capturing.names_further_out.iter().cloned());
        }
        let lowering = Lowering {
            name: name.clone(),
            constructs: None,
            signature: Signature::Declared(signature),
            receiver: Receiver::Detached,
            code,
            body,
        };
        let lowered = lower_function(
            Surroundings {
                file: self.file,
                provider: self.provider,
                module_names: self.module_names,
                standing: self.standing,
            },
            table,
            self.written_names,
            &lowering,
            Some(Capturing {
                scope: &self.scope,
                holder: function,
                names_further_out,
                inherits_this: code.inherits_this(),
                taken: Vec::new(),
            }),
        )?;
        let captured: Vec<ValueId> = lowered.taken.iter().map(|held| held.outer).collect();
        self.nested.push(lowered.function);
        self.nested.extend(lowered.nested);
        Ok(self.assign(
            function,
            signature,
            Rvalue::Closure {
                function: name,
                captured,
            },
            at,
        ))
    }

    fn lower_statements(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statements: &[AstStatement<'_>],
    ) -> Result<Flow, Vec<Diagnostic>> {
        for statement in statements {
            let flow = self.lower_statement(table, function, statement)?;
            if !flow.continues() {
                return Ok(flow);
            }
        }
        Ok(Flow::Continues)
    }

    fn lower_statement(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &AstStatement<'_>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        match statement {
            AstStatement::VariableDeclaration(declaration) => {
                let widening = declaration.kind != VariableDeclarationKind::Const;
                for declarator in &declaration.declarations {
                    self.lower_declarator(table, function, declarator, widening)?;
                }
                Ok(Flow::Continues)
            }
            AstStatement::ExpressionStatement(statement) => {
                self.lower_effect(table, function, &statement.expression)?;
                Ok(Flow::Continues)
            }
            AstStatement::ReturnStatement(statement) => {
                let returned = match &statement.argument {
                    Some(argument) => {
                        let wanted = self.returns;
                        let value = self.lower_expression_expecting(
                            table,
                            function,
                            argument,
                            Some(wanted),
                        )?;
                        if !savitar_types::assignable(table, function.type_of(value), wanted) {
                            return Err(vec![refuse(
                                "unsupported-return",
                                statement.span,
                                "Savitar cannot show this value fits the declared return type",
                            )]);
                        }
                        Some(self.coerced(function, table, value, wanted, statement.span))
                    }
                    None => {
                        if !types::leaving_without_a_value_fits(table, self.returns) {
                            return Err(vec![refuse(
                                "unsupported-return",
                                statement.span,
                                "this `return` carries no value, and the declared return type admits none",
                            )]);
                        }
                        None
                    }
                };
                self.seal(function, Terminator::Return(returned));
                Ok(Flow::Returned)
            }
            AstStatement::BlockStatement(block) => {
                let standing = self.standing_declarations();
                let flow = self.lower_statements(table, function, &block.body)?;
                self.leave_declarations(standing);
                Ok(flow)
            }
            AstStatement::IfStatement(statement) => self.lower_if(table, function, statement),
            AstStatement::LabeledStatement(statement) => {
                self.lower_labelled(table, function, statement)
            }
            AstStatement::WhileStatement(statement) => {
                self.lower_while(table, function, statement, Vec::new())
            }
            AstStatement::ForStatement(statement) => {
                self.lower_for(table, function, statement, Vec::new())
            }
            AstStatement::DoWhileStatement(statement) => {
                self.lower_do_while(table, function, statement, Vec::new())
            }
            AstStatement::ForOfStatement(statement) => {
                self.lower_for_of(table, function, statement, Vec::new())
            }
            AstStatement::SwitchStatement(statement) => {
                self.lower_switch(table, function, statement, Vec::new())
            }
            AstStatement::BreakStatement(statement) => {
                self.lower_break(function, statement.label.as_ref(), statement.span)
            }
            AstStatement::ContinueStatement(statement) => {
                self.lower_continue(function, statement.label.as_ref(), statement.span)
            }
            other => Err(vec![refuse(
                "unsupported-statement",
                statement_span(other),
                "this statement is not lowered yet",
            )]),
        }
    }

    fn lower_effect(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        expression: &Expression<'_>,
    ) -> Result<(), Vec<Diagnostic>> {
        if let Expression::UpdateExpression(update) = expression {
            let op = if update.operator == UpdateOperator::Increment {
                BinaryOp::Add
            } else {
                BinaryOp::Subtract
            };
            return match &update.argument {
                SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
                    self.lower_name_write(table, function, id, Written::Stepped(op), update.span)
                }
                SimpleAssignmentTarget::StaticMemberExpression(member) => self.lower_field_write(
                    table,
                    function,
                    member,
                    Written::Stepped(op),
                    update.span,
                ),
                SimpleAssignmentTarget::ComputedMemberExpression(member) => self
                    .lower_element_write(
                        table,
                        function,
                        member,
                        Written::Stepped(op),
                        update.span,
                    ),
                _ => Err(vec![refuse(
                    "unsupported-statement",
                    update.span,
                    "only `++` and `--` on a name, a field or an element are lowered yet",
                )]),
            };
        }

        if let Some(call) = base_construction_call(expression) {
            return self.lower_base_construction(table, function, call);
        }

        if let Expression::ParenthesizedExpression(inner) = expression {
            return self.lower_effect(table, function, &inner.expression);
        }

        let Expression::AssignmentExpression(assignment) = expression else {
            self.lower_expression(table, function, expression)?;
            return Ok(());
        };
        let written = match assignment.operator {
            AssignmentOperator::Assign => Written::Value(&assignment.right),
            other => {
                let Some(op) = compound_op(other) else {
                    return Err(vec![refuse(
                        "unsupported-operator",
                        assignment.span,
                        format!("the assignment `{}` is not lowered yet", other.as_str()),
                    )]);
                };
                Written::Combined(op, &assignment.right)
            }
        };
        match &assignment.left {
            AssignmentTarget::AssignmentTargetIdentifier(id) => {
                self.lower_name_write(table, function, id, written, assignment.span)
            }
            AssignmentTarget::StaticMemberExpression(member) => {
                self.lower_field_write(table, function, member, written, assignment.span)
            }
            AssignmentTarget::ComputedMemberExpression(member) => {
                self.lower_element_write(table, function, member, written, assignment.span)
            }
            AssignmentTarget::ArrayAssignmentTarget(_)
            | AssignmentTarget::ObjectAssignmentTarget(_) => {
                let Written::Value(expression) = written else {
                    return Err(vec![refuse(
                        "unsupported-statement",
                        assignment.span,
                        "a compound assignment to a pattern is not lowered yet",
                    )]);
                };
                let source = self.lower_expression(table, function, expression)?;
                self.write_pattern(table, function, &assignment.left, source)
            }
            _ => Err(vec![refuse(
                "unsupported-statement",
                assignment.span,
                "only assignment to a name, a field, an element or a pattern is lowered yet",
            )]),
        }
    }

    fn lower_base_construction(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        call: &oxc_ast::ast::CallExpression<'_>,
    ) -> Result<(), Vec<Diagnostic>> {
        let Some(constructing) = self.constructing.as_ref() else {
            return Err(vec![refuse(
                "unsupported-call",
                call.span,
                "`super()` runs the base constructor, so it is lowered only inside a constructor",
            )]);
        };
        let Some(base) = constructing.base.as_ref() else {
            return Err(vec![refuse(
                "unsupported-call",
                call.span,
                "`super()` has no base class to construct here, because this class extends nothing",
            )]);
        };
        if base.standing {
            return Err(vec![refuse(
                "unsupported-call",
                call.span,
                "a constructor that calls `super()` more than once is not lowered yet",
            )]);
        }
        let receiver = constructing.receiver;
        let base_class = base.name.clone();
        let forms = base.forms.clone();

        let mut expressions = Vec::with_capacity(call.arguments.len());
        for argument in &call.arguments {
            let Some(expression) = argument.as_expression() else {
                return Err(vec![refuse(
                    "unsupported-expression",
                    call.span,
                    "an argument that is not an expression is not lowered yet",
                )]);
            };
            expressions.push(expression);
        }
        let written = self.written_type_arguments(table, call.type_arguments.as_deref())?;

        let mut values = Vec::with_capacity(expressions.len());
        for expression in &expressions {
            values.push(self.lower_expression(table, function, expression)?);
        }
        let held: Vec<TypeId> = values
            .iter()
            .map(|value| function.type_of(*value))
            .collect();
        let fitted = self.chosen_form(table, &forms, &held, &written, call.span)?;

        let mut arguments = Vec::with_capacity(values.len());
        for (position, value) in values.iter().enumerate() {
            arguments.push(self.coerced(
                function,
                table,
                *value,
                fitted.slots[position],
                expressions[position].span(),
            ));
        }
        self.statements.push(Statement::ConstructBase {
            base_class,
            object: receiver,
            arguments,
            span: span_of(call.span),
        });
        if let Some(constructing) = self.constructing.as_mut()
            && let Some(base) = constructing.base.as_mut()
        {
            base.standing = true;
        }
        self.forget_narrowing_a_call_could_falsify(function, call.span);
        Ok(())
    }

    fn write_pattern(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        target: &AssignmentTarget<'_>,
        source: ValueId,
    ) -> Result<(), Vec<Diagnostic>> {
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(id) => {
                self.store_name(table, function, id.name.as_str(), id.span, source, id.span)
            }
            AssignmentTarget::ArrayAssignmentTarget(array) => {
                self.write_array_pattern(table, function, array, source)
            }
            AssignmentTarget::ObjectAssignmentTarget(object) => {
                self.write_object_pattern(table, function, object, source)
            }
            AssignmentTarget::StaticMemberExpression(member) => {
                self.lower_field_write(table, function, member, Written::Held(source), member.span)
            }
            AssignmentTarget::ComputedMemberExpression(member) => self.lower_element_write(
                table,
                function,
                member,
                Written::Held(source),
                member.span,
            ),
            other => Err(vec![refuse(
                "unsupported-pattern",
                other.span(),
                "a pattern target that is neither a name, a field nor an element is not lowered yet",
            )]),
        }
    }

    fn write_maybe_defaulted(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        target: &oxc_ast::ast::AssignmentTargetMaybeDefault<'_>,
        source: ValueId,
    ) -> Result<(), Vec<Diagnostic>> {
        match target {
            oxc_ast::ast::AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(with) => {
                let value = self.defaulted(table, function, source, &with.init, with.span)?;
                self.write_pattern(table, function, &with.binding, value)
            }
            other => {
                let Some(plain) = other.as_assignment_target() else {
                    return Err(vec![refuse(
                        "unsupported-pattern",
                        other.span(),
                        "this pattern target is not lowered yet",
                    )]);
                };
                self.write_pattern(table, function, plain, source)
            }
        }
    }

    fn write_array_pattern(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        pattern: &oxc_ast::ast::ArrayAssignmentTarget<'_>,
        source: ValueId,
    ) -> Result<(), Vec<Diagnostic>> {
        for (position, element) in pattern.elements.iter().enumerate() {
            let Some(element) = element.as_ref() else {
                continue;
            };
            let at = element.span();
            let index = self.index_constant(table, function, position, at);
            let value = self.read_element_at(table, function, source, index, at)?;
            self.write_maybe_defaulted(table, function, element, value)?;
        }

        if let Some(rest) = &pattern.rest {
            let from = u32::try_from(pattern.elements.len()).expect("a pattern is small");
            let ty = types::rest_of(table, function.type_of(source), from)
                .map_err(|reason| vec![refuse("unsupported-pattern", rest.span, reason)])?;
            let value = self.assign(
                function,
                ty,
                Rvalue::Slice {
                    array: source,
                    from,
                },
                rest.span,
            );
            self.write_pattern(table, function, &rest.target, value)?;
        }
        Ok(())
    }

    fn write_object_pattern(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        pattern: &oxc_ast::ast::ObjectAssignmentTarget<'_>,
        source: ValueId,
    ) -> Result<(), Vec<Diagnostic>> {
        let mut taken = Vec::with_capacity(pattern.properties.len());
        for property in &pattern.properties {
            match property {
                oxc_ast::ast::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(
                    shorthand,
                ) => {
                    let name = shorthand.binding.name.to_string();
                    let read =
                        self.read_field_at(table, function, source, &name, shorthand.span)?;
                    let value = match &shorthand.init {
                        Some(fallback) => {
                            self.defaulted(table, function, read, fallback, shorthand.span)?
                        }
                        None => read,
                    };
                    self.store_name(
                        table,
                        function,
                        &name,
                        shorthand.binding.span,
                        value,
                        shorthand.span,
                    )?;
                    taken.push(name);
                }
                oxc_ast::ast::AssignmentTargetProperty::AssignmentTargetPropertyProperty(
                    renamed,
                ) => {
                    if renamed.computed {
                        return Err(vec![refuse(
                            "unsupported-pattern",
                            renamed.span,
                            "a computed key in an object pattern is not lowered yet",
                        )]);
                    }
                    let Some(name) = renamed.name.static_name() else {
                        return Err(vec![refuse(
                            "unsupported-pattern",
                            renamed.span,
                            "a key in an object pattern that is not a plain name is not lowered yet",
                        )]);
                    };
                    let name = name.to_string();
                    let value = self.read_field_at(table, function, source, &name, renamed.span)?;
                    taken.push(name);
                    self.write_maybe_defaulted(table, function, &renamed.binding, value)?;
                }
            }
        }

        if let Some(rest) = &pattern.rest {
            let ty = types::object_rest_of(table, function.type_of(source), &taken)
                .map_err(|reason| vec![refuse("unsupported-pattern", rest.span, reason)])?;
            let value = self.assign(
                function,
                ty,
                Rvalue::Rest {
                    object: source,
                    taken,
                },
                rest.span,
            );
            self.write_pattern(table, function, &rest.target, value)?;
        }
        Ok(())
    }

    fn lower_name_write(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        id: &oxc_ast::ast::IdentifierReference<'_>,
        written: Written<'_, '_>,
        span: oxc_span::Span,
    ) -> Result<(), Vec<Diagnostic>> {
        let held = self.bound(id.name.as_str(), id.span)?;
        let stored = match written {
            Written::Value(expression) => {
                self.lower_expression_expecting(table, function, expression, Some(held.declared))?
            }
            Written::Held(value) => value,
            Written::Combined(op, expression) => {
                let standing = held.value_or_refuse(id.name.as_str(), id.span)?;
                let value = self.lower_expression(table, function, expression)?;
                self.combine(table, function, op, standing, value, span)?
            }
            Written::Stepped(op) => {
                let standing = held.value_or_refuse(id.name.as_str(), id.span)?;
                let one = self.literal_one(table, function, span);
                self.combine(table, function, op, standing, one, span)?
            }
        };
        self.store_name(table, function, id.name.as_str(), id.span, stored, span)
    }

    fn store_name(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        name: &str,
        at: oxc_span::Span,
        value: ValueId,
        span: oxc_span::Span,
    ) -> Result<(), Vec<Diagnostic>> {
        if self.holds_a_capture_of(name) {
            return Err(vec![refuse(
                "unsupported-capture",
                span,
                format!(
                    "`{name}` is captured from the function around this closure, and writing to a captured name is not lowered yet"
                ),
            )]);
        }
        let held = self.bound(name, at)?;
        let stored = self.kept(table, function, value, held.declared, name, span)?;
        self.scope
            .store(name, Binding::written(stored, held.declared), span)
    }

    fn kept(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        value: ValueId,
        declared: TypeId,
        name: &str,
        span: oxc_span::Span,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        if !savitar_types::assignable(table, function.type_of(value), declared) {
            return Err(vec![refuse(
                "unsupported-assignment",
                span,
                format!("Savitar cannot show this value fits the type `{name}` holds"),
            )]);
        }
        Ok(value)
    }

    fn lower_field_write(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        member: &oxc_ast::ast::StaticMemberExpression<'_>,
        written: Written<'_, '_>,
        span: oxc_span::Span,
    ) -> Result<(), Vec<Diagnostic>> {
        let object = self.lower_expression(table, function, &member.object)?;
        let name = member.property.name.to_string();
        let writing = match self.constructing.as_ref() {
            Some(constructing) if constructing.receiver == object => {
                types::Writing::WhileConstructing {
                    inherited: &constructing.inherited,
                }
            }
            _ => types::Writing::Plainly,
        };
        let slots = self.field_slots(table, function.type_of(object), &name, member, writing)?;
        let agreed = match slots.as_slice() {
            [slot] => Some(*slot),
            _ => None,
        };
        let value = match written {
            Written::Value(expression) => {
                self.lower_expression_expecting(table, function, expression, agreed)?
            }
            Written::Held(value) => value,
            Written::Combined(op, expression) => {
                let held = self.read_field(table, function, object, &name, member)?;
                let value = self.lower_expression(table, function, expression)?;
                self.combine(table, function, op, held, value, span)?
            }
            Written::Stepped(op) => {
                let held = self.read_field(table, function, object, &name, member)?;
                let one = self.literal_one(table, function, span);
                self.combine(table, function, op, held, one, span)?
            }
        };
        let value = self.fitted_in_every(table, function, value, &slots, span)?;
        self.statements.push(Statement::WriteField {
            object,
            name,
            value,
            span: span_of(span),
        });
        Ok(())
    }

    fn lower_element_write(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        member: &oxc_ast::ast::ComputedMemberExpression<'_>,
        written: Written<'_, '_>,
        span: oxc_span::Span,
    ) -> Result<(), Vec<Diagnostic>> {
        let array = self.lower_expression(table, function, &member.object)?;
        let index = self.lower_expression(table, function, &member.expression)?;
        let slot = types::element_slot(table, function.type_of(array), function.type_of(index))
            .map_err(|reason| vec![refuse("unsupported-write", member.span, reason)])?;
        let value = match written {
            Written::Value(expression) => {
                self.lower_expression_expecting(table, function, expression, Some(slot))?
            }
            Written::Held(value) => value,
            Written::Combined(op, expression) => {
                let held = self.read_element(table, function, array, index, member)?;
                let value = self.lower_expression(table, function, expression)?;
                self.combine(table, function, op, held, value, span)?
            }
            Written::Stepped(op) => {
                let held = self.read_element(table, function, array, index, member)?;
                let one = self.literal_one(table, function, span);
                self.combine(table, function, op, held, one, span)?
            }
        };
        let value = self.fitted(table, function, value, slot, span)?;
        self.statements.push(Statement::WriteElement {
            array,
            index,
            value,
            span: span_of(span),
        });
        Ok(())
    }

    fn field_slots(
        &self,
        table: &mut TypeTable,
        object: TypeId,
        name: &str,
        member: &oxc_ast::ast::StaticMemberExpression<'_>,
        writing: types::Writing,
    ) -> Result<Vec<TypeId>, Vec<Diagnostic>> {
        let shapeless = match types::field_slots(table, object, name, writing) {
            Ok(slots) => return Ok(slots),
            Err(types::SlotRefusal::Declined(reason)) => {
                return Err(vec![refuse("unsupported-write", member.span, reason)]);
            }
            Err(types::SlotRefusal::Shapeless(reason)) => reason,
        };
        if types::is_union(table, object) {
            return Err(vec![refuse("unsupported-write", member.span, shapeless)]);
        }
        match self.answer_at(table, member.property.span) {
            Answer::Known(id) => Ok(vec![id]),
            Answer::Unsupported(detail) => {
                Err(vec![refuse("unsupported-type", member.span, detail)])
            }
            Answer::OutOfScope => Err(vec![refuse("unsupported-write", member.span, shapeless)]),
        }
    }

    fn fitted_in_every(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        value: ValueId,
        slots: &[TypeId],
        span: oxc_span::Span,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        if let [slot] = slots {
            return self.fitted(table, function, value, *slot, span);
        }
        for &slot in slots {
            if !savitar_types::assignable(table, function.type_of(value), slot) {
                return Err(vec![refuse(
                    "unsupported-assignment",
                    span,
                    "Savitar cannot show this value fits the field in every member of this union",
                )]);
            }
        }
        Ok(value)
    }

    fn read_field(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        object: ValueId,
        name: &str,
        member: &oxc_ast::ast::StaticMemberExpression<'_>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let ty = self.member_type(table, function.type_of(object), name, member)?;
        Ok(self.assign(
            function,
            ty,
            Rvalue::ReadField {
                object,
                name: name.to_string(),
            },
            member.span,
        ))
    }

    fn read_element(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        array: ValueId,
        index: ValueId,
        member: &oxc_ast::ast::ComputedMemberExpression<'_>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let ty = types::element_result(table, function.type_of(array), function.type_of(index))
            .map_err(|reason| vec![refuse("unsupported-read", member.span, reason)])?;
        Ok(self.assign(
            function,
            ty,
            Rvalue::ReadElement { array, index },
            member.span,
        ))
    }

    fn fitted(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        value: ValueId,
        slot: TypeId,
        span: oxc_span::Span,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        if !savitar_types::assignable(table, function.type_of(value), slot) {
            return Err(vec![refuse(
                "unsupported-assignment",
                span,
                "Savitar cannot show this value fits the type the target holds",
            )]);
        }
        Ok(self.coerced(function, table, value, slot, span))
    }

    fn written_type_arguments(
        &mut self,
        table: &mut TypeTable,
        written: Option<&oxc_ast::ast::TSTypeParameterInstantiation<'_>>,
    ) -> Result<Vec<TypeId>, Vec<Diagnostic>> {
        let Some(written) = written else {
            return Ok(Vec::new());
        };
        let mut resolved = Vec::with_capacity(written.params.len());
        for argument in &written.params {
            resolved.push(self.type_at(table, argument.span())?);
        }
        Ok(resolved)
    }

    fn chosen_form(
        &mut self,
        table: &mut TypeTable,
        forms: &[TypeId],
        held: &[TypeId],
        written: &[TypeId],
        span: oxc_span::Span,
    ) -> Result<types::Fitted, Vec<Diagnostic>> {
        let mut fitting: Vec<types::Fitted> = Vec::new();
        let mut refusals = Vec::new();
        for form in forms {
            match types::fits_call_with(table, *form, held, written) {
                Ok(fitted) => {
                    if !fitting
                        .iter()
                        .any(|held| types::fittings_agree(table, held, &fitted))
                    {
                        fitting.push(fitted);
                    }
                }
                Err(reason) => refusals.push(reason),
            }
        }
        if fitting.len() > 1 {
            return Err(vec![refuse(
                "unsupported-call",
                span,
                "this call fits more than one of the declared forms, and they disagree on the types, so Savitar does not choose between them",
            )]);
        }
        fitting.pop().ok_or_else(|| {
            vec![refuse(
                "unsupported-call",
                span,
                format!(
                    "no declared form of this call fits: {}",
                    refusals.join("; ")
                ),
            )]
        })
    }

    fn lower_new(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        new_expression: &oxc_ast::ast::NewExpression<'_>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let callee = self.lower_expression(table, function, &new_expression.callee)?;
        let forms = types::constructible_forms(table, function.type_of(callee))
            .map_err(|reason| vec![refuse("unsupported-call", new_expression.span, reason)])?;
        let written =
            self.written_type_arguments(table, new_expression.type_arguments.as_deref())?;

        let mut values = Vec::with_capacity(new_expression.arguments.len());
        let mut spans = Vec::with_capacity(new_expression.arguments.len());
        for argument in &new_expression.arguments {
            let Some(expression) = argument.as_expression() else {
                return Err(vec![refuse(
                    "unsupported-expression",
                    new_expression.span,
                    "an argument that is not an expression is not lowered yet",
                )]);
            };
            spans.push(expression.span());
            values.push(self.lower_expression(table, function, expression)?);
        }
        let held: Vec<TypeId> = values
            .iter()
            .map(|value| function.type_of(*value))
            .collect();

        let fitted = self.chosen_form(table, &forms, &held, &written, new_expression.span)?;
        let mut arguments = Vec::with_capacity(values.len());
        for (position, value) in values.iter().enumerate() {
            arguments.push(self.coerced(
                function,
                table,
                *value,
                fitted.slots[position],
                spans[position],
            ));
        }
        let made = self.assign(
            function,
            fitted.returns,
            Rvalue::New { callee, arguments },
            new_expression.span,
        );
        self.forget_narrowing_a_call_could_falsify(function, new_expression.span);
        Ok(made)
    }

    fn lower_call(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        call: &oxc_ast::ast::CallExpression<'_>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        if matches!(call.callee, Expression::Super(_)) {
            return Err(vec![refuse(
                "unsupported-call",
                call.span,
                "`super()` yields no value, so it is lowered only as a statement of its own",
            )]);
        }
        let callee = self.lower_expression(table, function, &call.callee)?;
        let signature = function.type_of(callee);
        let forms = types::callable_forms(table, signature)
            .map_err(|reason| vec![refuse("unsupported-call", call.span, reason)])?;

        let mut expressions = Vec::with_capacity(call.arguments.len());
        for argument in &call.arguments {
            let Argument::SpreadElement(_) = argument else {
                let Some(expression) = argument.as_expression() else {
                    return Err(vec![refuse(
                        "unsupported-expression",
                        call.span,
                        "an argument that is not an expression is not lowered yet",
                    )]);
                };
                expressions.push(expression);
                continue;
            };
            return Err(vec![refuse(
                "unsupported-expression",
                call.span,
                "a spread argument is not lowered yet",
            )]);
        }

        let written = self.written_type_arguments(table, call.type_arguments.as_deref())?;

        if let [only] = forms.as_slice()
            && !types::takes_type_arguments(table, *only)
            && written.is_empty()
        {
            let wanted = types::argument_types(table, *only, expressions.len())
                .map_err(|reason| vec![refuse("unsupported-call", call.span, reason)])?;
            let returns = types::call_result(table, *only)
                .map_err(|reason| vec![refuse("unsupported-call", call.span, reason)])?;
            let mut arguments = Vec::with_capacity(expressions.len());
            for (position, expression) in expressions.iter().enumerate() {
                let slot = wanted[position];
                let value =
                    self.lower_expression_expecting(table, function, expression, Some(slot))?;
                arguments.push(self.argued(table, function, value, slot, expression.span())?);
            }
            let answered = self.assign(
                function,
                returns,
                Rvalue::Call { callee, arguments },
                call.span,
            );
            self.forget_narrowing_a_call_could_falsify(function, call.span);
            return Ok(answered);
        }

        let mut values = Vec::with_capacity(expressions.len());
        for expression in &expressions {
            values.push(self.lower_expression(table, function, expression)?);
        }
        let held: Vec<TypeId> = values
            .iter()
            .map(|value| function.type_of(*value))
            .collect();

        let fitted = self.chosen_form(table, &forms, &held, &written, call.span)?;

        let mut arguments = Vec::with_capacity(values.len());
        for (position, value) in values.iter().enumerate() {
            arguments.push(self.coerced(
                function,
                table,
                *value,
                fitted.slots[position],
                expressions[position].span(),
            ));
        }
        let answered = self.assign(
            function,
            fitted.returns,
            Rvalue::Call { callee, arguments },
            call.span,
        );
        self.forget_narrowing_a_call_could_falsify(function, call.span);
        Ok(answered)
    }

    fn argued(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        value: ValueId,
        slot: TypeId,
        span: oxc_span::Span,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        if !savitar_types::assignable(table, function.type_of(value), slot) {
            return Err(vec![refuse(
                "unsupported-argument",
                span,
                "Savitar cannot show this argument fits the parameter it is passed to",
            )]);
        }
        Ok(self.coerced(function, table, value, slot, span))
    }

    fn literal_one(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        span: oxc_span::Span,
    ) -> ValueId {
        let ty = table.number_literal(1.0);
        self.assign(
            function,
            ty,
            Rvalue::Constant(Constant::Number(1.0f64.to_bits())),
            span,
        )
    }

    fn carried_names(
        &self,
        body: &AstStatement<'_>,
        stepping: &[&Expression<'_>],
        span: oxc_span::Span,
    ) -> Result<Vec<Carried>, Vec<Diagnostic>> {
        let mut found = assigned::assigned_names(std::slice::from_ref(body));
        for expression in stepping {
            found.extend(assigned::assigned_names_in(expression));
        }
        let mut carried = Vec::new();
        for name in found {
            let Some((key, binding)) = self.scope.innermost_entry(&name) else {
                continue;
            };
            let Some(entering) = binding.value else {
                return Err(vec![refuse(
                    "unsupported-statement",
                    span,
                    format!(
                        "`{name}` is written in this loop and was never written before it, so the header has no value to carry in"
                    ),
                )]);
            };
            carried.push(Carried {
                key,
                entering,
                declared: binding.declared,
            });
        }
        Ok(carried)
    }

    fn open_loop(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        carried: Vec<Carried>,
        entering: &[ValueId],
        repeating: Repeating,
        span: oxc_span::Span,
    ) -> Looping {
        let header = function.add_block(empty_block(span));
        let mut parameters = Vec::with_capacity(entering.len() + carried.len());
        let mut arguments = Vec::with_capacity(entering.len() + carried.len());
        for value in entering {
            parameters.push(function.fresh_value(function.type_of(*value)));
            arguments.push(*value);
        }
        for held in &carried {
            parameters.push(function.fresh_value(held.declared));
            arguments.push(self.coerced(function, table, held.entering, held.declared, span));
        }
        function.block_mut(header).parameters = parameters.clone();

        let held: Vec<TypeId> = parameters
            .iter()
            .map(|parameter| function.type_of(*parameter))
            .collect();
        let (repeat, landing) = match repeating {
            Repeating::AtHeader => (header, parameters.clone()),
            Repeating::ThroughLatch => {
                let latch = function.add_block(empty_block(span));
                let landing = held
                    .into_iter()
                    .map(|ty| function.fresh_value(ty))
                    .collect();
                (latch, landing)
            }
        };
        let exit = function.add_block(empty_block(span));

        self.seal(
            function,
            Terminator::Jump {
                target: header,
                arguments,
            },
        );
        self.current = header;
        self.statements.clear();
        for (held, parameter) in carried.iter().zip(parameters.iter().skip(entering.len())) {
            self.scope
                .put(&held.key, Binding::written(*parameter, held.declared));
        }
        Looping {
            header,
            repeat,
            exit,
            leading: parameters[..entering.len()].to_vec(),
            landing,
            carried,
            at_header: self.scope.clone(),
        }
    }

    fn leaving_edge(
        &mut self,
        function: &mut Function,
        looping: &Looping,
        leaving: BlockId,
    ) -> (BlockId, Scope) {
        let at_leaving = self.scope.clone();
        self.current = leaving;
        self.statements.clear();
        self.seal(
            function,
            Terminator::Jump {
                target: looping.exit,
                arguments: Vec::new(),
            },
        );
        (leaving, at_leaving)
    }

    fn lower_loop_body(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        looping: &Looping,
        body: &AstStatement<'_>,
        labels: Vec<String>,
    ) -> Result<Jumped, Vec<Diagnostic>> {
        self.enclosing.push(Breakable::around(
            labels,
            Target::Loop {
                exit: looping.exit,
                repeat: looping.repeat,
            },
        ));
        let flow = self.lower_statement(table, function, body)?;
        let frame = self
            .enclosing
            .pop()
            .expect("a loop pops the frame it pushed");
        Ok(Jumped {
            flow,
            breaks: frame.breaks,
            continues: frame.continues,
        })
    }

    fn repeat_edges(
        &mut self,
        function: &mut Function,
        looping: &Looping,
        flow: Flow,
        continues: Vec<(BlockId, Scope)>,
        span: oxc_span::Span,
    ) -> Result<(), Vec<Diagnostic>> {
        let mut edges = continues;
        if flow.continues() {
            self.seal(
                function,
                Terminator::Jump {
                    target: looping.repeat,
                    arguments: Vec::new(),
                },
            );
            edges.push((self.current, std::mem::take(&mut self.scope)));
        }
        if edges.is_empty() {
            return Ok(());
        }
        function.block_mut(looping.repeat).parameters = looping.landing.clone();
        for (site, scope) in &edges {
            every_change_is_carried(looping, scope, span)?;
            let mut arguments = looping.leading.clone();
            for held in &looping.carried {
                let leaving = carried_value(scope, held, span)?;
                arguments.push(self.appended(function, *site, leaving, held.declared, span));
            }
            function.block_mut(*site).terminator = Terminator::Jump {
                target: looping.repeat,
                arguments,
            };
        }
        Ok(())
    }

    fn stand_at_latch(&mut self, looping: &Looping) {
        self.current = looping.repeat;
        self.statements.clear();
        self.scope = looping.at_header.clone();
        for (held, parameter) in looping
            .carried
            .iter()
            .zip(looping.landing.iter().skip(looping.leading.len()))
        {
            self.scope
                .put(&held.key, Binding::written(*parameter, held.declared));
        }
    }

    fn close_loop(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        looping: &Looping,
        leaving: Option<(BlockId, Scope)>,
        breaks: Vec<(BlockId, Scope)>,
        span: oxc_span::Span,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let mut edges: Vec<(BlockId, Scope)> = leaving.into_iter().collect();
        edges.extend(breaks);
        if edges.is_empty() {
            self.scope = looping.at_header.clone();
            return Ok(Flow::Returned);
        }
        self.scope = self.joined(
            table,
            function,
            looping.exit,
            &looping.at_header,
            &edges,
            span,
        )?;
        self.current = looping.exit;
        self.statements.clear();
        Ok(Flow::Continues)
    }

    fn lower_for_of(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::ForOfStatement<'_>,
        labels: Vec<String>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        if statement.r#await {
            return Err(vec![refuse(
                "unsupported-statement",
                statement.span,
                "`for await` is not lowered yet",
            )]);
        }
        let ForStatementLeft::VariableDeclaration(declaration) = &statement.left else {
            return Err(vec![refuse(
                "unsupported-statement",
                statement.span,
                "a `for of` over an existing name is not lowered yet",
            )]);
        };
        let [declarator] = declaration.declarations.as_slice() else {
            return Err(vec![refuse(
                "unsupported-statement",
                statement.span,
                "a `for of` must bind exactly one name",
            )]);
        };
        let iterated = self.lower_expression(table, function, &statement.right)?;
        let element = match table.kind(function.type_of(iterated)).clone() {
            TypeKind::Array(element) | TypeKind::ReadonlyArray(element) => element,
            _ => {
                return Err(vec![refuse(
                    "unsupported-statement",
                    statement.span,
                    "only an array is walked by `for of` yet; another iterable needs the iterator protocol",
                )]);
            }
        };

        let number = table.intern(TypeKind::Number);
        let zero_type = table.number_literal(0.0);
        let zero = self.assign(
            function,
            zero_type,
            Rvalue::Constant(Constant::Number(0.0f64.to_bits())),
            statement.span,
        );
        let start = self.assign(function, number, Rvalue::Widen(zero), statement.span);

        let carried = self.carried_names(&statement.body, &[], statement.span)?;
        let looping = self.open_loop(
            table,
            function,
            carried,
            &[start],
            Repeating::ThroughLatch,
            statement.span,
        );
        let index = looping.leading[0];

        let length = self.assign(
            function,
            number,
            Rvalue::ReadField {
                object: iterated,
                name: "length".to_string(),
            },
            statement.span,
        );
        let boolean = table.intern(TypeKind::Boolean);
        let in_range = self.assign(
            function,
            boolean,
            Rvalue::Binary {
                op: BinaryOp::Less,
                left: index,
                right: length,
            },
            statement.span,
        );

        let body = function.add_block(empty_block(statement.span));
        let leaving = function.add_block(empty_block(statement.span));
        self.seal(
            function,
            Terminator::Branch {
                condition: in_range,
                then_block: body,
                then_arguments: Vec::new(),
                else_block: leaving,
                else_arguments: Vec::new(),
            },
        );
        let leaving = self.leaving_edge(function, &looping, leaving);

        self.current = body;
        self.statements.clear();
        let current = self.assign(
            function,
            element,
            Rvalue::ReadElement {
                array: iterated,
                index,
            },
            statement.span,
        );
        let standing = self.standing_declarations();
        self.bind_pattern(table, function, &declarator.id, current)?;

        let jumped = self.lower_loop_body(table, function, &looping, &statement.body, labels)?;
        let repeats = jumped.repeats();
        self.repeat_edges(
            function,
            &looping,
            jumped.flow,
            jumped.continues,
            statement.span,
        )?;
        if repeats {
            self.stand_at_latch(&looping);
            let one_type = table.number_literal(1.0);
            let one = self.assign(
                function,
                one_type,
                Rvalue::Constant(Constant::Number(1.0f64.to_bits())),
                statement.span,
            );
            let stepped = self.assign(
                function,
                number,
                Rvalue::Binary {
                    op: BinaryOp::Add,
                    left: looping.landing[0],
                    right: one,
                },
                statement.span,
            );
            let mut back = vec![stepped];
            for held in &looping.carried {
                let leaving = carried_value(&self.scope, held, statement.span)?;
                back.push(self.coerced(function, table, leaving, held.declared, statement.span));
            }
            self.seal(
                function,
                Terminator::Jump {
                    target: looping.header,
                    arguments: back,
                },
            );
        }

        let flow = self.close_loop(
            table,
            function,
            &looping,
            Some(leaving),
            jumped.breaks,
            statement.span,
        )?;
        self.leave_declarations(standing);
        Ok(flow)
    }

    fn lower_do_while(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::DoWhileStatement<'_>,
        labels: Vec<String>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let carried = self.carried_names(&statement.body, &[&statement.test], statement.span)?;
        let looping = self.open_loop(
            table,
            function,
            carried,
            &[],
            Repeating::ThroughLatch,
            statement.span,
        );

        let jumped = self.lower_loop_body(table, function, &looping, &statement.body, labels)?;
        let repeats = jumped.repeats();
        self.repeat_edges(
            function,
            &looping,
            jumped.flow,
            jumped.continues,
            statement.span,
        )?;

        let mut leaving = None;
        if repeats {
            self.stand_at_latch(&looping);
            let condition = self.lower_expression(table, function, &statement.test)?;
            every_change_is_carried(&looping, &self.scope, statement.span)?;
            let mut back = Vec::with_capacity(looping.carried.len());
            for held in &looping.carried {
                let leaving = carried_value(&self.scope, held, statement.span)?;
                back.push(self.coerced(function, table, leaving, held.declared, statement.span));
            }
            let block = function.add_block(empty_block(statement.span));
            self.seal(
                function,
                Terminator::Branch {
                    condition,
                    then_block: looping.header,
                    then_arguments: back,
                    else_block: block,
                    else_arguments: Vec::new(),
                },
            );
            leaving = Some(self.leaving_edge(function, &looping, block));
        }

        self.close_loop(
            table,
            function,
            &looping,
            leaving,
            jumped.breaks,
            statement.span,
        )
    }

    fn lower_for_init(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::ForStatement<'_>,
    ) -> Result<(), Vec<Diagnostic>> {
        match &statement.init {
            Some(ForStatementInit::VariableDeclaration(declaration)) => {
                let widening = declaration.kind != VariableDeclarationKind::Const;
                for declarator in &declaration.declarations {
                    self.lower_declarator(table, function, declarator, widening)?;
                }
                Ok(())
            }
            Some(init) => {
                let Some(expression) = init.as_expression() else {
                    return Err(vec![refuse(
                        "unsupported-statement",
                        statement.span,
                        "this `for` initialiser is not lowered yet",
                    )]);
                };
                self.lower_effect(table, function, expression)
            }
            None => Ok(()),
        }
    }

    fn lower_for(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::ForStatement<'_>,
        labels: Vec<String>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let standing = self.standing_declarations();
        self.lower_for_init(table, function, statement)?;

        let mut stepping = Vec::new();
        if let Some(test) = &statement.test {
            stepping.push(test);
        }
        if let Some(update) = &statement.update {
            stepping.push(update);
        }
        let carried = self.carried_names(&statement.body, &stepping, statement.span)?;
        let looping = self.open_loop(
            table,
            function,
            carried,
            &[],
            Repeating::ThroughLatch,
            statement.span,
        );

        let leaving = match &statement.test {
            Some(test) => {
                let condition = self.lower_expression(table, function, test)?;
                let body = function.add_block(empty_block(statement.span));
                let leaving = function.add_block(empty_block(statement.span));
                self.seal(
                    function,
                    Terminator::Branch {
                        condition,
                        then_block: body,
                        then_arguments: Vec::new(),
                        else_block: leaving,
                        else_arguments: Vec::new(),
                    },
                );
                let leaving = self.leaving_edge(function, &looping, leaving);
                self.current = body;
                self.statements.clear();
                Some(leaving)
            }
            None => None,
        };

        let jumped = self.lower_loop_body(table, function, &looping, &statement.body, labels)?;
        let repeats = jumped.repeats();
        self.repeat_edges(
            function,
            &looping,
            jumped.flow,
            jumped.continues,
            statement.span,
        )?;
        if repeats {
            self.stand_at_latch(&looping);
            if let Some(update) = &statement.update {
                self.lower_effect(table, function, update)?;
            }
            every_change_is_carried(&looping, &self.scope, statement.span)?;
            let mut back = Vec::with_capacity(looping.carried.len());
            for held in &looping.carried {
                let leaving = carried_value(&self.scope, held, statement.span)?;
                back.push(self.coerced(function, table, leaving, held.declared, statement.span));
            }
            self.seal(
                function,
                Terminator::Jump {
                    target: looping.header,
                    arguments: back,
                },
            );
        }

        let flow = self.close_loop(
            table,
            function,
            &looping,
            leaving,
            jumped.breaks,
            statement.span,
        )?;
        self.leave_declarations(standing);
        Ok(flow)
    }

    fn lower_while(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::WhileStatement<'_>,
        labels: Vec<String>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let carried = self.carried_names(&statement.body, &[], statement.span)?;
        let looping = self.open_loop(
            table,
            function,
            carried,
            &[],
            Repeating::AtHeader,
            statement.span,
        );

        let condition = self.lower_expression(table, function, &statement.test)?;
        let body = function.add_block(empty_block(statement.span));
        let leaving = function.add_block(empty_block(statement.span));
        self.seal(
            function,
            Terminator::Branch {
                condition,
                then_block: body,
                then_arguments: Vec::new(),
                else_block: leaving,
                else_arguments: Vec::new(),
            },
        );
        let leaving = self.leaving_edge(function, &looping, leaving);

        self.current = body;
        self.statements.clear();
        let jumped = self.lower_loop_body(table, function, &looping, &statement.body, labels)?;
        self.repeat_edges(
            function,
            &looping,
            jumped.flow,
            jumped.continues,
            statement.span,
        )?;
        self.close_loop(
            table,
            function,
            &looping,
            Some(leaving),
            jumped.breaks,
            statement.span,
        )
    }

    fn tested_fact(&self, test: &Expression<'_>) -> Option<Tested> {
        match test {
            Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot => {
                let inner = self.tested_fact(&unary.argument)?;
                Some(Tested {
                    equal: !inner.equal,
                    ..inner
                })
            }
            Expression::ParenthesizedExpression(inner) => self.tested_fact(&inner.expression),
            Expression::Identifier(_) => Some(Tested {
                name: self.name_in_scope(test)?,
                fact: Fact::Truth,
                equal: true,
            }),
            Expression::BinaryExpression(binary) => match binary.operator {
                BinaryOperator::StrictEquality | BinaryOperator::StrictInequality => {
                    let equal = binary.operator == BinaryOperator::StrictEquality;
                    self.fact_between(&binary.left, &binary.right, equal)
                        .or_else(|| self.fact_between(&binary.right, &binary.left, equal))
                }
                BinaryOperator::In => self.fact_about_a_field(&binary.left, &binary.right),
                _ => None,
            },
            _ => None,
        }
    }

    fn fact_between(
        &self,
        subject: &Expression<'_>,
        against: &Expression<'_>,
        equal: bool,
    ) -> Option<Tested> {
        if let (Some(name), Some(kind)) = (self.name_in_scope(subject), nullish_kind(against)) {
            return Some(Tested {
                name,
                fact: Fact::Nullish(kind),
                equal,
            });
        }
        if let (Some(name), Some(word)) = (self.name_behind_typeof(subject), typeof_word(against)) {
            return Some(Tested {
                name,
                fact: Fact::Primitive(word),
                equal,
            });
        }
        if let (Some((name, field)), Some(literal)) =
            (self.field_of_name_in_scope(subject), literal_kind(against))
        {
            return Some(Tested {
                name,
                fact: Fact::Discriminant {
                    field,
                    literals: vec![literal],
                },
                equal,
            });
        }
        None
    }

    fn fact_about_a_field(
        &self,
        named: &Expression<'_>,
        holder: &Expression<'_>,
    ) -> Option<Tested> {
        let Expression::StringLiteral(field) = named else {
            return None;
        };
        Some(Tested {
            name: self.name_in_scope(holder)?,
            fact: Fact::Declares(field.value.to_string()),
            equal: true,
        })
    }

    fn name_in_scope(&self, side: &Expression<'_>) -> Option<String> {
        match side {
            Expression::Identifier(id) if self.scope.holds(id.name.as_str()) => {
                Some(id.name.to_string())
            }
            _ => None,
        }
    }

    fn name_behind_typeof(&self, side: &Expression<'_>) -> Option<String> {
        let Expression::UnaryExpression(unary) = side else {
            return None;
        };
        if unary.operator != UnaryOperator::Typeof {
            return None;
        }
        self.name_in_scope(&unary.argument)
    }

    fn field_of_name_in_scope(&self, side: &Expression<'_>) -> Option<(String, String)> {
        let Expression::StaticMemberExpression(member) = side else {
            return None;
        };
        let name = self.name_in_scope(&member.object)?;
        Some((name, member.property.name.to_string()))
    }

    fn narrow_by_test(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        tested: Option<&Tested>,
        holds: bool,
        span: oxc_span::Span,
    ) {
        let Some(tested) = tested else {
            return;
        };
        let Some((narrowing, binding)) = self.scope.innermost_entry(&tested.name) else {
            return;
        };
        let Some(standing) = binding.value else {
            return;
        };
        let held = function.type_of(standing);
        let asserted = holds == tested.equal;
        let narrowed = match &tested.fact {
            Fact::Nullish(kind) => {
                if asserted {
                    types::holds_member(table, held, kind).then(|| table.intern(kind.clone()))
                } else {
                    types::without_member(table, held, kind)
                }
            }
            Fact::Primitive(word) => {
                if asserted {
                    types::with_only_typeof(table, held, word)
                } else {
                    types::without_typeof(table, held, word)
                }
            }
            Fact::Discriminant { field, literals } => {
                if asserted {
                    types::with_only_discriminant(table, held, field, literals)
                } else {
                    types::without_discriminant(table, held, field, literals)
                }
            }
            Fact::Among(literals) => {
                if asserted {
                    types::with_only_literals(table, held, literals)
                } else {
                    types::without_literals(table, held, literals)
                }
            }
            Fact::Truth => asserted
                .then(|| types::without_always_false(table, held))
                .flatten(),
            Fact::Declares(field) => {
                if asserted {
                    types::with_only_declaring(table, held, field)
                } else {
                    types::without_declaring(table, held, field)
                }
            }
        };
        let Some(narrowed) = narrowed else {
            return;
        };
        if narrowed == held {
            return;
        }
        let value = self.assign(function, narrowed, Rvalue::Narrow(standing), span);
        let by_a_field = binding.narrowed_by_a_field || tested.fact.is_about_a_field();
        self.scope.put(
            &narrowing,
            Binding::narrowed(value, binding.declared, by_a_field),
        );
    }

    fn forget_narrowing_a_call_could_falsify(
        &mut self,
        function: &mut Function,
        span: oxc_span::Span,
    ) {
        for (key, binding) in self.scope.entries() {
            if !binding.narrowed_by_a_field {
                continue;
            }
            let Some(standing) = binding.value else {
                continue;
            };
            let forgotten = if function.type_of(standing) == binding.declared {
                standing
            } else {
                self.assign(function, binding.declared, Rvalue::Widen(standing), span)
            };
            self.scope
                .put(&key, Binding::written(forgotten, binding.declared));
        }
    }

    fn switch_subject(&self, discriminant: &Expression<'_>) -> Option<Subject> {
        if let Some(name) = self.name_in_scope(discriminant) {
            return Some(Subject { name, field: None });
        }
        let (name, field) = self.field_of_name_in_scope(discriminant)?;
        Some(Subject {
            name,
            field: Some(field),
        })
    }

    fn narrow_in_case(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        subject: Option<&Subject>,
        entry: &CaseEntry,
        decided: &[TypeKind],
        span: oxc_span::Span,
    ) {
        let Some(subject) = subject else {
            return;
        };
        let (literals, equal) = match entry {
            CaseEntry::Matched(literals) => (literals.clone(), true),
            CaseEntry::Unmatched => (decided.to_vec(), false),
            CaseEntry::Undecided => return,
        };
        if literals.is_empty() {
            return;
        }
        let tested = Tested {
            name: subject.name.clone(),
            fact: subject.fact_about(literals),
            equal,
        };
        self.narrow_by_test(table, function, Some(&tested), true, span);
    }

    fn lower_if(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::IfStatement<'_>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let condition = self.lower_expression(table, function, &statement.test)?;
        let tested = self.tested_fact(&statement.test);
        let before = self.scope.clone();

        let then_entry = function.add_block(empty_block(statement.span));
        let else_entry = function.add_block(empty_block(statement.span));
        self.seal(
            function,
            Terminator::Branch {
                condition,
                then_block: then_entry,
                then_arguments: Vec::new(),
                else_block: else_entry,
                else_arguments: Vec::new(),
            },
        );

        let join = function.add_block(empty_block(statement.span));

        self.current = then_entry;
        self.statements.clear();
        self.scope = before.clone();
        self.narrow_by_test(table, function, tested.as_ref(), true, statement.span);
        let then_flow = self.lower_statement(table, function, &statement.consequent)?;
        let then_exit = self.current;
        if then_flow.continues() {
            self.seal(
                function,
                Terminator::Jump {
                    target: join,
                    arguments: Vec::new(),
                },
            );
        }
        let then_scope = std::mem::replace(&mut self.scope, before.clone());

        self.current = else_entry;
        self.statements.clear();
        self.narrow_by_test(table, function, tested.as_ref(), false, statement.span);
        let else_flow = match &statement.alternate {
            Some(alternate) => self.lower_statement(table, function, alternate)?,
            None => Flow::Continues,
        };
        let else_exit = self.current;
        if else_flow.continues() {
            self.seal(
                function,
                Terminator::Jump {
                    target: join,
                    arguments: Vec::new(),
                },
            );
        }
        let else_scope = std::mem::take(&mut self.scope);

        if !then_flow.continues() && !else_flow.continues() {
            self.scope = before;
            return Ok(jumped_out_as(then_flow, else_flow));
        }

        let mut parameters = Vec::new();
        let mut then_arguments = Vec::new();
        let mut else_arguments = Vec::new();
        let mut merged = before.clone();

        if then_flow.continues() && else_flow.continues() {
            for (key, held) in before.entries() {
                let declared = held.declared;
                let in_then = then_scope.held_at(&key, statement.span)?;
                let in_else = else_scope.held_at(&key, statement.span)?;
                let narrowed_by_a_field =
                    in_then.narrowed_by_a_field || in_else.narrowed_by_a_field;
                let from_then = in_then.value;
                let from_else = in_else.value;
                if from_then == from_else {
                    merged.put(
                        &key,
                        Binding {
                            value: from_then,
                            declared,
                            narrowed_by_a_field,
                        },
                    );
                    continue;
                }
                let (Some(from_then), Some(from_else)) = (from_then, from_else) else {
                    merged.put(&key, Binding::awaiting_a_write(declared));
                    continue;
                };
                let wanted = table.union(vec![
                    function.type_of(from_then),
                    function.type_of(from_else),
                ]);
                let carried_then =
                    self.appended(function, then_exit, from_then, wanted, statement.span);
                let carried_else =
                    self.appended(function, else_exit, from_else, wanted, statement.span);
                let parameter = function.fresh_value(wanted);
                parameters.push(parameter);
                then_arguments.push(carried_then);
                else_arguments.push(carried_else);
                merged.put(
                    &key,
                    Binding::narrowed(parameter, declared, narrowed_by_a_field),
                );
            }
        } else if then_flow.continues() {
            merged = then_scope;
        } else {
            merged = else_scope;
        }

        function.block_mut(join).parameters = parameters;
        if then_flow.continues() {
            function.block_mut(then_exit).terminator = Terminator::Jump {
                target: join,
                arguments: then_arguments,
            };
        }
        if else_flow.continues() {
            function.block_mut(else_exit).terminator = Terminator::Jump {
                target: join,
                arguments: else_arguments,
            };
        }

        self.scope = merged;
        self.current = join;
        self.statements.clear();
        Ok(Flow::Continues)
    }

    fn lower_labelled(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::LabeledStatement<'_>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let mut labels = vec![statement.label.name.to_string()];
        let mut body = &statement.body;
        while let AstStatement::LabeledStatement(inner) = body {
            labels.push(inner.label.name.to_string());
            body = &inner.body;
        }
        for (position, label) in labels.iter().enumerate() {
            let already_standing = labels[..position].contains(label)
                || self.enclosing.iter().any(|frame| frame.answers_to(label));
            if already_standing {
                return Err(vec![refuse(
                    "unsupported-statement",
                    statement.span,
                    format!("`{label}` labels a statement already inside one with that label"),
                )]);
            }
        }
        match body {
            AstStatement::WhileStatement(body) => self.lower_while(table, function, body, labels),
            AstStatement::ForStatement(body) => self.lower_for(table, function, body, labels),
            AstStatement::DoWhileStatement(body) => {
                self.lower_do_while(table, function, body, labels)
            }
            AstStatement::ForOfStatement(body) => self.lower_for_of(table, function, body, labels),
            AstStatement::SwitchStatement(body) => self.lower_switch(table, function, body, labels),
            body => self.lower_labelled_block(table, function, labels, body),
        }
    }

    fn lower_labelled_block(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        labels: Vec<String>,
        body: &AstStatement<'_>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let span = statement_span(body);
        let before = self.scope.clone();
        let join = function.add_block(empty_block(span));
        self.enclosing
            .push(Breakable::around(labels, Target::Labelled(join)));
        let flow = self.lower_statement(table, function, body)?;
        let frame = self
            .enclosing
            .pop()
            .expect("a labelled statement pops the frame it pushed");
        let mut edges = frame.breaks;
        if flow.continues() {
            let exit = self.current;
            self.seal(
                function,
                Terminator::Jump {
                    target: join,
                    arguments: Vec::new(),
                },
            );
            edges.push((exit, std::mem::take(&mut self.scope)));
        }
        if edges.is_empty() {
            self.scope = before;
            return Ok(flow);
        }
        self.scope = self.joined(table, function, join, &before, &edges, span)?;
        self.current = join;
        self.statements.clear();
        Ok(Flow::Continues)
    }

    fn jump_target(
        &self,
        label: Option<&oxc_ast::ast::LabelIdentifier<'_>>,
        repeating: bool,
        span: oxc_span::Span,
    ) -> Result<usize, Vec<Diagnostic>> {
        let word = if repeating { "continue" } else { "break" };
        let Some(label) = label else {
            let bare = self.enclosing.iter().rposition(|frame| {
                if repeating {
                    frame.target.repeat().is_some()
                } else {
                    frame.target.caught_by_a_bare_break()
                }
            });
            return bare.ok_or_else(|| {
                let around = if repeating {
                    "a `continue` with no loop around it is not lowered"
                } else {
                    "a `break` with no loop or `switch` around it is not lowered"
                };
                vec![refuse("unsupported-statement", span, around)]
            });
        };
        let name = label.name.as_str();
        let found = self
            .enclosing
            .iter()
            .rposition(|frame| frame.answers_to(name))
            .ok_or_else(|| {
                vec![refuse(
                    "unsupported-statement",
                    span,
                    format!("`{word} {name}` names no label on a statement around it"),
                )]
            })?;
        if repeating && self.enclosing[found].target.repeat().is_none() {
            return Err(vec![refuse(
                "unsupported-statement",
                span,
                format!("`continue {name}` names a label that is not on a loop"),
            )]);
        }
        Ok(found)
    }

    fn lower_break(
        &mut self,
        function: &mut Function,
        label: Option<&oxc_ast::ast::LabelIdentifier<'_>>,
        span: oxc_span::Span,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let found = self.jump_target(label, false, span)?;
        let target = self.enclosing[found].target.exit();
        self.seal(
            function,
            Terminator::Jump {
                target,
                arguments: Vec::new(),
            },
        );
        let site = (self.current, self.scope.clone());
        self.enclosing[found].breaks.push(site);
        Ok(Flow::Broke)
    }

    fn lower_continue(
        &mut self,
        function: &mut Function,
        label: Option<&oxc_ast::ast::LabelIdentifier<'_>>,
        span: oxc_span::Span,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let found = self.jump_target(label, true, span)?;
        let target = self.enclosing[found]
            .target
            .repeat()
            .expect("a `continue` only lands on a loop");
        self.seal(
            function,
            Terminator::Jump {
                target,
                arguments: Vec::new(),
            },
        );
        let site = (self.current, self.scope.clone());
        self.enclosing[found].continues.push(site);
        Ok(Flow::Continued)
    }

    fn lower_switch(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::SwitchStatement<'_>,
        labels: Vec<String>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let discriminant = self.lower_expression(table, function, &statement.discriminant)?;
        let before = self.scope.clone();

        let join = function.add_block(empty_block(statement.span));
        let bodies: Vec<BlockId> = statement
            .cases
            .iter()
            .map(|_| function.add_block(empty_block(statement.span)))
            .collect();
        let fallback = statement
            .cases
            .iter()
            .position(|case| case.test.is_none())
            .map(|position| bodies[position]);

        let mut label_types = Vec::with_capacity(statement.cases.len());
        let mut case_literals: Vec<Option<TypeKind>> = vec![None; statement.cases.len()];
        let mut arrival: Vec<Option<BlockId>> = vec![None; statement.cases.len()];
        for (position, case) in statement.cases.iter().enumerate() {
            let Some(test) = &case.test else {
                continue;
            };
            let label = self.lower_expression(table, function, test)?;
            label_types.push(function.type_of(label));
            case_literals[position] = types::label_literal(table, function.type_of(label));
            let matched = self.combine(
                table,
                function,
                BinaryOp::StrictEqual,
                discriminant,
                label,
                case.span,
            )?;
            let next = function.add_block(empty_block(statement.span));
            arrival[position] = Some(self.current);
            self.seal(
                function,
                Terminator::Branch {
                    condition: matched,
                    then_block: bodies[position],
                    then_arguments: Vec::new(),
                    else_block: next,
                    else_arguments: Vec::new(),
                },
            );
            self.current = next;
            self.statements.clear();
        }

        let mut edges: Vec<(BlockId, Scope)> = Vec::new();
        let unmatched = self.current;
        let exhaustive = fallback.is_none()
            && types::exhausted(table, function.type_of(discriminant), &label_types);
        if exhaustive {
            self.seal(function, Terminator::Unreachable(span_of(statement.span)));
        } else {
            self.seal(
                function,
                Terminator::Jump {
                    target: fallback.unwrap_or(join),
                    arguments: Vec::new(),
                },
            );
            if fallback.is_none() {
                edges.push((unmatched, before.clone()));
            } else if let Some(position) =
                statement.cases.iter().position(|case| case.test.is_none())
            {
                arrival[position] = Some(unmatched);
            }
        }

        let subject = self.switch_subject(&statement.discriminant);
        let decided: Vec<TypeKind> = case_literals.iter().flatten().cloned().collect();

        self.enclosing
            .push(Breakable::around(labels, Target::Switch(join)));
        let last = statement.cases.len().saturating_sub(1);
        let mut entries: Vec<CaseEntry> = Vec::with_capacity(statement.cases.len());
        let mut fell_through = false;
        let mut carrying: Option<(BlockId, Scope)> = None;
        for (position, case) in statement.cases.iter().enumerate() {
            let reached = match (&case.test, &case_literals[position]) {
                (None, _) => CaseEntry::Unmatched,
                (Some(_), Some(literal)) => CaseEntry::Matched(vec![literal.clone()]),
                (Some(_), None) => CaseEntry::Undecided,
            };
            let fallen = std::mem::take(&mut fell_through);
            let entry = match position.checked_sub(1) {
                Some(earlier) if fallen => reached.fallen_into_from(&entries[earlier]),
                _ => reached,
            };
            entries.push(entry);

            self.scope = match carrying.take() {
                Some(fell) => {
                    let Some(tested) = arrival[position] else {
                        return Err(vec![refuse(
                            "malformed-ir",
                            case.span,
                            "a `case` run into from the one before it has no matching edge of its own",
                        )]);
                    };
                    let stub = function.add_block(empty_block(statement.span));
                    retarget(function, tested, bodies[position], stub);
                    let arriving = vec![(stub, before.clone()), fell];
                    self.joined(
                        table,
                        function,
                        bodies[position],
                        &before,
                        &arriving,
                        case.span,
                    )?
                }
                None => before.clone(),
            };
            self.current = bodies[position];
            self.statements.clear();
            let standing = self.standing_declarations();
            self.narrow_in_case(
                table,
                function,
                subject.as_ref(),
                &entries[position],
                &decided,
                case.span,
            );
            let flow = self.lower_statements(table, function, &case.consequent)?;
            self.leave_declarations(standing);
            if !flow.continues() {
                continue;
            }
            if position == last {
                let exit = self.current;
                self.seal(
                    function,
                    Terminator::Jump {
                        target: join,
                        arguments: Vec::new(),
                    },
                );
                edges.push((exit, std::mem::take(&mut self.scope)));
                continue;
            }
            let exit = self.current;
            fell_through = true;
            if !case.consequent.is_empty() {
                carrying = Some((exit, self.scope.clone()));
            }
            self.seal(
                function,
                Terminator::Jump {
                    target: bodies[position + 1],
                    arguments: Vec::new(),
                },
            );
        }
        let frame = self
            .enclosing
            .pop()
            .expect("a `switch` pops the frame it pushed");
        edges.extend(frame.breaks);

        if edges.is_empty() {
            self.scope = before;
            return Ok(Flow::Returned);
        }

        self.scope = self.joined(table, function, join, &before, &edges, statement.span)?;
        self.current = join;
        self.statements.clear();
        Ok(Flow::Continues)
    }

    fn joined(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        join: BlockId,
        shape: &Scope,
        edges: &[(BlockId, Scope)],
        span: oxc_span::Span,
    ) -> Result<Scope, Vec<Diagnostic>> {
        let mut parameters = Vec::new();
        let mut passed: Vec<Vec<ValueId>> = vec![Vec::new(); edges.len()];
        let mut merged = shape.clone();
        for (key, held) in shape.entries() {
            let declared = held.declared;
            let on_edges = edges
                .iter()
                .map(|(_, scope)| scope.held_at(&key, span))
                .collect::<Result<Vec<Binding>, Vec<Diagnostic>>>()?;
            let Some(leading) = on_edges.first() else {
                break;
            };
            let narrowed_by_a_field = on_edges.iter().any(|edge| edge.narrowed_by_a_field);
            let first = leading.value;
            if on_edges.iter().all(|edge| edge.value == first) {
                merged.put(
                    &key,
                    Binding {
                        value: first,
                        declared,
                        narrowed_by_a_field,
                    },
                );
                continue;
            }
            let Some(standing) = on_edges
                .iter()
                .map(|edge| edge.value)
                .collect::<Option<Vec<ValueId>>>()
            else {
                merged.put(&key, Binding::awaiting_a_write(declared));
                continue;
            };
            let members: Vec<TypeId> = standing
                .iter()
                .map(|&value| function.type_of(value))
                .collect();
            let wanted = table.union(members);
            for (position, ((exit, _), &value)) in edges.iter().zip(standing.iter()).enumerate() {
                let carried = self.appended(function, *exit, value, wanted, span);
                passed[position].push(carried);
            }
            let parameter = function.fresh_value(wanted);
            parameters.push(parameter);
            merged.put(
                &key,
                Binding::narrowed(parameter, declared, narrowed_by_a_field),
            );
        }

        function.block_mut(join).parameters = parameters;
        for ((exit, _), arguments) in edges.iter().zip(passed) {
            function.block_mut(*exit).terminator = Terminator::Jump {
                target: join,
                arguments,
            };
        }
        Ok(merged)
    }

    fn appended(
        &mut self,
        function: &mut Function,
        exit: BlockId,
        value: ValueId,
        wanted: TypeId,
        span: oxc_span::Span,
    ) -> ValueId {
        if function.type_of(value) == wanted {
            return value;
        }
        let target = function.fresh_value(wanted);
        function.block_mut(exit).statements.push(Statement::Assign {
            target,
            value: Rvalue::Widen(value),
            span: span_of(span),
        });
        target
    }

    fn settled(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        value: ValueId,
        wanted: Option<TypeId>,
        span: oxc_span::Span,
    ) -> ValueId {
        match wanted {
            Some(wanted) if savitar_types::assignable(table, function.type_of(value), wanted) => {
                self.coerced(function, table, value, wanted, span)
            }
            _ => self.widened(table, function, value, span),
        }
    }

    fn widened(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        value: ValueId,
        span: oxc_span::Span,
    ) -> ValueId {
        let held = function.type_of(value);
        let wide = types::widen(table, held);
        if wide == held {
            return value;
        }
        self.assign(function, wide, Rvalue::Widen(value), span)
    }

    fn bound(&self, name: &str, span: oxc_span::Span) -> Result<Binding, Vec<Diagnostic>> {
        if let Some(binding) = self.scope.get(name) {
            return Ok(binding);
        }
        if self.reaches_a_name_further_out(name) {
            return Err(vec![refuse(
                "unsupported-capture",
                span,
                format!(
                    "`{name}` comes from the function around this closure, and writing to a captured name is not lowered yet"
                ),
            )]);
        }
        if let Some(held) = self.module_names.get(name) {
            let what = match held {
                ModuleName::BoundByTheInitializer => "bound by the initializer of this module",
                ModuleName::AHoistedFunction
                | ModuleName::AClassBoundWhereDeclared
                | ModuleName::DeclaredWithNoCodeBehindIt => "declared at the top of this module",
                ModuleName::Imported { .. } | ModuleName::AWholeModule { .. } => {
                    "imported from another module"
                }
            };
            return Err(vec![refuse(
                "unsupported-module-write",
                span,
                format!("`{name}` is {what}, and writing to it from here is not lowered yet"),
            )]);
        }
        Err(vec![refuse(
            "unsupported-reference",
            span,
            format!("`{name}` is not bound in this function"),
        )])
    }

    fn declare(&mut self, name: &str, binding: Binding) {
        let key = Key {
            name: name.to_string(),
            depth: self.shadowed.len(),
        };
        self.scope.put(&key, binding);
        self.shadowed.push(key);
    }

    fn standing_declarations(&self) -> usize {
        self.shadowed.len()
    }

    fn leave_declarations(&mut self, standing: usize) {
        for key in self.shadowed.split_off(standing).into_iter().rev() {
            self.scope.drop_one(&key);
        }
    }

    fn combine(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
        span: oxc_span::Span,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let ty = types::binary_result(table, op, function.type_of(left), function.type_of(right))
            .map_err(|reason| vec![refuse("unsupported-operator", span, reason)])?;
        Ok(self.assign(function, ty, Rvalue::Binary { op, left, right }, span))
    }

    fn lower_conditional(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        conditional: &oxc_ast::ast::ConditionalExpression<'_>,
        expected: Option<TypeId>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let condition = self.lower_expression(table, function, &conditional.test)?;
        let tested = self.tested_fact(&conditional.test);
        let before = self.scope.clone();
        let then_entry = function.add_block(empty_block(conditional.span));
        let else_entry = function.add_block(empty_block(conditional.span));
        let join = function.add_block(empty_block(conditional.span));
        self.seal(
            function,
            Terminator::Branch {
                condition,
                then_block: then_entry,
                then_arguments: Vec::new(),
                else_block: else_entry,
                else_arguments: Vec::new(),
            },
        );

        self.current = then_entry;
        self.statements.clear();
        self.narrow_by_test(table, function, tested.as_ref(), true, conditional.span);
        let then_value =
            self.lower_expression_expecting(table, function, &conditional.consequent, expected)?;
        let then_exit = self.current;
        let then_statements = std::mem::take(&mut self.statements);

        self.current = else_entry;
        self.statements.clear();
        self.scope = before.clone();
        self.narrow_by_test(table, function, tested.as_ref(), false, conditional.span);
        let else_value =
            self.lower_expression_expecting(table, function, &conditional.alternate, expected)?;
        let else_exit = self.current;
        let else_statements = std::mem::take(&mut self.statements);
        self.scope = before;

        let merged = table.union(vec![
            function.type_of(then_value),
            function.type_of(else_value),
        ]);

        self.current = then_exit;
        self.statements = then_statements;
        let then_carried = self.coerced(function, table, then_value, merged, conditional.span);
        self.seal(
            function,
            Terminator::Jump {
                target: join,
                arguments: vec![then_carried],
            },
        );

        self.current = else_exit;
        self.statements = else_statements;
        let else_carried = self.coerced(function, table, else_value, merged, conditional.span);
        self.seal(
            function,
            Terminator::Jump {
                target: join,
                arguments: vec![else_carried],
            },
        );

        let parameter = function.fresh_value(merged);
        function.block_mut(join).parameters = vec![parameter];
        self.current = join;
        self.statements.clear();
        Ok(parameter)
    }

    fn lower_template(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        template: &oxc_ast::ast::TemplateLiteral<'_>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let mut texts = Vec::with_capacity(template.quasis.len());
        for quasi in &template.quasis {
            let Some(cooked) = quasi.value.cooked.as_ref() else {
                return Err(vec![refuse(
                    "unsupported-expression",
                    quasi.span,
                    "a template part with an invalid escape is not lowered yet",
                )]);
            };
            texts.push(cooked.to_string());
        }

        if template.expressions.is_empty() {
            let text = texts.into_iter().next().unwrap_or_default();
            let ty = table.intern(TypeKind::StringLiteral(text.clone()));
            return Ok(self.assign(
                function,
                ty,
                Rvalue::Constant(Constant::String(text)),
                template.span,
            ));
        }

        let text = table.intern(TypeKind::String);
        let mut parts = Vec::with_capacity(texts.len() + template.expressions.len());
        for (index, expression) in template.expressions.iter().enumerate() {
            if let Some(quasi) = texts.get(index).filter(|quasi| !quasi.is_empty()) {
                let ty = table.intern(TypeKind::StringLiteral(quasi.clone()));
                parts.push(self.assign(
                    function,
                    ty,
                    Rvalue::Constant(Constant::String(quasi.clone())),
                    template.span,
                ));
            }
            let value = self.lower_expression(table, function, expression)?;
            let span = expression_span(expression);
            let converted = types::unary_result(table, UnaryOp::ToText, function.type_of(value))
                .map_err(|reason| vec![refuse("unsupported-expression", span, reason)])?;
            parts.push(self.assign(
                function,
                converted,
                Rvalue::Unary {
                    op: UnaryOp::ToText,
                    operand: value,
                },
                span,
            ));
        }
        if let Some(quasi) = texts.last().filter(|quasi| !quasi.is_empty()) {
            let ty = table.intern(TypeKind::StringLiteral(quasi.clone()));
            parts.push(self.assign(
                function,
                ty,
                Rvalue::Constant(Constant::String(quasi.clone())),
                template.span,
            ));
        }

        Ok(self.assign(function, text, Rvalue::Concat(parts), template.span))
    }

    fn lower_logical(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        logical: &oxc_ast::ast::LogicalExpression<'_>,
        expected: Option<TypeId>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let left = self.lower_expression(table, function, &logical.left)?;
        let short_circuit_fact = self.tested_fact(&logical.left);
        let before = self.scope.clone();
        let coalescing = logical.operator == LogicalOperator::Coalesce;
        let condition = if coalescing {
            let boolean = table.intern(TypeKind::Boolean);
            self.assign(
                function,
                boolean,
                Rvalue::Unary {
                    op: UnaryOp::IsNullish,
                    operand: left,
                },
                logical.span,
            )
        } else {
            left
        };

        let right_entry = function.add_block(empty_block(logical.span));
        let carry_entry = function.add_block(empty_block(logical.span));
        let join = function.add_block(empty_block(logical.span));
        let right_on_then = logical.operator != LogicalOperator::Or;
        let (then_block, else_block) = if right_on_then {
            (right_entry, carry_entry)
        } else {
            (carry_entry, right_entry)
        };
        self.seal(
            function,
            Terminator::Branch {
                condition,
                then_block,
                then_arguments: Vec::new(),
                else_block,
                else_arguments: Vec::new(),
            },
        );

        self.current = right_entry;
        self.statements.clear();
        if !coalescing {
            self.narrow_by_test(
                table,
                function,
                short_circuit_fact.as_ref(),
                right_on_then,
                logical.span,
            );
        }
        let right = self.lower_expression_expecting(table, function, &logical.right, expected)?;
        let right_exit = self.current;
        let right_statements = std::mem::take(&mut self.statements);

        let carried_type = if coalescing {
            types::without_nullish(table, function.type_of(left))
        } else {
            Some(function.type_of(left))
        };
        let merged = match carried_type {
            Some(kept) => table.union(vec![kept, function.type_of(right)]),
            None => function.type_of(right),
        };

        self.current = right_exit;
        self.statements = right_statements;
        let widened = self.coerced(function, table, right, merged, logical.span);
        self.seal(
            function,
            Terminator::Jump {
                target: join,
                arguments: vec![widened],
            },
        );

        self.current = carry_entry;
        self.statements.clear();
        match carried_type {
            Some(kept) => {
                let narrowed = if kept == function.type_of(left) {
                    left
                } else {
                    self.assign(function, kept, Rvalue::Narrow(left), logical.span)
                };
                let carried = self.coerced(function, table, narrowed, merged, logical.span);
                self.seal(
                    function,
                    Terminator::Jump {
                        target: join,
                        arguments: vec![carried],
                    },
                );
            }
            None => self.seal(function, Terminator::Unreachable(span_of(logical.span))),
        }

        let parameter = function.fresh_value(merged);
        function.block_mut(join).parameters = vec![parameter];
        self.current = join;
        self.statements.clear();
        self.scope = before;
        Ok(parameter)
    }

    fn coerced(
        &mut self,
        function: &mut Function,
        _table: &mut TypeTable,
        value: ValueId,
        wanted: TypeId,
        span: oxc_span::Span,
    ) -> ValueId {
        if function.type_of(value) == wanted {
            return value;
        }
        self.assign(function, wanted, Rvalue::Widen(value), span)
    }

    fn lower_declarator(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        declarator: &VariableDeclarator<'_>,
        widening: bool,
    ) -> Result<(), Vec<Diagnostic>> {
        let BindingPattern::BindingIdentifier(id) = &declarator.id else {
            return self.lower_destructuring(table, function, declarator);
        };
        let names_a_signature = declarator.type_annotation.is_some()
            || declarator.init.as_ref().is_some_and(is_a_closure);
        let declared = names_a_signature
            .then(|| self.type_at(table, id.span).ok())
            .flatten();
        let Some(init) = &declarator.init else {
            let Some(declared) = declared else {
                return Err(vec![refuse(
                    "unsupported-statement",
                    declarator.span,
                    "a declaration with neither an initialiser nor a type annotation gives the name no type to hold",
                )]);
            };
            self.declare(id.name.as_str(), Binding::awaiting_a_write(declared));
            return Ok(());
        };
        let value = self.lower_expression_expecting(table, function, init, declared)?;
        let value = match declared {
            Some(declared) => {
                if !savitar_types::assignable(table, function.type_of(value), declared) {
                    return Err(vec![refuse(
                        "unsupported-assignment",
                        declarator.span,
                        "Savitar cannot show this value fits the declared type",
                    )]);
                }
                self.coerced(function, table, value, declared, declarator.span)
            }
            None if widening => self.widened(table, function, value, declarator.span),
            None => value,
        };
        let held = function.type_of(value);
        self.declare(id.name.as_str(), Binding::written(value, held));
        Ok(())
    }

    fn lower_destructuring(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        declarator: &VariableDeclarator<'_>,
    ) -> Result<(), Vec<Diagnostic>> {
        let Some(init) = &declarator.init else {
            return Err(vec![refuse(
                "unsupported-statement",
                declarator.span,
                "a declaration without an initialiser is not lowered yet",
            )]);
        };
        let source = self.lower_expression(table, function, init)?;
        self.bind_pattern(table, function, &declarator.id, source)
    }

    fn bind_pattern(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        pattern: &BindingPattern<'_>,
        source: ValueId,
    ) -> Result<(), Vec<Diagnostic>> {
        match pattern {
            BindingPattern::BindingIdentifier(id) => {
                self.declare(
                    id.name.as_str(),
                    Binding::written(source, function.type_of(source)),
                );
                Ok(())
            }
            BindingPattern::ArrayPattern(array) => {
                self.bind_array_pattern(table, function, array, source)
            }
            BindingPattern::ObjectPattern(object) => {
                self.bind_object_pattern(table, function, object, source)
            }
            BindingPattern::AssignmentPattern(default) => {
                let value =
                    self.defaulted(table, function, source, &default.right, default.span)?;
                self.bind_pattern(table, function, &default.left, value)
            }
        }
    }

    fn defaulted(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        read: ValueId,
        fallback: &Expression<'_>,
        span: oxc_span::Span,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let held = function.type_of(read);
        if !types::may_be_undefined(table, held) {
            return Ok(read);
        }
        let Some(kept) = types::without_member(table, held, &TypeKind::Undefined) else {
            let given = self.lower_expression(table, function, fallback)?;
            return Ok(self.widened(table, function, given, span));
        };

        let missing = self.tests_undefined(table, function, read, span);
        let fallback_entry = function.add_block(empty_block(span));
        let kept_entry = function.add_block(empty_block(span));
        self.seal(
            function,
            Terminator::Branch {
                condition: missing,
                then_block: fallback_entry,
                then_arguments: Vec::new(),
                else_block: kept_entry,
                else_arguments: Vec::new(),
            },
        );
        let join = function.add_block(empty_block(span));

        let before = self.scope.clone();
        self.current = fallback_entry;
        self.statements.clear();
        let given = self.lower_expression_expecting(table, function, fallback, Some(kept))?;
        let given = self.settled(table, function, given, Some(kept), span);
        let fallback_exit = self.current;
        self.seal(
            function,
            Terminator::Jump {
                target: join,
                arguments: Vec::new(),
            },
        );
        if self.scope != before {
            return Err(vec![refuse(
                "unsupported-pattern",
                span,
                "a default in a pattern that writes to a name is not lowered yet",
            )]);
        }

        self.current = kept_entry;
        self.statements.clear();
        let narrowed = self.assign(function, kept, Rvalue::Narrow(read), span);
        let kept_exit = self.current;
        self.seal(
            function,
            Terminator::Jump {
                target: join,
                arguments: Vec::new(),
            },
        );

        let wanted = table.union(vec![function.type_of(given), kept]);
        let from_fallback = self.appended(function, fallback_exit, given, wanted, span);
        let from_kept = self.appended(function, kept_exit, narrowed, wanted, span);
        let bound = function.fresh_value(wanted);
        function.block_mut(join).parameters = vec![bound];
        function.block_mut(fallback_exit).terminator = Terminator::Jump {
            target: join,
            arguments: vec![from_fallback],
        };
        function.block_mut(kept_exit).terminator = Terminator::Jump {
            target: join,
            arguments: vec![from_kept],
        };

        self.current = join;
        self.statements.clear();
        Ok(bound)
    }

    fn tests_undefined(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        value: ValueId,
        span: oxc_span::Span,
    ) -> ValueId {
        let nothing = table.intern(TypeKind::Undefined);
        let nothing = self.assign(
            function,
            nothing,
            Rvalue::Constant(Constant::Undefined),
            span,
        );
        let boolean = table.intern(TypeKind::Boolean);
        self.assign(
            function,
            boolean,
            Rvalue::Binary {
                op: BinaryOp::StrictEqual,
                left: value,
                right: nothing,
            },
            span,
        )
    }

    fn bind_array_pattern(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        pattern: &oxc_ast::ast::ArrayPattern<'_>,
        source: ValueId,
    ) -> Result<(), Vec<Diagnostic>> {
        for (position, element) in pattern.elements.iter().enumerate() {
            let Some(element) = element.as_ref() else {
                continue;
            };
            let at = element.span();
            let index = self.index_constant(table, function, position, at);
            let value = self.read_element_at(table, function, source, index, at)?;
            self.bind_pattern(table, function, element, value)?;
        }

        if let Some(rest) = &pattern.rest {
            let BindingPattern::BindingIdentifier(id) = &rest.argument else {
                return Err(vec![refuse(
                    "unsupported-pattern",
                    pattern.span,
                    "a rest in an array pattern that binds anything but a plain name is not lowered yet",
                )]);
            };
            let from = u32::try_from(pattern.elements.len()).expect("a pattern is small");
            let ty = types::rest_of(table, function.type_of(source), from)
                .map_err(|reason| vec![refuse("unsupported-pattern", pattern.span, reason)])?;
            let value = self.assign(
                function,
                ty,
                Rvalue::Slice {
                    array: source,
                    from,
                },
                id.span,
            );
            self.declare(id.name.as_str(), Binding::written(value, ty));
        }
        Ok(())
    }

    fn bind_object_pattern(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        pattern: &oxc_ast::ast::ObjectPattern<'_>,
        source: ValueId,
    ) -> Result<(), Vec<Diagnostic>> {
        let mut taken = Vec::with_capacity(pattern.properties.len());
        for property in &pattern.properties {
            let name = self.pattern_key(property)?;
            let value = self.read_field_at(table, function, source, &name, property.span)?;
            taken.push(name);
            self.bind_pattern(table, function, &property.value, value)?;
        }

        if let Some(rest) = &pattern.rest {
            let BindingPattern::BindingIdentifier(id) = &rest.argument else {
                return Err(vec![refuse(
                    "unsupported-pattern",
                    rest.span,
                    "a rest in an object pattern that binds anything but a plain name is not lowered yet",
                )]);
            };
            let ty = types::object_rest_of(table, function.type_of(source), &taken)
                .map_err(|reason| vec![refuse("unsupported-pattern", rest.span, reason)])?;
            let value = self.assign(
                function,
                ty,
                Rvalue::Rest {
                    object: source,
                    taken,
                },
                id.span,
            );
            self.declare(id.name.as_str(), Binding::written(value, ty));
        }
        Ok(())
    }

    fn pattern_key(
        &self,
        property: &oxc_ast::ast::BindingProperty<'_>,
    ) -> Result<String, Vec<Diagnostic>> {
        if property.computed {
            return Err(vec![refuse(
                "unsupported-pattern",
                property.span,
                "a computed key in an object pattern is not lowered yet",
            )]);
        }
        property.key.static_name().map(Into::into).ok_or_else(|| {
            vec![refuse(
                "unsupported-pattern",
                property.span,
                "a key in an object pattern that is not a plain name is not lowered yet",
            )]
        })
    }

    fn read_field_at(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        object: ValueId,
        name: &str,
        span: oxc_span::Span,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let ty = types::field_result(table, function.type_of(object), name)
            .map_err(|reason| vec![refuse("unsupported-pattern", span, reason)])?;
        Ok(self.assign(
            function,
            ty,
            Rvalue::ReadField {
                object,
                name: name.to_string(),
            },
            span,
        ))
    }

    fn index_constant(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        position: usize,
        span: oxc_span::Span,
    ) -> ValueId {
        let at = position as f64;
        let ty = table.number_literal(at);
        self.assign(
            function,
            ty,
            Rvalue::Constant(Constant::Number(at.to_bits())),
            span,
        )
    }

    fn read_element_at(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        array: ValueId,
        index: ValueId,
        span: oxc_span::Span,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        let ty = types::element_result(table, function.type_of(array), function.type_of(index))
            .map_err(|reason| vec![refuse("unsupported-pattern", span, reason)])?;
        Ok(self.assign(function, ty, Rvalue::ReadElement { array, index }, span))
    }

    fn lower_expression(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        expression: &Expression<'_>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        self.lower_expression_expecting(table, function, expression, None)
    }

    fn lower_expression_expecting(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        expression: &Expression<'_>,
        expected: Option<TypeId>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
        match expression {
            Expression::NumericLiteral(literal) => {
                let ty = table.number_literal(literal.value);
                Ok(self.assign(
                    function,
                    ty,
                    Rvalue::Constant(Constant::Number(literal.value.to_bits())),
                    literal.span,
                ))
            }
            Expression::StringLiteral(literal) => {
                let ty = table.intern(TypeKind::StringLiteral(literal.value.to_string()));
                Ok(self.assign(
                    function,
                    ty,
                    Rvalue::Constant(Constant::String(literal.value.to_string())),
                    literal.span,
                ))
            }
            Expression::BooleanLiteral(literal) => {
                let ty = table.intern(TypeKind::BooleanLiteral(literal.value));
                Ok(self.assign(
                    function,
                    ty,
                    Rvalue::Constant(Constant::Boolean(literal.value)),
                    literal.span,
                ))
            }
            Expression::NullLiteral(literal) => {
                let ty = table.intern(TypeKind::Null);
                Ok(self.assign(function, ty, Rvalue::Constant(Constant::Null), literal.span))
            }
            Expression::Identifier(id) => {
                if let Some(known) = self.scope.get(id.name.as_str()) {
                    return known.value_or_refuse(id.name.as_str(), id.span);
                }
                if let Some(held) = self.captured(function, id.name.as_str(), id.span)? {
                    return Ok(held);
                }
                if id.name == "undefined" {
                    let ty = table.intern(TypeKind::Undefined);
                    return Ok(self.assign(
                        function,
                        ty,
                        Rvalue::Constant(Constant::Undefined),
                        id.span,
                    ));
                }
                let held = self.module_names.get(id.name.as_str()).cloned();
                let read = match held {
                    None => Rvalue::Global(id.name.to_string()),
                    Some(ModuleName::BoundByTheInitializer) if self.initialising_the_module => {
                        return Err(vec![refuse(
                            "unsupported-reference",
                            id.span,
                            format!(
                                "`{}` is read before the initializer of this module binds it",
                                id.name
                            ),
                        )]);
                    }
                    Some(ModuleName::AClassBoundWhereDeclared)
                        if self.initialising_the_module
                            && !self.declarations_bound_so_far.contains(id.name.as_str()) =>
                    {
                        return Err(vec![refuse(
                            "unsupported-reference",
                            id.span,
                            format!(
                                "`{}` names a class this module declares further down, and a class binds where it is declared and not before",
                                id.name
                            ),
                        )]);
                    }
                    Some(ModuleName::DeclaredWithNoCodeBehindIt) => {
                        return Err(vec![refuse(
                            "unsupported-ambient-name",
                            id.span,
                            format!(
                                "`{}` is declared without code behind it, so nothing this module emits binds the name",
                                id.name
                            ),
                        )]);
                    }
                    Some(
                        ModuleName::BoundByTheInitializer
                        | ModuleName::AHoistedFunction
                        | ModuleName::AClassBoundWhereDeclared,
                    ) => Rvalue::ModuleBinding(id.name.to_string()),
                    Some(ModuleName::AWholeModule { specifier }) => {
                        return Err(vec![refuse(
                            "unsupported-import",
                            id.span,
                            format!(
                                "`{specifier}` is imported whole, and the shape of a module object is not lowered yet"
                            ),
                        )]);
                    }
                    Some(ModuleName::Imported {
                        specifier,
                        exported,
                    }) => {
                        if is_a_package_specifier(&specifier) {
                            return Err(vec![refuse(
                                "unsupported-package-import",
                                id.span,
                                format!(
                                    "`{exported}` comes from the package `{specifier}`, and nothing a package declares is checked by Savitar"
                                ),
                            )]);
                        }
                        if self.initialising_the_module
                            && let Some(problem) =
                                unordered_read(self.standing, &specifier, &exported, id.span)
                        {
                            return Err(vec![problem]);
                        }
                        Rvalue::Imported {
                            specifier,
                            name: exported,
                        }
                    }
                };
                let ty = self.type_at(table, id.span)?;
                Ok(self.assign(function, ty, read, id.span))
            }
            Expression::ThisExpression(this) => {
                if let Some(held) = self.scope.get(THIS) {
                    return held.value_or_refuse(THIS, this.span);
                }
                if self
                    .capturing
                    .as_ref()
                    .is_some_and(|capturing| capturing.inherits_this)
                    && let Some(held) = self.captured(function, THIS, this.span)?
                {
                    return Ok(held);
                }
                Err(vec![refuse(
                    "unsupported-expression",
                    this.span,
                    "`this` is not bound here, because the shape it would take is not known",
                )])
            }
            Expression::ParenthesizedExpression(inner) => {
                self.lower_expression_expecting(table, function, &inner.expression, expected)
            }
            Expression::ConditionalExpression(conditional) => {
                self.lower_conditional(table, function, conditional, expected)
            }
            Expression::LogicalExpression(logical) => {
                self.lower_logical(table, function, logical, expected)
            }
            Expression::TemplateLiteral(template) => self.lower_template(table, function, template),
            Expression::UnaryExpression(unary) => {
                let Some(op) = unary_op(unary.operator) else {
                    return Err(vec![refuse(
                        "unsupported-operator",
                        unary.span,
                        format!(
                            "the unary operator `{}` is not lowered yet",
                            unary.operator.as_str()
                        ),
                    )]);
                };
                let operand = self.lower_expression(table, function, &unary.argument)?;
                let ty = types::unary_result(table, op, function.type_of(operand))
                    .map_err(|reason| vec![refuse("unsupported-operator", unary.span, reason)])?;
                Ok(self.assign(function, ty, Rvalue::Unary { op, operand }, unary.span))
            }
            Expression::BinaryExpression(binary) => {
                let Some(op) = binary_op(binary.operator) else {
                    return Err(vec![refuse(
                        "unsupported-operator",
                        binary.span,
                        format!(
                            "the operator `{}` is not lowered yet",
                            binary.operator.as_str()
                        ),
                    )]);
                };
                let left = self.lower_expression(table, function, &binary.left)?;
                let right = self.lower_expression(table, function, &binary.right)?;
                let ty = types::binary_result(
                    table,
                    op,
                    function.type_of(left),
                    function.type_of(right),
                )
                .map_err(|reason| vec![refuse("unsupported-operator", binary.span, reason)])?;
                Ok(self.assign(
                    function,
                    ty,
                    Rvalue::Binary { op, left, right },
                    binary.span,
                ))
            }
            Expression::StaticMemberExpression(member) => {
                let object = self.lower_expression(table, function, &member.object)?;
                let name = member.property.name.to_string();
                self.read_field(table, function, object, &name, member)
            }
            Expression::ComputedMemberExpression(member) => {
                let array = self.lower_expression(table, function, &member.object)?;
                let index = self.lower_expression(table, function, &member.expression)?;
                self.read_element(table, function, array, index, member)
            }
            Expression::ObjectExpression(object) => {
                let mut keys = Vec::with_capacity(object.properties.len());
                for property in &object.properties {
                    let ObjectPropertyKind::ObjectProperty(property) = property else {
                        return Err(vec![refuse(
                            "unsupported-expression",
                            object.span,
                            "a spread in an object literal is not lowered yet",
                        )]);
                    };
                    let Some(name) = property.key.static_name() else {
                        return Err(vec![refuse(
                            "unsupported-expression",
                            object.span,
                            "a computed key in an object literal is not lowered yet",
                        )]);
                    };
                    keys.push(types::Given {
                        name: name.to_string(),
                        literal: literal_type(table, &property.value),
                    });
                }
                let declared = expected.and_then(|id| types::contextual_fields(table, id, &keys));
                let mut values = Vec::with_capacity(object.properties.len());
                for (position, property) in object.properties.iter().enumerate() {
                    let ObjectPropertyKind::ObjectProperty(property) = property else {
                        unreachable!("every property was accepted above")
                    };
                    let name = &keys[position].name;
                    let wanted = declared.as_ref().and_then(|fields| {
                        fields
                            .iter()
                            .find(|field| field.name == *name)
                            .map(|field| field.type_id)
                    });
                    let value =
                        self.lower_expression_expecting(table, function, &property.value, wanted)?;
                    let value = self.settled(table, function, value, wanted, property.span);
                    values.push((name.clone(), value));
                }
                let fields = values
                    .iter()
                    .map(|(name, value)| Field {
                        name: name.clone(),
                        type_id: function.type_of(*value),
                        optional: false,
                        method: false,
                        readonly: false,
                    })
                    .collect();
                let ty = table.object(fields);
                Ok(self.assign(
                    function,
                    ty,
                    Rvalue::NewObject { fields: values },
                    object.span,
                ))
            }
            Expression::ArrayExpression(array) => {
                let positional = expected
                    .and_then(|id| types::contextual_elements(table, id, array.elements.len()));
                let declared = expected.and_then(|id| match table.kind(id) {
                    TypeKind::Array(element) | TypeKind::ReadonlyArray(element) => Some(*element),
                    _ => None,
                });
                let mut elements = Vec::with_capacity(array.elements.len());
                for (position, element) in array.elements.iter().enumerate() {
                    let Some(element) = element.as_expression() else {
                        return Err(vec![refuse(
                            "unsupported-expression",
                            array.span,
                            "a hole or spread in an array literal is not lowered yet",
                        )]);
                    };
                    let span = element.span();
                    let wanted = positional
                        .as_ref()
                        .map(|(wanted, _)| wanted[position])
                        .or(declared);
                    let value =
                        self.lower_expression_expecting(table, function, element, wanted)?;
                    elements.push(self.settled(table, function, value, wanted, span));
                }
                if let Some((_, readonly)) = positional {
                    let held: Vec<TypeId> =
                        elements.iter().map(|id| function.type_of(*id)).collect();
                    let ty = table.tuple(held, readonly);
                    return Ok(self.assign(
                        function,
                        ty,
                        Rvalue::NewArray { elements },
                        array.span,
                    ));
                }
                if elements.is_empty() {
                    let Some(declared) = expected else {
                        return Err(vec![refuse(
                            "unsupported-expression",
                            array.span,
                            "an empty array literal names no element type, and nothing declares it",
                        )]);
                    };
                    if !matches!(
                        table.kind(declared),
                        TypeKind::Array(_) | TypeKind::ReadonlyArray(_)
                    ) {
                        return Err(vec![refuse(
                            "unsupported-expression",
                            array.span,
                            "an empty array literal is declared as something other than an array",
                        )]);
                    }
                    return Ok(self.assign(
                        function,
                        declared,
                        Rvalue::NewArray {
                            elements: Vec::new(),
                        },
                        array.span,
                    ));
                }
                let members: Vec<TypeId> =
                    elements.iter().map(|id| function.type_of(*id)).collect();
                let element = table.union(members);
                let ty = table.intern(TypeKind::Array(element));
                Ok(self.assign(function, ty, Rvalue::NewArray { elements }, array.span))
            }
            Expression::ArrowFunctionExpression(arrow) => {
                let body = match &arrow.body {
                    oxc_ast::ast::ArrowFunctionBody::FunctionBody(statements) => {
                        Body::Statements(statements)
                    }
                    returned => {
                        let Some(returned) = returned.as_expression() else {
                            return Err(vec![refuse(
                                "unsupported-expression",
                                arrow.span,
                                "this arrow body is not lowered yet",
                            )]);
                        };
                        Body::OneExpression(returned)
                    }
                };
                self.lower_closure(table, function, Code::AnArrow(arrow), body, expected)
            }
            Expression::FunctionExpression(declaration) => {
                if declaration.id.is_some() {
                    return Err(vec![refuse(
                        "unsupported-expression",
                        declaration.span,
                        "a function expression with a name of its own is not lowered yet, because that name is bound inside its own body",
                    )]);
                }
                let Some(body) = declaration.body.as_ref() else {
                    return Err(vec![refuse(
                        "unsupported-expression",
                        declaration.span,
                        "a function expression with no body is not lowered yet",
                    )]);
                };
                self.lower_closure(
                    table,
                    function,
                    Code::AFunction(declaration),
                    Body::Statements(body),
                    expected,
                )
            }
            Expression::CallExpression(call) => self.lower_call(table, function, call),
            Expression::NewExpression(new_expression) => {
                self.lower_new(table, function, new_expression)
            }
            Expression::AwaitExpression(await_expression) => {
                let promised =
                    self.lower_expression(table, function, &await_expression.argument)?;
                let held = types::awaited(table, function.type_of(promised)).map_err(|_| {
                    vec![refuse(
                        "unsupported-expression",
                        await_expression.span,
                        "`await` of something that is not a promise is not lowered yet",
                    )]
                })?;
                let settled = self.assign(
                    function,
                    held,
                    Rvalue::Await(promised),
                    await_expression.span,
                );
                self.forget_narrowing_a_call_could_falsify(function, await_expression.span);
                Ok(settled)
            }
            other => {
                use oxc_span::GetSpan;
                Err(vec![refuse(
                    "unsupported-expression",
                    other.span(),
                    format!("`{}` is not lowered yet", expression_kind(other)),
                )])
            }
        }
    }
}

fn is_a_closure(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_) => true,
        Expression::ParenthesizedExpression(inner) => is_a_closure(&inner.expression),
        _ => false,
    }
}

fn nullish_kind(side: &Expression<'_>) -> Option<TypeKind> {
    match side {
        Expression::Identifier(id) if id.name == "undefined" => Some(TypeKind::Undefined),
        Expression::NullLiteral(_) => Some(TypeKind::Null),
        _ => None,
    }
}

fn typeof_word(side: &Expression<'_>) -> Option<String> {
    let Expression::StringLiteral(literal) = side else {
        return None;
    };
    types::narrowing_typeof_word(literal.value.as_str()).then(|| literal.value.to_string())
}

fn literal_kind(side: &Expression<'_>) -> Option<TypeKind> {
    match side {
        Expression::StringLiteral(literal) => {
            Some(TypeKind::StringLiteral(literal.value.to_string()))
        }
        Expression::NumericLiteral(literal) => {
            Some(TypeKind::NumberLiteral(literal.value.to_bits()))
        }
        Expression::BooleanLiteral(literal) => Some(TypeKind::BooleanLiteral(literal.value)),
        _ => None,
    }
}

fn literal_type(table: &mut TypeTable, expression: &Expression<'_>) -> Option<TypeId> {
    match expression {
        Expression::NumericLiteral(literal) => Some(table.number_literal(literal.value)),
        Expression::StringLiteral(literal) => {
            Some(table.intern(TypeKind::StringLiteral(literal.value.to_string())))
        }
        Expression::BooleanLiteral(literal) => {
            Some(table.intern(TypeKind::BooleanLiteral(literal.value)))
        }
        Expression::NullLiteral(_) => Some(table.intern(TypeKind::Null)),
        _ => None,
    }
}

fn binary_op(operator: BinaryOperator) -> Option<BinaryOp> {
    Some(match operator {
        BinaryOperator::Addition => BinaryOp::Add,
        BinaryOperator::Subtraction => BinaryOp::Subtract,
        BinaryOperator::Multiplication => BinaryOp::Multiply,
        BinaryOperator::Division => BinaryOp::Divide,
        BinaryOperator::Remainder => BinaryOp::Remainder,
        BinaryOperator::LessThan => BinaryOp::Less,
        BinaryOperator::LessEqualThan => BinaryOp::LessOrEqual,
        BinaryOperator::GreaterThan => BinaryOp::Greater,
        BinaryOperator::GreaterEqualThan => BinaryOp::GreaterOrEqual,
        BinaryOperator::StrictEquality => BinaryOp::StrictEqual,
        BinaryOperator::StrictInequality => BinaryOp::StrictNotEqual,
        BinaryOperator::In => BinaryOp::In,
        _ => return None,
    })
}

fn compound_op(operator: AssignmentOperator) -> Option<BinaryOp> {
    Some(match operator {
        AssignmentOperator::Addition => BinaryOp::Add,
        AssignmentOperator::Subtraction => BinaryOp::Subtract,
        AssignmentOperator::Multiplication => BinaryOp::Multiply,
        AssignmentOperator::Division => BinaryOp::Divide,
        AssignmentOperator::Remainder => BinaryOp::Remainder,
        _ => return None,
    })
}

fn unary_op(operator: UnaryOperator) -> Option<UnaryOp> {
    Some(match operator {
        UnaryOperator::UnaryNegation => UnaryOp::Negate,
        UnaryOperator::LogicalNot => UnaryOp::Not,
        UnaryOperator::Typeof => UnaryOp::TypeOf,
        _ => return None,
    })
}

fn expression_kind(expression: &Expression<'_>) -> &'static str {
    match expression {
        Expression::ConditionalExpression(_) => "a conditional expression",
        Expression::AwaitExpression(_) => "await",
        Expression::NewExpression(_) => "new",
        Expression::ArrowFunctionExpression(_) => "an arrow function",
        Expression::FunctionExpression(_) => "a function expression",
        Expression::AssignmentExpression(_) => "an assignment used as a value",
        Expression::UpdateExpression(_) => "an update used as a value",
        Expression::TSAsExpression(_) => "an `as` assertion",
        Expression::TSNonNullExpression(_) => "a `!` assertion",
        Expression::TSSatisfiesExpression(_) => "a `satisfies` expression",
        Expression::SequenceExpression(_) => "a comma expression",
        Expression::TaggedTemplateExpression(_) => "a tagged template",
        Expression::BigIntLiteral(_) => "a bigint literal",
        Expression::RegExpLiteral(_) => "a regular expression literal",
        Expression::ChainExpression(_) => "an optional chain",
        Expression::ClassExpression(_) => "a class expression",
        Expression::Super(_) => "super",
        Expression::YieldExpression(_) => "yield",
        _ => "this expression",
    }
}

#[cfg(test)]
mod scope_refusals {
    use super::{Binding, Carried, Key, Scope, carried_value};
    use savitar_diagnostics::Severity;
    use savitar_ir::Function;
    use savitar_types::{TypeId, TypeKind, TypeTable};

    struct Bench {
        function: Function,
        number: TypeId,
    }

    fn bench() -> Bench {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        Bench {
            function: Function::new("probe"),
            number,
        }
    }

    fn asked_at() -> oxc_span::Span {
        oxc_span::Span::new(17, 23)
    }

    fn key(name: &str, depth: usize) -> Key {
        Key {
            name: name.to_string(),
            depth,
        }
    }

    #[test]
    fn storing_a_name_no_declaration_stands_for_is_refused_with_a_span() {
        let mut bench = bench();
        let value = bench.function.fresh_value(bench.number);
        let mut scope = Scope::new();

        let refusal = scope
            .store("absent", Binding::written(value, bench.number), asked_at())
            .expect_err("a write with no declaration standing must be refused");

        assert_eq!(refusal.len(), 1);
        assert_eq!(refusal[0].rule, "malformed-ir");
        assert_eq!(refusal[0].severity, Severity::Unsupported);
        assert_eq!(refusal[0].span.start, 17);
        assert_eq!(refusal[0].span.end, 23);
        assert!(refusal[0].message.contains("absent"));
    }

    #[test]
    fn storing_a_name_a_declaration_stands_for_replaces_that_binding() {
        let mut bench = bench();
        let first = bench.function.fresh_value(bench.number);
        let second = bench.function.fresh_value(bench.number);
        let mut scope = Scope::new();
        scope.put(&key("count", 0), Binding::written(first, bench.number));

        scope
            .store("count", Binding::written(second, bench.number), asked_at())
            .expect("a write to a standing declaration is kept");

        assert_eq!(scope.get("count").and_then(|held| held.value), Some(second));
        assert_eq!(scope.entries().len(), 1);
    }

    #[test]
    fn storing_a_shadowed_name_replaces_only_the_innermost_declaration() {
        let mut bench = bench();
        let outer = bench.function.fresh_value(bench.number);
        let inner = bench.function.fresh_value(bench.number);
        let written = bench.function.fresh_value(bench.number);
        let mut scope = Scope::new();
        scope.put(&key("count", 0), Binding::written(outer, bench.number));
        scope.put(&key("count", 1), Binding::written(inner, bench.number));

        scope
            .store("count", Binding::written(written, bench.number), asked_at())
            .expect("a write to a standing declaration is kept");

        assert_eq!(
            scope.at(&key("count", 0)).and_then(|held| held.value),
            Some(outer)
        );
        assert_eq!(
            scope.at(&key("count", 1)).and_then(|held| held.value),
            Some(written)
        );
    }

    #[test]
    fn asking_what_a_key_holds_where_it_does_not_stand_is_refused_with_a_span() {
        let scope = Scope::new();

        let refusal = scope
            .held_at(&key("gone", 2), asked_at())
            .expect_err("a key that does not stand must be refused");

        assert_eq!(refusal.len(), 1);
        assert_eq!(refusal[0].rule, "malformed-ir");
        assert_eq!(refusal[0].severity, Severity::Unsupported);
        assert_eq!(refusal[0].span.start, 17);
        assert_eq!(refusal[0].span.end, 23);
        assert!(refusal[0].message.contains("gone"));
    }

    #[test]
    fn asking_what_a_key_holds_at_a_depth_that_does_not_stand_is_refused() {
        let mut bench = bench();
        let value = bench.function.fresh_value(bench.number);
        let mut scope = Scope::new();
        scope.put(&key("count", 0), Binding::written(value, bench.number));

        scope
            .held_at(&key("count", 1), asked_at())
            .expect_err("the same name at another depth is another declaration");
    }

    #[test]
    fn a_carried_name_that_holds_no_value_on_the_edge_back_is_refused_with_a_span() {
        let mut bench = bench();
        let entering = bench.function.fresh_value(bench.number);
        let carried = Carried {
            key: key("total", 0),
            entering,
            declared: bench.number,
        };
        let mut scope = Scope::new();
        scope.put(&carried.key, Binding::awaiting_a_write(bench.number));

        let refusal = carried_value(&scope, &carried, asked_at())
            .expect_err("a carried name with no value must be refused");

        assert_eq!(refusal.len(), 1);
        assert_eq!(refusal[0].rule, "malformed-ir");
        assert_eq!(refusal[0].severity, Severity::Unsupported);
        assert_eq!(refusal[0].span.start, 17);
        assert_eq!(refusal[0].span.end, 23);
        assert!(refusal[0].message.contains("total"));
    }

    #[test]
    fn a_carried_name_whose_declaration_left_the_edge_back_is_refused() {
        let mut bench = bench();
        let entering = bench.function.fresh_value(bench.number);
        let carried = Carried {
            key: key("total", 0),
            entering,
            declared: bench.number,
        };

        carried_value(&Scope::new(), &carried, asked_at())
            .expect_err("a carried name that no longer stands must be refused");
    }

    #[test]
    fn a_carried_name_that_holds_a_value_on_the_edge_back_answers_it() {
        let mut bench = bench();
        let entering = bench.function.fresh_value(bench.number);
        let leaving = bench.function.fresh_value(bench.number);
        let carried = Carried {
            key: key("total", 0),
            entering,
            declared: bench.number,
        };
        let mut scope = Scope::new();
        scope.put(&carried.key, Binding::written(leaving, bench.number));

        let answered = carried_value(&scope, &carried, asked_at())
            .expect("a carried name that holds a value answers it");

        assert_eq!(answered, leaving);
    }

    #[test]
    fn every_standing_declaration_has_an_entry_in_key_order() {
        let mut bench = bench();
        let value = bench.function.fresh_value(bench.number);
        let mut scope = Scope::new();
        scope.put(&key("count", 1), Binding::written(value, bench.number));
        scope.put(&key("count", 0), Binding::written(value, bench.number));
        scope.put(&key("budget", 3), Binding::written(value, bench.number));

        let entries = scope.entries();

        assert_eq!(
            entries
                .iter()
                .map(|(held, _)| held.clone())
                .collect::<Vec<_>>(),
            vec![key("budget", 3), key("count", 0), key("count", 1)]
        );
    }

    #[test]
    fn the_innermost_entry_names_the_declaration_a_put_goes_back_to() {
        let mut bench = bench();
        let outer = bench.function.fresh_value(bench.number);
        let inner = bench.function.fresh_value(bench.number);
        let narrowed = bench.function.fresh_value(bench.number);
        let mut scope = Scope::new();
        scope.put(&key("frame", 0), Binding::written(outer, bench.number));
        scope.put(&key("frame", 4), Binding::written(inner, bench.number));

        let (standing, held) = scope
            .innermost_entry("frame")
            .expect("a standing name answers its innermost declaration");
        assert_eq!(standing, key("frame", 4));
        assert_eq!(held.value, Some(inner));

        scope.put(&standing, Binding::narrowed(narrowed, held.declared, true));

        assert_eq!(scope.entries().len(), 2);
        assert_eq!(
            scope.at(&key("frame", 4)).and_then(|seen| seen.value),
            Some(narrowed)
        );
        assert_eq!(
            scope.at(&key("frame", 0)).and_then(|seen| seen.value),
            Some(outer)
        );
    }

    #[test]
    fn a_name_no_declaration_stands_for_has_no_innermost_entry() {
        assert!(Scope::new().innermost_entry("absent").is_none());
    }
}
