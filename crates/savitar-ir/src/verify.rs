use std::collections::HashMap;

use crate::{Block, BlockId, Dominance, Function, Rvalue, Statement, Terminator, ValueId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Malformation {
    NoEntryBlock,
    EntryBlockTakesParameters,
    MissingBlock(BlockId),
    ValueDefinedTwice(ValueId),
    ValueNeverDefined(ValueId),
    UseBeforeDefinition {
        value: ValueId,
        block: BlockId,
    },
    JumpArgumentCount {
        from: BlockId,
        to: BlockId,
        given: usize,
        expected: usize,
    },
    JumpArgumentType {
        from: BlockId,
        to: BlockId,
        position: usize,
    },
}

#[must_use]
pub fn verify(function: &Function) -> Vec<Malformation> {
    let mut found = Vec::new();

    let Some(entry) = function.entry() else {
        return vec![Malformation::NoEntryBlock];
    };
    if !function.block(entry).parameters.is_empty() {
        found.push(Malformation::EntryBlockTakesParameters);
    }

    for (id, block) in function.blocks() {
        for successor in block.terminator.successors() {
            if !function.has_block(successor) {
                found.push(Malformation::MissingBlock(successor));
            }
        }
        if let Some(problem) = jump_mismatch(function, id, block) {
            found.push(problem);
        }
    }
    if !found.is_empty() {
        return found;
    }

    let mut defined_in: HashMap<ValueId, BlockId> = HashMap::new();
    let mut definition_index: HashMap<ValueId, usize> = HashMap::new();
    for parameter in function.parameters() {
        if defined_in.insert(*parameter, entry).is_some() {
            found.push(Malformation::ValueDefinedTwice(*parameter));
        }
        definition_index.insert(*parameter, 0);
    }
    for (id, block) in function.blocks() {
        for parameter in &block.parameters {
            if defined_in.insert(*parameter, id).is_some() {
                found.push(Malformation::ValueDefinedTwice(*parameter));
            }
            definition_index.insert(*parameter, 0);
        }
        for (at, statement) in block.statements.iter().enumerate() {
            if let Statement::Assign { target, .. } = statement {
                if defined_in.insert(*target, id).is_some() {
                    found.push(Malformation::ValueDefinedTwice(*target));
                }
                definition_index.insert(*target, at + 1);
            }
        }
    }

    let dominance = Dominance::of(function);
    for (id, block) in function.blocks() {
        if !dominance.reachable(id) {
            continue;
        }
        for (at, statement) in block.statements.iter().enumerate() {
            for used in statement_uses(statement) {
                check_use(
                    &dominance,
                    &defined_in,
                    &definition_index,
                    used,
                    id,
                    at,
                    &mut found,
                );
            }
        }
        for used in terminator_uses(&block.terminator) {
            check_use(
                &dominance,
                &defined_in,
                &definition_index,
                used,
                id,
                block.statements.len(),
                &mut found,
            );
        }
    }

    found
}

fn check_use(
    dominance: &Dominance,
    defined_in: &HashMap<ValueId, BlockId>,
    definition_index: &HashMap<ValueId, usize>,
    value: ValueId,
    block: BlockId,
    at: usize,
    found: &mut Vec<Malformation>,
) {
    let Some(&definition) = defined_in.get(&value) else {
        found.push(Malformation::ValueNeverDefined(value));
        return;
    };
    let dominated = if definition == block {
        definition_index.get(&value).copied().unwrap_or(0) <= at
    } else {
        dominance.dominates(definition, block)
    };
    if !dominated {
        found.push(Malformation::UseBeforeDefinition { value, block });
    }
}

fn jump_mismatch(function: &Function, from: BlockId, block: &Block) -> Option<Malformation> {
    let edges: Vec<(BlockId, &[ValueId])> = match &block.terminator {
        Terminator::Jump { target, arguments } => vec![(*target, arguments.as_slice())],
        Terminator::Branch {
            then_block,
            then_arguments,
            else_block,
            else_arguments,
            ..
        } => vec![
            (*then_block, then_arguments.as_slice()),
            (*else_block, else_arguments.as_slice()),
        ],
        Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
    };

    for (to, arguments) in edges {
        if !function.has_block(to) {
            continue;
        }
        let expected = &function.block(to).parameters;
        if arguments.len() != expected.len() {
            return Some(Malformation::JumpArgumentCount {
                from,
                to,
                given: arguments.len(),
                expected: expected.len(),
            });
        }
        for (position, (given, wanted)) in arguments.iter().zip(expected).enumerate() {
            if function.type_of(*given) != function.type_of(*wanted) {
                return Some(Malformation::JumpArgumentType { from, to, position });
            }
        }
    }
    None
}

fn statement_uses(statement: &Statement) -> Vec<ValueId> {
    match statement {
        Statement::Assign { value, .. } => rvalue_uses(value),
        Statement::WriteField { object, value, .. } => vec![*object, *value],
        Statement::WriteElement {
            array,
            index,
            value,
            ..
        } => vec![*array, *index, *value],
    }
}

fn rvalue_uses(value: &Rvalue) -> Vec<ValueId> {
    match value {
        Rvalue::Parameter(_) | Rvalue::Global(_) | Rvalue::Constant(_) => Vec::new(),
        Rvalue::Widen(value) | Rvalue::Narrow(value) | Rvalue::Await(value) => vec![*value],
        Rvalue::Concat(parts) => parts.clone(),
        Rvalue::Unary { operand, .. } => vec![*operand],
        Rvalue::Binary { left, right, .. } => vec![*left, *right],
        Rvalue::ReadField { object, .. } => vec![*object],
        Rvalue::ReadElement { array, index } => vec![*array, *index],
        Rvalue::Slice { array, .. } => vec![*array],
        Rvalue::NewObject { fields } => fields.iter().map(|(_, id)| *id).collect(),
        Rvalue::NewArray { elements } => elements.clone(),
        Rvalue::Call { callee, arguments } => {
            let mut used = vec![*callee];
            used.extend(arguments.iter().copied());
            used
        }
    }
}

fn terminator_uses(terminator: &Terminator) -> Vec<ValueId> {
    match terminator {
        Terminator::Jump { arguments, .. } => arguments.clone(),
        Terminator::Branch {
            condition,
            then_arguments,
            else_arguments,
            ..
        } => {
            let mut used = vec![*condition];
            used.extend(then_arguments.iter().copied());
            used.extend(else_arguments.iter().copied());
            used
        }
        Terminator::Return(value) => value.iter().copied().collect(),
        Terminator::Unreachable => Vec::new(),
    }
}
