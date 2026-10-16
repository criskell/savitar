mod assigned;
mod types;

use std::collections::HashMap;

use oxc_allocator::Allocator;
use oxc_ast::ast::VariableDeclarationKind;
use oxc_ast::ast::{
    Argument, AssignmentTarget, BindingPattern, Declaration, Expression, ForStatementInit,
    ForStatementLeft, FunctionBody, ObjectPropertyKind, SimpleAssignmentTarget,
    Statement as AstStatement, VariableDeclarator,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use oxc_syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use savitar_diagnostics::{Diagnostic, SourceFile, Span};
use savitar_ir::{
    BinaryOp, Block, BlockId, Constant, Function, Rvalue, Statement, Terminator, UnaryOp, ValueId,
    verify,
};
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_types::{Field, TypeId, TypeKind, TypeTable};

pub use types::{binary_result, element_result, field_result, unary_result};

#[derive(Debug)]
pub struct LowerReport {
    pub functions: Vec<Function>,
    pub diagnostics: Vec<Diagnostic>,
}

type Scope = HashMap<String, ValueId>;

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
enum Breakable {
    Switch(BlockId),
    Loop { exit: BlockId, repeat: BlockId },
}

enum Repeating {
    AtHeader,
    ThroughLatch,
}

struct Looping {
    header: BlockId,
    repeat: BlockId,
    exit: BlockId,
    leading: Vec<ValueId>,
    landing: Vec<ValueId>,
    carried: Vec<String>,
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

fn every_change_is_carried(
    looping: &Looping,
    scope: &Scope,
    span: oxc_span::Span,
) -> Result<(), Vec<Diagnostic>> {
    for (name, value) in scope {
        if !looping.at_header.contains_key(name) {
            continue;
        }
        if looping.at_header.get(name) != Some(value) && !looping.carried.contains(name) {
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
    Discriminant {
        field: String,
        literals: Vec<TypeKind>,
    },
    Among(Vec<TypeKind>),
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

fn case_entries(
    statement: &oxc_ast::ast::SwitchStatement<'_>,
    literals: &[Option<TypeKind>],
) -> Vec<CaseEntry> {
    let mut entries: Vec<CaseEntry> = Vec::with_capacity(literals.len());
    for (position, case) in statement.cases.iter().enumerate() {
        let reached = match (&case.test, &literals[position]) {
            (None, _) => CaseEntry::Unmatched,
            (Some(_), Some(literal)) => CaseEntry::Matched(vec![literal.clone()]),
            (Some(_), None) => CaseEntry::Undecided,
        };
        let entry = match position.checked_sub(1) {
            Some(earlier) if statement.cases[earlier].consequent.is_empty() => {
                reached.fallen_into_from(&entries[earlier])
            }
            _ => reached,
        };
        entries.push(entry);
    }
    entries
}

enum Written<'target, 'source> {
    Value(&'target Expression<'source>),
    Combined(BinaryOp, &'target Expression<'source>),
    Stepped(BinaryOp),
}

struct Context<'a> {
    file: &'a SourceFile,
    provider: &'a dyn TypeProvider,
    returns: TypeId,
    scope: Scope,
    statements: Vec<Statement>,
    current: BlockId,
    enclosing: Vec<Breakable>,
    breaks: Vec<(BlockId, Scope)>,
    continues: Vec<(BlockId, Scope)>,
}

fn span_of(span: oxc_span::Span) -> Span {
    Span::new(span.start, span.end)
}

fn refuse(rule: &'static str, span: oxc_span::Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::unsupported(rule, span_of(span), message)
        .with_help("Savitar lowers only what it can type without guessing")
}

#[must_use]
pub fn lower(file: &SourceFile, provider: &dyn TypeProvider, table: &mut TypeTable) -> LowerReport {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(file.name()).unwrap_or_else(|_| SourceType::ts());
    let parsed = Parser::new(&allocator, file.text(), source_type).parse();
    if parsed.fatal_error {
        return LowerReport {
            functions: Vec::new(),
            diagnostics: Vec::new(),
        };
    }

    let mut functions = Vec::new();
    let mut diagnostics = Vec::new();

    for statement in &parsed.program.body {
        let declaration = match statement {
            AstStatement::FunctionDeclaration(declaration) => declaration,
            AstStatement::ExportDeclaration(export) => match &export.declaration {
                Declaration::FunctionDeclaration(declaration) => declaration,
                _ => continue,
            },
            _ => continue,
        };
        let Some(body) = &declaration.body else {
            continue;
        };
        let name = declaration
            .id
            .as_ref()
            .map_or_else(|| "anonymous".to_string(), |id| id.name.to_string());

        match lower_function(file, provider, table, &name, declaration, body) {
            Ok(function) => functions.push(function),
            Err(problems) => diagnostics.extend(problems),
        }
    }

    diagnostics.sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.rule));
    LowerReport {
        functions,
        diagnostics,
    }
}

fn empty_block() -> Block {
    Block {
        parameters: Vec::new(),
        statements: Vec::new(),
        terminator: Terminator::Unreachable,
    }
}

fn lower_function(
    file: &SourceFile,
    provider: &dyn TypeProvider,
    table: &mut TypeTable,
    name: &str,
    declaration: &oxc_ast::ast::Function<'_>,
    body: &FunctionBody<'_>,
) -> Result<Function, Vec<Diagnostic>> {
    if declaration.generator {
        return Err(vec![refuse(
            "unsupported-function",
            declaration.span,
            "a generator function is not lowered yet",
        )]);
    }
    let Some(id) = declaration.id.as_ref() else {
        return Err(vec![refuse(
            "unsupported-function",
            declaration.span,
            "a function with no name is not lowered yet, because its signature is asked for by name",
        )]);
    };

    let mut function = Function::new(name);
    let entry = function.add_block(empty_block());
    let mut context = Context {
        file,
        provider,
        returns: table.intern(TypeKind::Unknown),
        scope: Scope::new(),
        statements: Vec::new(),
        current: entry,
        enclosing: Vec::new(),
        breaks: Vec::new(),
        continues: Vec::new(),
    };

    let signature = context.type_at(table, id.span)?;
    let declared = types::declared_return(table, signature)
        .map_err(|reason| vec![refuse("unsupported-function", declaration.span, reason)])?;
    context.returns = if declaration.r#async {
        types::awaited(table, declared)
            .map_err(|reason| vec![refuse("unsupported-function", declaration.span, reason)])?
    } else {
        declared
    };

    for parameter in &declaration.params.items {
        let BindingPattern::BindingIdentifier(id) = &parameter.pattern else {
            return Err(vec![refuse(
                "unsupported-pattern",
                parameter.span,
                "a parameter that is not a plain name is not lowered yet",
            )]);
        };
        let ty = context.type_at(table, id.span)?;
        let value = function.declare_parameter(ty);
        context.scope.insert(id.name.to_string(), value);
    }

    let flow = context.lower_statements(table, &mut function, &body.statements)?;
    if flow == Flow::Continues {
        if !types::leaving_without_a_value_fits(table, context.returns) {
            return Err(vec![refuse(
                "unsupported-return",
                body.span,
                "this function can reach its end without returning a value the declared type admits",
            )]);
        }
        context.seal(&mut function, Terminator::Return(None));
    }

    let malformations = verify(&function);
    if !malformations.is_empty() {
        return Err(vec![refuse(
            "malformed-ir",
            body.span,
            format!("lowering produced invalid code: {malformations:?}"),
        )]);
    }
    Ok(function)
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
                let outer: Vec<String> = self.scope.keys().cloned().collect();
                let flow = self.lower_statements(table, function, &block.body)?;
                self.scope.retain(|name, _| outer.contains(name));
                Ok(flow)
            }
            AstStatement::IfStatement(statement) => self.lower_if(table, function, statement),
            AstStatement::WhileStatement(statement) => self.lower_while(table, function, statement),
            AstStatement::ForStatement(statement) => self.lower_for(table, function, statement),
            AstStatement::DoWhileStatement(statement) => {
                self.lower_do_while(table, function, statement)
            }
            AstStatement::ForOfStatement(statement) => {
                self.lower_for_of(table, function, statement)
            }
            AstStatement::SwitchStatement(statement) => {
                self.lower_switch(table, function, statement)
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
            _ => Err(vec![refuse(
                "unsupported-statement",
                assignment.span,
                "only assignment to a name, a field or an element is lowered yet",
            )]),
        }
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
            Written::Value(expression) => self.lower_expression(table, function, expression)?,
            Written::Combined(op, expression) => {
                let value = self.lower_expression(table, function, expression)?;
                self.combine(table, function, op, held, value, span)?
            }
            Written::Stepped(op) => {
                let one = self.literal_one(table, function, span);
                self.combine(table, function, op, held, one, span)?
            }
        };
        self.scope.insert(id.name.to_string(), stored);
        Ok(())
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
        let slot = self.field_slot(table, function.type_of(object), &name, member)?;
        let value = match written {
            Written::Value(expression) => {
                self.lower_expression_expecting(table, function, expression, Some(slot))?
            }
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
        let value = self.fitted(table, function, value, slot, span)?;
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

    fn field_slot(
        &self,
        table: &mut TypeTable,
        object: TypeId,
        name: &str,
        member: &oxc_ast::ast::StaticMemberExpression<'_>,
    ) -> Result<TypeId, Vec<Diagnostic>> {
        let shapeless = match types::field_slot(table, object, name) {
            Ok(slot) => return Ok(slot),
            Err(types::SlotRefusal::Declined(reason)) => {
                return Err(vec![refuse("unsupported-write", member.span, reason)]);
            }
            Err(types::SlotRefusal::Shapeless(reason)) => reason,
        };
        match self.answer_at(table, member.property.span) {
            Answer::Known(id) => Ok(id),
            Answer::Unsupported(detail) => {
                Err(vec![refuse("unsupported-type", member.span, detail)])
            }
            Answer::OutOfScope => Err(vec![refuse("unsupported-write", member.span, shapeless)]),
        }
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
        Ok(self.assign(
            function,
            fitted.returns,
            Rvalue::New { callee, arguments },
            new_expression.span,
        ))
    }

    fn lower_call(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        call: &oxc_ast::ast::CallExpression<'_>,
    ) -> Result<ValueId, Vec<Diagnostic>> {
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
            return Ok(self.assign(
                function,
                returns,
                Rvalue::Call { callee, arguments },
                call.span,
            ));
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
        Ok(self.assign(
            function,
            fitted.returns,
            Rvalue::Call { callee, arguments },
            call.span,
        ))
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

    fn carried_names(&self, body: &AstStatement<'_>, stepping: &[&Expression<'_>]) -> Vec<String> {
        let mut found = assigned::assigned_names(std::slice::from_ref(body));
        for expression in stepping {
            found.extend(assigned::assigned_names_in(expression));
        }
        found
            .into_iter()
            .filter(|name| self.scope.contains_key(name))
            .collect()
    }

    fn open_loop(
        &mut self,
        function: &mut Function,
        carried: Vec<String>,
        entering: &[ValueId],
        repeating: Repeating,
    ) -> Looping {
        let header = function.add_block(empty_block());
        let mut parameters = Vec::with_capacity(entering.len() + carried.len());
        let mut arguments = Vec::with_capacity(entering.len() + carried.len());
        for value in entering {
            parameters.push(function.fresh_value(function.type_of(*value)));
            arguments.push(*value);
        }
        for name in &carried {
            let incoming = self.scope[name];
            parameters.push(function.fresh_value(function.type_of(incoming)));
            arguments.push(incoming);
        }
        function.block_mut(header).parameters = parameters.clone();

        let held: Vec<TypeId> = parameters
            .iter()
            .map(|parameter| function.type_of(*parameter))
            .collect();
        let (repeat, landing) = match repeating {
            Repeating::AtHeader => (header, parameters.clone()),
            Repeating::ThroughLatch => {
                let latch = function.add_block(empty_block());
                let landing = held
                    .into_iter()
                    .map(|ty| function.fresh_value(ty))
                    .collect();
                (latch, landing)
            }
        };
        let exit = function.add_block(empty_block());

        self.seal(
            function,
            Terminator::Jump {
                target: header,
                arguments,
            },
        );
        self.current = header;
        self.statements.clear();
        for (name, parameter) in carried.iter().zip(parameters.iter().skip(entering.len())) {
            self.scope.insert(name.clone(), *parameter);
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
    ) -> Result<Jumped, Vec<Diagnostic>> {
        let outer_breaks = std::mem::take(&mut self.breaks);
        let outer_continues = std::mem::take(&mut self.continues);
        self.enclosing.push(Breakable::Loop {
            exit: looping.exit,
            repeat: looping.repeat,
        });
        let flow = self.lower_statement(table, function, body)?;
        self.enclosing.pop();
        let breaks = std::mem::replace(&mut self.breaks, outer_breaks);
        let continues = std::mem::replace(&mut self.continues, outer_continues);
        Ok(Jumped {
            flow,
            breaks,
            continues,
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
            for name in &looping.carried {
                arguments.push(scope[name]);
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
        for (name, parameter) in looping
            .carried
            .iter()
            .zip(looping.landing.iter().skip(looping.leading.len()))
        {
            self.scope.insert(name.clone(), *parameter);
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
    ) -> Flow {
        let mut edges: Vec<(BlockId, Scope)> = leaving.into_iter().collect();
        edges.extend(breaks);
        if edges.is_empty() {
            self.scope = looping.at_header.clone();
            return Flow::Returned;
        }
        self.scope = self.joined(
            table,
            function,
            looping.exit,
            &looping.at_header,
            &edges,
            span,
        );
        self.current = looping.exit;
        self.statements.clear();
        Flow::Continues
    }

    fn lower_for_of(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::ForOfStatement<'_>,
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
        let BindingPattern::BindingIdentifier(bound) = &declarator.id else {
            return Err(vec![refuse(
                "unsupported-pattern",
                statement.span,
                "a `for of` over a pattern is not lowered yet",
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

        let carried = self.carried_names(&statement.body, &[]);
        let looping = self.open_loop(function, carried, &[start], Repeating::ThroughLatch);
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

        let body = function.add_block(empty_block());
        let leaving = function.add_block(empty_block());
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
        self.scope.insert(bound.name.to_string(), current);

        let jumped = self.lower_loop_body(table, function, &looping, &statement.body)?;
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
            for name in &looping.carried {
                back.push(self.scope[name]);
            }
            self.seal(
                function,
                Terminator::Jump {
                    target: looping.header,
                    arguments: back,
                },
            );
        }

        Ok(self.close_loop(
            table,
            function,
            &looping,
            Some(leaving),
            jumped.breaks,
            statement.span,
        ))
    }

    fn lower_do_while(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::DoWhileStatement<'_>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let carried = self.carried_names(&statement.body, &[&statement.test]);
        let looping = self.open_loop(function, carried, &[], Repeating::ThroughLatch);

        let jumped = self.lower_loop_body(table, function, &looping, &statement.body)?;
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
            for name in &looping.carried {
                back.push(self.scope[name]);
            }
            let block = function.add_block(empty_block());
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

        Ok(self.close_loop(
            table,
            function,
            &looping,
            leaving,
            jumped.breaks,
            statement.span,
        ))
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
    ) -> Result<Flow, Vec<Diagnostic>> {
        let Some(test) = &statement.test else {
            return Err(vec![refuse(
                "unsupported-statement",
                statement.span,
                "a `for` with no test is not lowered yet, because its only way out is a `break`",
            )]);
        };

        let around: Vec<String> = self.scope.keys().cloned().collect();
        self.lower_for_init(table, function, statement)?;

        let mut stepping = vec![test];
        if let Some(update) = &statement.update {
            stepping.push(update);
        }
        let carried = self.carried_names(&statement.body, &stepping);
        let looping = self.open_loop(function, carried, &[], Repeating::ThroughLatch);

        let condition = self.lower_expression(table, function, test)?;
        let body = function.add_block(empty_block());
        let leaving = function.add_block(empty_block());
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
        let jumped = self.lower_loop_body(table, function, &looping, &statement.body)?;
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
            for name in &looping.carried {
                back.push(self.scope[name]);
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
        );
        self.scope.retain(|name, _| around.contains(name));
        Ok(flow)
    }

    fn lower_while(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::WhileStatement<'_>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let carried = self.carried_names(&statement.body, &[]);
        let looping = self.open_loop(function, carried, &[], Repeating::AtHeader);

        let condition = self.lower_expression(table, function, &statement.test)?;
        let body = function.add_block(empty_block());
        let leaving = function.add_block(empty_block());
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
        let jumped = self.lower_loop_body(table, function, &looping, &statement.body)?;
        self.repeat_edges(
            function,
            &looping,
            jumped.flow,
            jumped.continues,
            statement.span,
        )?;
        Ok(self.close_loop(
            table,
            function,
            &looping,
            Some(leaving),
            jumped.breaks,
            statement.span,
        ))
    }

    fn tested_fact(&self, test: &Expression<'_>) -> Option<Tested> {
        let Expression::BinaryExpression(binary) = test else {
            return None;
        };
        let equal = match binary.operator {
            BinaryOperator::StrictEquality => true,
            BinaryOperator::StrictInequality => false,
            _ => return None,
        };
        self.fact_between(&binary.left, &binary.right, equal)
            .or_else(|| self.fact_between(&binary.right, &binary.left, equal))
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

    fn name_in_scope(&self, side: &Expression<'_>) -> Option<String> {
        match side {
            Expression::Identifier(id) if self.scope.contains_key(id.name.as_str()) => {
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
        let Some(&value) = self.scope.get(&tested.name) else {
            return;
        };
        let held = function.type_of(value);
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
        };
        let Some(narrowed) = narrowed else {
            return;
        };
        if narrowed == held {
            return;
        }
        let value = self.assign(function, narrowed, Rvalue::Narrow(value), span);
        self.scope.insert(tested.name.clone(), value);
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

        let then_entry = function.add_block(empty_block());
        let else_entry = function.add_block(empty_block());
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

        let join = function.add_block(empty_block());

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
            let mut names: Vec<&String> = before.keys().collect();
            names.sort();
            for name in names {
                let from_then = then_scope[name];
                let from_else = else_scope[name];
                if from_then == from_else {
                    merged.insert(name.clone(), from_then);
                    continue;
                }
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
                merged.insert(name.clone(), parameter);
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

    fn lower_break(
        &mut self,
        function: &mut Function,
        label: Option<&oxc_ast::ast::LabelIdentifier<'_>>,
        span: oxc_span::Span,
    ) -> Result<Flow, Vec<Diagnostic>> {
        if label.is_some() {
            return Err(vec![refuse(
                "unsupported-statement",
                span,
                "a labelled `break` is not lowered yet",
            )]);
        }
        match self.enclosing.last() {
            Some(Breakable::Switch(join)) => {
                let target = *join;
                self.seal(
                    function,
                    Terminator::Jump {
                        target,
                        arguments: Vec::new(),
                    },
                );
                self.breaks.push((self.current, self.scope.clone()));
                Ok(Flow::Broke)
            }
            Some(Breakable::Loop { exit, .. }) => {
                let target = *exit;
                self.seal(
                    function,
                    Terminator::Jump {
                        target,
                        arguments: Vec::new(),
                    },
                );
                self.breaks.push((self.current, self.scope.clone()));
                Ok(Flow::Broke)
            }
            None => Err(vec![refuse(
                "unsupported-statement",
                span,
                "a `break` with no loop or `switch` around it is not lowered",
            )]),
        }
    }

    fn lower_continue(
        &mut self,
        function: &mut Function,
        label: Option<&oxc_ast::ast::LabelIdentifier<'_>>,
        span: oxc_span::Span,
    ) -> Result<Flow, Vec<Diagnostic>> {
        if label.is_some() {
            return Err(vec![refuse(
                "unsupported-statement",
                span,
                "a labelled `continue` is not lowered yet",
            )]);
        }
        let innermost = self
            .enclosing
            .iter()
            .rev()
            .find_map(|breakable| match breakable {
                Breakable::Loop { repeat, .. } => Some(*repeat),
                Breakable::Switch(_) => None,
            });
        let Some(target) = innermost else {
            return Err(vec![refuse(
                "unsupported-statement",
                span,
                "a `continue` with no loop around it is not lowered",
            )]);
        };
        self.seal(
            function,
            Terminator::Jump {
                target,
                arguments: Vec::new(),
            },
        );
        self.continues.push((self.current, self.scope.clone()));
        Ok(Flow::Continued)
    }

    fn lower_switch(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::SwitchStatement<'_>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let discriminant = self.lower_expression(table, function, &statement.discriminant)?;
        let before = self.scope.clone();

        let join = function.add_block(empty_block());
        let bodies: Vec<BlockId> = statement
            .cases
            .iter()
            .map(|_| function.add_block(empty_block()))
            .collect();
        let fallback = statement
            .cases
            .iter()
            .position(|case| case.test.is_none())
            .map(|position| bodies[position]);

        let mut labels = Vec::with_capacity(statement.cases.len());
        let mut case_literals: Vec<Option<TypeKind>> = vec![None; statement.cases.len()];
        for (position, case) in statement.cases.iter().enumerate() {
            let Some(test) = &case.test else {
                continue;
            };
            let label = self.lower_expression(table, function, test)?;
            labels.push(function.type_of(label));
            case_literals[position] = types::label_literal(table, function.type_of(label));
            let matched = self.combine(
                table,
                function,
                BinaryOp::StrictEqual,
                discriminant,
                label,
                case.span,
            )?;
            let next = function.add_block(empty_block());
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
        let exhaustive =
            fallback.is_none() && types::exhausted(table, function.type_of(discriminant), &labels);
        if exhaustive {
            self.seal(function, Terminator::Unreachable);
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
            }
        }

        let subject = self.switch_subject(&statement.discriminant);
        let entries = case_entries(statement, &case_literals);
        let decided: Vec<TypeKind> = case_literals.iter().flatten().cloned().collect();

        let outer_breaks = std::mem::take(&mut self.breaks);
        self.enclosing.push(Breakable::Switch(join));
        let last = statement.cases.len().saturating_sub(1);
        for (position, case) in statement.cases.iter().enumerate() {
            self.current = bodies[position];
            self.statements.clear();
            self.scope = before.clone();
            self.narrow_in_case(
                table,
                function,
                subject.as_ref(),
                &entries[position],
                &decided,
                case.span,
            );
            let flow = self.lower_statements(table, function, &case.consequent)?;
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
            if !case.consequent.is_empty() {
                return Err(vec![refuse(
                    "unsupported-statement",
                    case.span,
                    "a `case` that runs into the next one is not lowered yet; end it with `break` or `return`",
                )]);
            }
            self.seal(
                function,
                Terminator::Jump {
                    target: bodies[position + 1],
                    arguments: Vec::new(),
                },
            );
        }
        self.enclosing.pop();
        edges.extend(std::mem::replace(&mut self.breaks, outer_breaks));

        if edges.is_empty() {
            self.scope = before;
            return Ok(Flow::Returned);
        }

        self.scope = self.joined(table, function, join, &before, &edges, statement.span);
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
    ) -> Scope {
        let mut names: Vec<&String> = shape.keys().collect();
        names.sort();
        let mut parameters = Vec::new();
        let mut passed: Vec<Vec<ValueId>> = vec![Vec::new(); edges.len()];
        let mut merged = shape.clone();
        for name in names {
            let first = edges[0].1[name];
            if edges.iter().all(|(_, scope)| scope[name] == first) {
                merged.insert(name.clone(), first);
                continue;
            }
            let members: Vec<TypeId> = edges
                .iter()
                .map(|(_, scope)| function.type_of(scope[name]))
                .collect();
            let wanted = table.union(members);
            for (position, (exit, scope)) in edges.iter().enumerate() {
                let carried = self.appended(function, *exit, scope[name], wanted, span);
                passed[position].push(carried);
            }
            let parameter = function.fresh_value(wanted);
            parameters.push(parameter);
            merged.insert(name.clone(), parameter);
        }

        function.block_mut(join).parameters = parameters;
        for ((exit, _), arguments) in edges.iter().zip(passed) {
            function.block_mut(*exit).terminator = Terminator::Jump {
                target: join,
                arguments,
            };
        }
        merged
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

    fn bound(&self, name: &str, span: oxc_span::Span) -> Result<ValueId, Vec<Diagnostic>> {
        self.scope.get(name).copied().ok_or_else(|| {
            vec![refuse(
                "unsupported-reference",
                span,
                format!("`{name}` is not bound in this function"),
            )]
        })
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
        let then_entry = function.add_block(empty_block());
        let else_entry = function.add_block(empty_block());
        let join = function.add_block(empty_block());
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

        let right_entry = function.add_block(empty_block());
        let carry_entry = function.add_block(empty_block());
        let join = function.add_block(empty_block());
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
            None => self.seal(function, Terminator::Unreachable),
        }

        let parameter = function.fresh_value(merged);
        function.block_mut(join).parameters = vec![parameter];
        self.current = join;
        self.statements.clear();
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
        if let BindingPattern::ArrayPattern(pattern) = &declarator.id {
            return self.lower_array_pattern(table, function, declarator, pattern);
        }
        let BindingPattern::BindingIdentifier(id) = &declarator.id else {
            return Err(vec![refuse(
                "unsupported-pattern",
                declarator.span,
                "a binding that is not a plain name is not lowered yet",
            )]);
        };
        let Some(init) = &declarator.init else {
            return Err(vec![refuse(
                "unsupported-statement",
                declarator.span,
                "a declaration without an initialiser is not lowered yet",
            )]);
        };
        let declared = declarator
            .type_annotation
            .as_ref()
            .and_then(|_| self.type_at(table, id.span).ok());
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
        self.scope.insert(id.name.to_string(), value);
        Ok(())
    }

    fn lower_array_pattern(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        declarator: &VariableDeclarator<'_>,
        pattern: &oxc_ast::ast::ArrayPattern<'_>,
    ) -> Result<(), Vec<Diagnostic>> {
        let Some(init) = &declarator.init else {
            return Err(vec![refuse(
                "unsupported-statement",
                declarator.span,
                "a declaration without an initialiser is not lowered yet",
            )]);
        };
        let source = self.lower_expression(table, function, init)?;

        let mut names = Vec::with_capacity(pattern.elements.len());
        for element in &pattern.elements {
            let Some(element) = element.as_ref() else {
                return Err(vec![refuse(
                    "unsupported-pattern",
                    pattern.span,
                    "a hole in an array pattern is not lowered yet",
                )]);
            };
            let BindingPattern::BindingIdentifier(id) = element else {
                return Err(vec![refuse(
                    "unsupported-pattern",
                    pattern.span,
                    "an array pattern that binds anything but a plain name is not lowered yet",
                )]);
            };
            names.push(id);
        }

        for (position, id) in names.iter().enumerate() {
            let index = self.index_constant(table, function, position, id.span);
            let value = self.read_element_at(table, function, source, index, id.span)?;
            self.scope.insert(id.name.to_string(), value);
        }

        if let Some(rest) = &pattern.rest {
            let BindingPattern::BindingIdentifier(id) = &rest.argument else {
                return Err(vec![refuse(
                    "unsupported-pattern",
                    pattern.span,
                    "a rest in an array pattern that binds anything but a plain name is not lowered yet",
                )]);
            };
            let from = u32::try_from(names.len()).expect("a pattern is small");
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
            self.scope.insert(id.name.to_string(), value);
        }
        Ok(())
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
                    return Ok(*known);
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
                let ty = self.type_at(table, id.span)?;
                Ok(self.assign(function, ty, Rvalue::Global(id.name.to_string()), id.span))
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
                Ok(self.assign(
                    function,
                    held,
                    Rvalue::Await(promised),
                    await_expression.span,
                ))
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
        Expression::ThisExpression(_) => "this",
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
