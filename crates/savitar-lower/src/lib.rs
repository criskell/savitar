mod types;

use std::collections::HashMap;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, BindingPattern, Expression, FunctionBody, ObjectPropertyKind,
    Statement as AstStatement, VariableDeclarator,
};
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxc_syntax::operator::{BinaryOperator, UnaryOperator};
use savitar_diagnostics::{Diagnostic, SourceFile, Span};
use savitar_ir::{
    BinaryOp, Block, Constant, Function, Rvalue, Statement, Terminator, UnaryOp, ValueId, verify,
};
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_types::{Field, TypeId, TypeKind, TypeTable};

pub use types::{binary_result, element_result, field_result, unary_result};

#[derive(Debug)]
pub struct LowerReport {
    pub functions: Vec<Function>,
    pub diagnostics: Vec<Diagnostic>,
}

struct Context<'a> {
    file: &'a SourceFile,
    provider: &'a dyn TypeProvider,
    scope: HashMap<String, ValueId>,
    statements: Vec<Statement>,
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

fn lower_function(
    file: &SourceFile,
    provider: &dyn TypeProvider,
    table: &mut TypeTable,
    name: &str,
    declaration: &oxc_ast::ast::Function<'_>,
    body: &FunctionBody<'_>,
) -> Result<Function, Vec<Diagnostic>> {
    let mut function = Function::new(name);
    let mut context = Context {
        file,
        provider,
        scope: HashMap::new(),
        statements: Vec::new(),
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

    let mut returned = None;
    for statement in &body.statements {
        match statement {
            AstStatement::VariableDeclaration(declaration) => {
                for declarator in &declaration.declarations {
                    context.lower_declarator(table, &mut function, declarator)?;
                }
            }
            AstStatement::ReturnStatement(statement) => {
                let Some(argument) = &statement.argument else {
                    break;
                };
                returned = Some(context.lower_expression(table, &mut function, argument)?);
                break;
            }
            other => {
                return Err(vec![refuse(
                    "unsupported-statement",
                    statement_span(other),
                    "only declarations and a final return are lowered yet",
                )]);
            }
        }
    }

    function.add_block(Block {
        parameters: Vec::new(),
        statements: context.statements,
        terminator: Terminator::Return(returned),
    });

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

    fn lower_declarator(
        &mut self,
        table: &mut TypeTable,
        function: &mut Function,
        declarator: &VariableDeclarator<'_>,
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
