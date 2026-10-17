use std::collections::BTreeSet;

use oxc_ast::ast::{
    ArrowFunctionBody, AssignmentTarget, AssignmentTargetMaybeDefault, AssignmentTargetProperty,
    Expression, ForStatementInit, FormalParameters, SimpleAssignmentTarget, Statement,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    ThisBody,
    ThroughClosures,
}

#[must_use]
pub fn assigned_names(statements: &[Statement<'_>]) -> BTreeSet<String> {
    walked(statements, Reach::ThisBody)
}

#[must_use]
pub fn names_written_through_closures(statements: &[Statement<'_>]) -> BTreeSet<String> {
    walked(statements, Reach::ThroughClosures)
}

fn walked(statements: &[Statement<'_>], reach: Reach) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for statement in statements {
        walk_statement(statement, &mut found, reach);
    }
    found
}

#[must_use]
pub fn assigned_names_in(expression: &Expression<'_>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    walk_expression(expression, &mut found, Reach::ThisBody);
    found
}

fn walk_statement(statement: &Statement<'_>, found: &mut BTreeSet<String>, reach: Reach) {
    match statement {
        Statement::ExpressionStatement(statement) => {
            walk_expression(&statement.expression, found, reach);
        }
        Statement::BlockStatement(block) => {
            for inner in &block.body {
                walk_statement(inner, found, reach);
            }
        }
        Statement::LabeledStatement(statement) => {
            walk_statement(&statement.body, found, reach);
        }
        Statement::IfStatement(statement) => {
            walk_expression(&statement.test, found, reach);
            walk_statement(&statement.consequent, found, reach);
            if let Some(alternate) = &statement.alternate {
                walk_statement(alternate, found, reach);
            }
        }
        Statement::WhileStatement(statement) => {
            walk_expression(&statement.test, found, reach);
            walk_statement(&statement.body, found, reach);
        }
        Statement::DoWhileStatement(statement) => {
            walk_expression(&statement.test, found, reach);
            walk_statement(&statement.body, found, reach);
        }
        Statement::ForStatement(statement) => {
            match &statement.init {
                Some(ForStatementInit::VariableDeclaration(declaration)) => {
                    for declarator in &declaration.declarations {
                        if let Some(init) = &declarator.init {
                            walk_expression(init, found, reach);
                        }
                    }
                }
                Some(init) => {
                    if let Some(expression) = init.as_expression() {
                        walk_expression(expression, found, reach);
                    }
                }
                None => {}
            }
            if let Some(test) = &statement.test {
                walk_expression(test, found, reach);
            }
            if let Some(update) = &statement.update {
                walk_expression(update, found, reach);
            }
            walk_statement(&statement.body, found, reach);
        }
        Statement::ForOfStatement(statement) => {
            walk_expression(&statement.right, found, reach);
            walk_statement(&statement.body, found, reach);
        }
        Statement::SwitchStatement(statement) => {
            walk_expression(&statement.discriminant, found, reach);
            for case in &statement.cases {
                if let Some(test) = &case.test {
                    walk_expression(test, found, reach);
                }
                for inner in &case.consequent {
                    walk_statement(inner, found, reach);
                }
            }
        }
        Statement::ReturnStatement(statement) => {
            if let Some(argument) = &statement.argument {
                walk_expression(argument, found, reach);
            }
        }
        Statement::VariableDeclaration(declaration) => {
            for declarator in &declaration.declarations {
                if let Some(init) = &declarator.init {
                    walk_expression(init, found, reach);
                }
            }
        }
        _ => {}
    }
}

fn walk_target(target: &AssignmentTarget<'_>, found: &mut BTreeSet<String>, reach: Reach) {
    match target {
        AssignmentTarget::AssignmentTargetIdentifier(id) => {
            found.insert(id.name.to_string());
        }
        AssignmentTarget::ArrayAssignmentTarget(array) => {
            for element in array.elements.iter().flatten() {
                walk_maybe_defaulted(element, found, reach);
            }
            if let Some(rest) = &array.rest {
                walk_target(&rest.target, found, reach);
            }
        }
        AssignmentTarget::ObjectAssignmentTarget(object) => {
            for property in &object.properties {
                match property {
                    AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(shorthand) => {
                        found.insert(shorthand.binding.name.to_string());
                        if let Some(init) = &shorthand.init {
                            walk_expression(init, found, reach);
                        }
                    }
                    AssignmentTargetProperty::AssignmentTargetPropertyProperty(renamed) => {
                        walk_maybe_defaulted(&renamed.binding, found, reach);
                    }
                }
            }
            if let Some(rest) = &object.rest {
                walk_target(&rest.target, found, reach);
            }
        }
        _ => {}
    }
}

fn walk_maybe_defaulted(
    target: &AssignmentTargetMaybeDefault<'_>,
    found: &mut BTreeSet<String>,
    reach: Reach,
) {
    match target {
        AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(with) => {
            walk_target(&with.binding, found, reach);
            walk_expression(&with.init, found, reach);
        }
        other => {
            if let Some(plain) = other.as_assignment_target() {
                walk_target(plain, found, reach);
            }
        }
    }
}

fn walk_expression(expression: &Expression<'_>, found: &mut BTreeSet<String>, reach: Reach) {
    match expression {
        Expression::AssignmentExpression(assignment) => {
            walk_target(&assignment.left, found, reach);
            walk_expression(&assignment.right, found, reach);
        }
        Expression::UpdateExpression(update) => {
            if let SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = &update.argument {
                found.insert(id.name.to_string());
            }
        }
        Expression::ParenthesizedExpression(inner) => {
            walk_expression(&inner.expression, found, reach)
        }
        Expression::UnaryExpression(unary) => walk_expression(&unary.argument, found, reach),
        Expression::BinaryExpression(binary) => {
            walk_expression(&binary.left, found, reach);
            walk_expression(&binary.right, found, reach);
        }
        Expression::LogicalExpression(logical) => {
            walk_expression(&logical.left, found, reach);
            walk_expression(&logical.right, found, reach);
        }
        Expression::ConditionalExpression(conditional) => {
            walk_expression(&conditional.test, found, reach);
            walk_expression(&conditional.consequent, found, reach);
            walk_expression(&conditional.alternate, found, reach);
        }
        Expression::CallExpression(call) => {
            walk_expression(&call.callee, found, reach);
            for argument in &call.arguments {
                if let Some(expression) = argument.as_expression() {
                    walk_expression(expression, found, reach);
                }
            }
        }
        Expression::StaticMemberExpression(member) => walk_expression(&member.object, found, reach),
        Expression::ComputedMemberExpression(member) => {
            walk_expression(&member.object, found, reach);
            walk_expression(&member.expression, found, reach);
        }
        Expression::ArrayExpression(array) => {
            for element in &array.elements {
                if let Some(expression) = element.as_expression() {
                    walk_expression(expression, found, reach);
                }
            }
        }
        Expression::ArrowFunctionExpression(arrow) if reach == Reach::ThroughClosures => {
            walk_parameters(&arrow.params, found, reach);
            match &arrow.body {
                ArrowFunctionBody::FunctionBody(body) => {
                    for statement in &body.statements {
                        walk_statement(statement, found, reach);
                    }
                }
                returned => {
                    if let Some(expression) = returned.as_expression() {
                        walk_expression(expression, found, reach);
                    }
                }
            }
        }
        Expression::FunctionExpression(declaration) if reach == Reach::ThroughClosures => {
            walk_parameters(&declaration.params, found, reach);
            if let Some(body) = declaration.body.as_ref() {
                for statement in &body.statements {
                    walk_statement(statement, found, reach);
                }
            }
        }
        _ => {}
    }
}

fn walk_parameters(parameters: &FormalParameters<'_>, found: &mut BTreeSet<String>, reach: Reach) {
    for parameter in &parameters.items {
        if let Some(initializer) = parameter.initializer.as_deref() {
            walk_expression(initializer, found, reach);
        }
    }
}
