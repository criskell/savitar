mod assigned;
mod types;

use std::collections::HashMap;

use oxc_allocator::Allocator;
use oxc_ast::ast::VariableDeclarationKind;
use oxc_ast::ast::{
    Argument, AssignmentTarget, BindingPattern, Expression, FunctionBody, ObjectPropertyKind,
    SimpleAssignmentTarget, Statement as AstStatement, VariableDeclarator,
};
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxc_syntax::operator::{AssignmentOperator, BinaryOperator, UnaryOperator, UpdateOperator};
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
}

struct Context<'a> {
    file: &'a SourceFile,
    provider: &'a dyn TypeProvider,
    scope: Scope,
    statements: Vec<Statement>,
    current: BlockId,
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
        let AstStatement::FunctionDeclaration(declaration) = statement else {
            continue;
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
    let mut function = Function::new(name);
    let entry = function.add_block(empty_block());
    let mut context = Context {
        file,
        provider,
        scope: Scope::new(),
        statements: Vec::new(),
        current: entry,
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

impl Context<'_> {
    fn type_at(
        &self,
        table: &mut TypeTable,
        span: oxc_span::Span,
    ) -> Result<TypeId, Vec<Diagnostic>> {
        let query = Span::new(span.end.saturating_sub(1), span.end);
        match self.provider.type_at(table, self.file.name(), query) {
            Answer::Known(id) => Ok(id),
            Answer::Unsupported(reason) => Err(vec![refuse("unsupported-type", span, reason)]),
            Answer::OutOfScope => Err(vec![refuse(
                "no-type-answer",
                span,
                "the provider has no type here",
            )]),
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
            match self.lower_statement(table, function, statement)? {
                Flow::Continues => {}
                Flow::Returned => return Ok(Flow::Returned),
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
                    Some(argument) => Some(self.lower_expression(table, function, argument)?),
                    None => None,
                };
                self.seal(function, Terminator::Return(returned));
                Ok(Flow::Returned)
            }
            AstStatement::BlockStatement(block) => {
                self.lower_statements(table, function, &block.body)
            }
            AstStatement::IfStatement(statement) => self.lower_if(table, function, statement),
            AstStatement::WhileStatement(statement) => self.lower_while(table, function, statement),
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
        use oxc_span::GetSpan;
        if let Expression::UpdateExpression(update) = expression {
            let SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = &update.argument else {
                return Err(vec![refuse(
                    "unsupported-statement",
                    update.span,
                    "only `++` and `--` on a plain name are lowered yet",
                )]);
            };
            let held = self.bound(id.name.as_str(), id.span)?;
            let op = if update.operator == UpdateOperator::Increment {
                BinaryOp::Add
            } else {
                BinaryOp::Subtract
            };
            let one_type = table.number_literal(1.0);
            let one = self.assign(
                function,
                one_type,
                Rvalue::Constant(Constant::Number(1.0f64.to_bits())),
                update.span,
            );
            let stepped = self.combine(table, function, op, held, one, update.span)?;
            self.scope.insert(id.name.to_string(), stepped);
            return Ok(());
        }

        let Expression::AssignmentExpression(assignment) = expression else {
            self.lower_expression(table, function, expression)?;
            return Ok(());
        };
        let AssignmentTarget::AssignmentTargetIdentifier(id) = &assignment.left else {
            return Err(vec![refuse(
                "unsupported-statement",
                assignment.span(),
                "only assignment to a plain name is lowered yet",
            )]);
        };
        let held = self.bound(id.name.as_str(), id.span)?;
        let value = self.lower_expression(table, function, &assignment.right)?;
        let stored = match assignment.operator {
            AssignmentOperator::Assign => value,
            AssignmentOperator::Addition => {
                self.combine(table, function, BinaryOp::Add, held, value, assignment.span)?
            }
            AssignmentOperator::Subtraction => self.combine(
                table,
                function,
                BinaryOp::Subtract,
                held,
                value,
                assignment.span,
            )?,
            AssignmentOperator::Multiplication => self.combine(
                table,
                function,
                BinaryOp::Multiply,
                held,
                value,
                assignment.span,
            )?,
            AssignmentOperator::Division => self.combine(
                table,
                function,
                BinaryOp::Divide,
                held,
                value,
                assignment.span,
            )?,
            AssignmentOperator::Remainder => self.combine(
                table,
                function,
                BinaryOp::Remainder,
                held,
                value,
                assignment.span,
            )?,
            other => {
                return Err(vec![refuse(
                    "unsupported-operator",
                    assignment.span,
                    format!("the assignment `{}` is not lowered yet", other.as_str()),
                )]);
            }
        };
        self.scope.insert(id.name.to_string(), stored);
        Ok(())
    }

    fn lower_while(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::WhileStatement<'_>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let carried: Vec<String> = assigned::assigned_names(std::slice::from_ref(&statement.body))
            .into_iter()
            .filter(|name| self.scope.contains_key(name))
            .collect();

        let header = function.add_block(empty_block());
        let mut parameters = Vec::with_capacity(carried.len());
        let mut entry_arguments = Vec::with_capacity(carried.len());
        for name in &carried {
            let incoming = self.scope[name];
            let parameter = function.fresh_value(function.type_of(incoming));
            parameters.push(parameter);
            entry_arguments.push(incoming);
        }
        function.block_mut(header).parameters = parameters.clone();
        self.seal(
            function,
            Terminator::Jump {
                target: header,
                arguments: entry_arguments,
            },
        );

        self.current = header;
        self.statements.clear();
        for (name, parameter) in carried.iter().zip(&parameters) {
            self.scope.insert(name.clone(), *parameter);
        }
        let at_header = self.scope.clone();

        let condition = self.lower_expression(table, function, &statement.test)?;
        let body = function.add_block(empty_block());
        let exit = function.add_block(empty_block());
        self.seal(
            function,
            Terminator::Branch {
                condition,
                then_block: body,
                then_arguments: Vec::new(),
                else_block: exit,
                else_arguments: Vec::new(),
            },
        );

        self.current = body;
        self.statements.clear();
        let body_flow = self.lower_statement(table, function, &statement.body)?;

        if body_flow == Flow::Continues {
            let mut back_arguments = Vec::with_capacity(carried.len());
            for name in &carried {
                back_arguments.push(self.scope[name]);
            }
            for (name, value) in &self.scope {
                if !at_header.contains_key(name) {
                    continue;
                }
                if at_header.get(name) != Some(value) && !carried.contains(name) {
                    return Err(vec![refuse(
                        "malformed-ir",
                        statement.span,
                        format!("`{name}` changes in the loop but carries no header parameter"),
                    )]);
                }
            }
            self.seal(
                function,
                Terminator::Jump {
                    target: header,
                    arguments: back_arguments,
                },
            );
        }

        self.scope = at_header;
        self.current = exit;
        self.statements.clear();
        Ok(Flow::Continues)
    }

    fn lower_if(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        statement: &oxc_ast::ast::IfStatement<'_>,
    ) -> Result<Flow, Vec<Diagnostic>> {
        let condition = self.lower_expression(table, function, &statement.test)?;
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
        let then_flow = self.lower_statement(table, function, &statement.consequent)?;
        let then_exit = self.current;
        if then_flow == Flow::Continues {
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
        let else_flow = match &statement.alternate {
            Some(alternate) => self.lower_statement(table, function, alternate)?,
            None => Flow::Continues,
        };
        let else_exit = self.current;
        if else_flow == Flow::Continues {
            self.seal(
                function,
                Terminator::Jump {
                    target: join,
                    arguments: Vec::new(),
                },
            );
        }
        let else_scope = std::mem::take(&mut self.scope);

        if then_flow == Flow::Returned && else_flow == Flow::Returned {
            self.scope = before;
            return Ok(Flow::Returned);
        }

        let mut parameters = Vec::new();
        let mut then_arguments = Vec::new();
        let mut else_arguments = Vec::new();
        let mut merged = before.clone();

        if then_flow == Flow::Continues && else_flow == Flow::Continues {
            let mut names: Vec<&String> = before.keys().collect();
            names.sort();
            for name in names {
                let from_then = then_scope[name];
                let from_else = else_scope[name];
                if from_then == from_else {
                    merged.insert(name.clone(), from_then);
                    continue;
                }
                if function.type_of(from_then) != function.type_of(from_else) {
                    return Err(vec![refuse(
                        "unsupported-statement",
                        statement.span,
                        format!("`{name}` leaves the branches with two different types"),
                    )]);
                }
                let parameter = function.fresh_value(function.type_of(from_then));
                parameters.push(parameter);
                then_arguments.push(from_then);
                else_arguments.push(from_else);
                merged.insert(name.clone(), parameter);
            }
        } else if then_flow == Flow::Continues {
            merged = then_scope;
        } else {
            merged = else_scope;
        }

        function.block_mut(join).parameters = parameters;
        if then_flow == Flow::Continues {
            function.block_mut(then_exit).terminator = Terminator::Jump {
                target: join,
                arguments: then_arguments,
            };
        }
        if else_flow == Flow::Continues {
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

    fn lower_declarator(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        declarator: &VariableDeclarator<'_>,
        widening: bool,
    ) -> Result<(), Vec<Diagnostic>> {
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
        let value = self.lower_expression(table, function, init)?;
        let value = if widening {
            self.widened(table, function, value, declarator.span)
        } else {
            value
        };
        self.scope.insert(id.name.to_string(), value);
        Ok(())
    }

    fn lower_expression(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        expression: &Expression<'_>,
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
                Err(vec![refuse(
                    "unsupported-reference",
                    id.span,
                    format!("`{}` is not bound in this function", id.name),
                )])
            }
            Expression::ParenthesizedExpression(inner) => {
                self.lower_expression(table, function, &inner.expression)
            }
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
                let ty = types::field_result(table, function.type_of(object), &name)
                    .map_err(|reason| vec![refuse("unsupported-read", member.span, reason)])?;
                Ok(self.assign(
                    function,
                    ty,
                    Rvalue::ReadField { object, name },
                    member.span,
                ))
            }
            Expression::ComputedMemberExpression(member) => {
                let array = self.lower_expression(table, function, &member.object)?;
                let index = self.lower_expression(table, function, &member.expression)?;
                let ty =
                    types::element_result(table, function.type_of(array), function.type_of(index))
                        .map_err(|reason| vec![refuse("unsupported-read", member.span, reason)])?;
                Ok(self.assign(
                    function,
                    ty,
                    Rvalue::ReadElement { array, index },
                    member.span,
                ))
            }
            Expression::ObjectExpression(object) => {
                let mut values = Vec::with_capacity(object.properties.len());
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
                    let value = self.lower_expression(table, function, &property.value)?;
                    values.push((name.to_string(), value));
                }
                let fields = values
                    .iter()
                    .map(|(name, value)| Field {
                        name: name.clone(),
                        type_id: function.type_of(*value),
                        optional: false,
                        method: false,
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
                let mut elements = Vec::with_capacity(array.elements.len());
                for element in &array.elements {
                    let Some(element) = element.as_expression() else {
                        return Err(vec![refuse(
                            "unsupported-expression",
                            array.span,
                            "a hole or spread in an array literal is not lowered yet",
                        )]);
                    };
                    elements.push(self.lower_expression(table, function, element)?);
                }
                if elements.is_empty() {
                    return Err(vec![refuse(
                        "unsupported-expression",
                        array.span,
                        "an empty array literal names no element type",
                    )]);
                }
                let members: Vec<TypeId> =
                    elements.iter().map(|id| function.type_of(*id)).collect();
                let element = table.union(members);
                let ty = table.intern(TypeKind::Array(element));
                Ok(self.assign(function, ty, Rvalue::NewArray { elements }, array.span))
            }
            Expression::CallExpression(call) => {
                let callee = self.lower_expression(table, function, &call.callee)?;
                let mut arguments = Vec::with_capacity(call.arguments.len());
                for argument in &call.arguments {
                    let Argument::SpreadElement(_) = argument else {
                        let Some(expression) = argument.as_expression() else {
                            return Err(vec![refuse(
                                "unsupported-expression",
                                call.span,
                                "an argument that is not an expression is not lowered yet",
                            )]);
                        };
                        arguments.push(self.lower_expression(table, function, expression)?);
                        continue;
                    };
                    return Err(vec![refuse(
                        "unsupported-expression",
                        call.span,
                        "a spread argument is not lowered yet",
                    )]);
                }
                let ty = types::call_result(table, function.type_of(callee))
                    .map_err(|reason| vec![refuse("unsupported-call", call.span, reason)])?;
                Ok(self.assign(function, ty, Rvalue::Call { callee, arguments }, call.span))
            }
            other => {
                use oxc_span::GetSpan;
                Err(vec![refuse(
                    "unsupported-expression",
                    other.span(),
                    "this expression is not lowered yet",
                )])
            }
        }
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

fn unary_op(operator: UnaryOperator) -> Option<UnaryOp> {
    Some(match operator {
        UnaryOperator::UnaryNegation => UnaryOp::Negate,
        UnaryOperator::LogicalNot => UnaryOp::Not,
        UnaryOperator::Typeof => UnaryOp::TypeOf,
        _ => return None,
    })
}
