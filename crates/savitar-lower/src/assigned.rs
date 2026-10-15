use std::collections::BTreeSet;

use oxc_ast::ast::{AssignmentTarget, Expression, SimpleAssignmentTarget, Statement};

#[must_use]
pub fn assigned_names(statements: &[Statement<'_>]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for statement in statements {
        walk_statement(statement, &mut found);
    }
    found
}

fn walk_statement(statement: &Statement<'_>, found: &mut BTreeSet<String>) {
    match statement {
        Statement::ExpressionStatement(statement) => {
            walk_expression(&statement.expression, found);
        }
        Statement::BlockStatement(block) => {
            for inner in &block.body {
                walk_statement(inner, found);
            }
        }
        Statement::IfStatement(statement) => {
            walk_expression(&statement.test, found);
            walk_statement(&statement.consequent, found);
            if let Some(alternate) = &statement.alternate {
                walk_statement(alternate, found);
            }
        }
        Statement::WhileStatement(statement) => {
            walk_expression(&statement.test, found);
            walk_statement(&statement.body, found);
        }
        Statement::DoWhileStatement(statement) => {
            walk_expression(&statement.test, found);
            walk_statement(&statement.body, found);
        }
        Statement::ForStatement(statement) => {
            if let Some(test) = &statement.test {
                walk_expression(test, found);
            }
            if let Some(update) = &statement.update {
                walk_expression(update, found);
            }
            walk_statement(&statement.body, found);
        }
        Statement::SwitchStatement(statement) => {
            walk_expression(&statement.discriminant, found);
            for case in &statement.cases {
                if let Some(test) = &case.test {
                    walk_expression(test, found);
                }
                for inner in &case.consequent {
                    walk_statement(inner, found);
                }
            }
        }
        Statement::ReturnStatement(statement) => {
            if let Some(argument) = &statement.argument {
                walk_expression(argument, found);
            }
        }
        Statement::VariableDeclaration(declaration) => {
            for declarator in &declaration.declarations {
                if let Some(init) = &declarator.init {
                    walk_expression(init, found);
                }
            }
        }
        _ => {}
    }
}

fn walk_expression(expression: &Expression<'_>, found: &mut BTreeSet<String>) {
    match expression {
        Expression::AssignmentExpression(assignment) => {
            if let AssignmentTarget::AssignmentTargetIdentifier(id) = &assignment.left {
                found.insert(id.name.to_string());
            }
            walk_expression(&assignment.right, found);
        }
        Expression::UpdateExpression(update) => {
            if let SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = &update.argument {
                found.insert(id.name.to_string());
            }
        }
        Expression::ParenthesizedExpression(inner) => walk_expression(&inner.expression, found),
        Expression::UnaryExpression(unary) => walk_expression(&unary.argument, found),
        Expression::BinaryExpression(binary) => {
            walk_expression(&binary.left, found);
            walk_expression(&binary.right, found);
        }
        Expression::LogicalExpression(logical) => {
            walk_expression(&logical.left, found);
            walk_expression(&logical.right, found);
        }
        Expression::ConditionalExpression(conditional) => {
            walk_expression(&conditional.test, found);
            walk_expression(&conditional.consequent, found);
            walk_expression(&conditional.alternate, found);
        }
        Expression::CallExpression(call) => {
            walk_expression(&call.callee, found);
            for argument in &call.arguments {
                if let Some(expression) = argument.as_expression() {
                    walk_expression(expression, found);
                }
            }
        }
        Expression::StaticMemberExpression(member) => walk_expression(&member.object, found),
        Expression::ComputedMemberExpression(member) => {
            walk_expression(&member.object, found);
            walk_expression(&member.expression, found);
        }
        Expression::ArrayExpression(array) => {
            for element in &array.elements {
                if let Some(expression) = element.as_expression() {
                    walk_expression(expression, found);
                }
            }
        }
        _ => {}
    }
}
