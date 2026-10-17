use std::collections::BTreeSet;

use oxc_ast::ast::{
    AssignmentTarget, AssignmentTargetMaybeDefault, AssignmentTargetProperty, Expression,
    ForStatementInit, SimpleAssignmentTarget, Statement,
};

#[must_use]
pub fn assigned_names(statements: &[Statement<'_>]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for statement in statements {
        walk_statement(statement, &mut found);
    }
    found
}

#[must_use]
pub fn assigned_names_in(expression: &Expression<'_>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    walk_expression(expression, &mut found);
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
        Statement::LabeledStatement(statement) => {
            walk_statement(&statement.body, found);
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
            match &statement.init {
                Some(ForStatementInit::VariableDeclaration(declaration)) => {
                    for declarator in &declaration.declarations {
                        if let Some(init) = &declarator.init {
                            walk_expression(init, found);
                        }
                    }
                }
                Some(init) => {
                    if let Some(expression) = init.as_expression() {
                        walk_expression(expression, found);
                    }
                }
                None => {}
            }
            if let Some(test) = &statement.test {
                walk_expression(test, found);
            }
            if let Some(update) = &statement.update {
                walk_expression(update, found);
            }
            walk_statement(&statement.body, found);
        }
        Statement::ForOfStatement(statement) => {
            walk_expression(&statement.right, found);
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

fn walk_target(target: &AssignmentTarget<'_>, found: &mut BTreeSet<String>) {
    match target {
        AssignmentTarget::AssignmentTargetIdentifier(id) => {
            found.insert(id.name.to_string());
        }
        AssignmentTarget::ArrayAssignmentTarget(array) => {
            for element in array.elements.iter().flatten() {
                walk_maybe_defaulted(element, found);
            }
            if let Some(rest) = &array.rest {
                walk_target(&rest.target, found);
            }
        }
        AssignmentTarget::ObjectAssignmentTarget(object) => {
            for property in &object.properties {
                match property {
                    AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(shorthand) => {
                        found.insert(shorthand.binding.name.to_string());
                        if let Some(init) = &shorthand.init {
                            walk_expression(init, found);
                        }
                    }
                    AssignmentTargetProperty::AssignmentTargetPropertyProperty(renamed) => {
                        walk_maybe_defaulted(&renamed.binding, found);
                    }
                }
            }
            if let Some(rest) = &object.rest {
                walk_target(&rest.target, found);
            }
        }
        _ => {}
    }
}

fn walk_maybe_defaulted(target: &AssignmentTargetMaybeDefault<'_>, found: &mut BTreeSet<String>) {
    match target {
        AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(with) => {
            walk_target(&with.binding, found);
            walk_expression(&with.init, found);
        }
        other => {
            if let Some(plain) = other.as_assignment_target() {
                walk_target(plain, found);
            }
        }
    }
}

fn walk_expression(expression: &Expression<'_>, found: &mut BTreeSet<String>) {
    match expression {
        Expression::AssignmentExpression(assignment) => {
            walk_target(&assignment.left, found);
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
