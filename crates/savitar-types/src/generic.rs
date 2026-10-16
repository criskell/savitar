use std::collections::{HashMap, HashSet};

use crate::{Field, IndexSignature, Parameter, TypeId, TypeKind, TypeTable};

pub type Bindings = HashMap<TypeId, TypeId>;

pub fn infer(
    table: &TypeTable,
    parameter: TypeId,
    argument: TypeId,
    bindings: &mut Bindings,
) -> Result<(), String> {
    let mut walked = HashSet::new();
    walk(table, parameter, argument, bindings, &mut walked)
}

fn walk(
    table: &TypeTable,
    parameter: TypeId,
    argument: TypeId,
    bindings: &mut Bindings,
    walked: &mut HashSet<(TypeId, TypeId)>,
) -> Result<(), String> {
    if !walked.insert((parameter, argument)) {
        return Ok(());
    }
    if let TypeKind::TypeParameter(_) = table.kind(parameter) {
        return match bindings.insert(parameter, argument) {
            Some(held) if held != argument => Err(
                "this call binds one type parameter to two different types, and Savitar does not choose between them"
                    .to_string(),
            ),
            _ => Ok(()),
        };
    }
    match (table.kind(parameter), table.kind(argument)) {
        (
            TypeKind::Array(held) | TypeKind::ReadonlyArray(held),
            TypeKind::Array(given) | TypeKind::ReadonlyArray(given),
        )
        | (TypeKind::Promise(held), TypeKind::Promise(given)) => {
            walk(table, *held, *given, bindings, walked)
        }
        (
            TypeKind::Array(held) | TypeKind::ReadonlyArray(held),
            TypeKind::Tuple { elements, .. },
        ) => {
            for &element in elements {
                walk(table, *held, element, bindings, walked)?;
            }
            Ok(())
        }
        (
            TypeKind::Tuple { elements, .. },
            TypeKind::Tuple {
                elements: given, ..
            },
        ) if elements.len() == given.len() => {
            for (&held, &given) in elements.iter().zip(given.iter()) {
                walk(table, held, given, bindings, walked)?;
            }
            Ok(())
        }
        (TypeKind::Object { fields, .. }, TypeKind::Object { fields: given, .. }) => {
            for field in fields {
                if let Some(given) = given.iter().find(|one| one.name == field.name) {
                    walk(table, field.type_id, given.type_id, bindings, walked)?;
                }
            }
            Ok(())
        }
        (
            TypeKind::Function {
                parameters,
                returns,
                ..
            },
            TypeKind::Function {
                parameters: given,
                returns: given_returns,
                ..
            },
        ) if parameters.len() == given.len() => {
            for (held, given) in parameters.iter().zip(given.iter()) {
                walk(table, held.type_id, given.type_id, bindings, walked)?;
            }
            walk(table, *returns, *given_returns, bindings, walked)
        }
        _ => Ok(()),
    }
}

pub fn substitute(
    table: &mut TypeTable,
    id: TypeId,
    bindings: &Bindings,
) -> Result<TypeId, String> {
    let mut entered = HashSet::new();
    replace(table, id, bindings, &mut entered)
}

fn replace(
    table: &mut TypeTable,
    id: TypeId,
    bindings: &Bindings,
    entered: &mut HashSet<TypeId>,
) -> Result<TypeId, String> {
    if !mentions_a_type_parameter(table, id) {
        return Ok(id);
    }
    if let Some(&bound) = bindings.get(&id) {
        return Ok(bound);
    }
    if !entered.insert(id) {
        return Err(
            "a recursive type carrying a type parameter is not substituted yet".to_string(),
        );
    }
    let kind = table.kind(id).clone();
    let built = match kind {
        TypeKind::TypeParameter(_) => {
            entered.remove(&id);
            return Err("this call leaves a type parameter with nothing to stand for".to_string());
        }
        TypeKind::Deferred(named) => {
            entered.remove(&id);
            return Err(format!(
                "this call reaches {}, which Savitar stopped unrolling",
                table.nominal_name(named)
            ));
        }
        TypeKind::Array(element) => TypeKind::Array(replace(table, element, bindings, entered)?),
        TypeKind::ReadonlyArray(element) => {
            TypeKind::ReadonlyArray(replace(table, element, bindings, entered)?)
        }
        TypeKind::Promise(held) => TypeKind::Promise(replace(table, held, bindings, entered)?),
        TypeKind::Tuple { elements, readonly } => {
            let mut built = Vec::with_capacity(elements.len());
            for element in elements {
                built.push(replace(table, element, bindings, entered)?);
            }
            TypeKind::Tuple {
                elements: built,
                readonly,
            }
        }
        TypeKind::Object { fields, indexes } => {
            let mut built = Vec::with_capacity(fields.len());
            for field in fields {
                built.push(Field {
                    type_id: replace(table, field.type_id, bindings, entered)?,
                    ..field
                });
            }
            let mut signatures = Vec::with_capacity(indexes.len());
            for index in indexes {
                signatures.push(IndexSignature {
                    key: replace(table, index.key, bindings, entered)?,
                    value: replace(table, index.value, bindings, entered)?,
                    readonly: index.readonly,
                });
            }
            TypeKind::Object {
                fields: built,
                indexes: signatures,
            }
        }
        TypeKind::Function {
            type_parameters,
            parameters,
            returns,
        } => {
            let kept: Vec<TypeId> = type_parameters
                .into_iter()
                .filter(|held| !bindings.contains_key(held))
                .collect();
            let mut built = Vec::with_capacity(parameters.len());
            for parameter in parameters {
                built.push(Parameter {
                    type_id: replace(table, parameter.type_id, bindings, entered)?,
                    ..parameter
                });
            }
            TypeKind::Function {
                type_parameters: kept,
                parameters: built,
                returns: replace(table, returns, bindings, entered)?,
            }
        }
        TypeKind::Conditional {
            check,
            bound,
            when_true,
            when_false,
        } => {
            let check = replace(table, check, bindings, entered)?;
            let bound = replace(table, bound, bindings, entered)?;
            let when_true = replace(table, when_true, bindings, entered)?;
            let when_false = replace(table, when_false, bindings, entered)?;
            entered.remove(&id);
            if mentions_a_type_parameter(table, check) || mentions_a_type_parameter(table, bound) {
                return Ok(table.conditional(check, bound, when_true, when_false));
            }
            return Ok(if crate::assignable(table, check, bound) {
                when_true
            } else {
                when_false
            });
        }
        TypeKind::IndexedAccess { object, index } => {
            let object = replace(table, object, bindings, entered)?;
            let index = replace(table, index, bindings, entered)?;
            entered.remove(&id);
            if mentions_a_type_parameter(table, object) || mentions_a_type_parameter(table, index) {
                return Ok(table.indexed_access(object, index));
            }
            return reach(table, object, index);
        }
        TypeKind::Overloaded(forms) => {
            let mut built = Vec::with_capacity(forms.len());
            for form in forms {
                built.push(replace(table, form, bindings, entered)?);
            }
            TypeKind::Overloaded(built)
        }
        TypeKind::Union(members) => {
            let mut built = Vec::with_capacity(members.len());
            for member in members {
                built.push(replace(table, member, bindings, entered)?);
            }
            entered.remove(&id);
            return Ok(table.union(built));
        }
        other => other,
    };
    entered.remove(&id);
    Ok(table.intern(built))
}

fn reach(table: &TypeTable, object: TypeId, index: TypeId) -> Result<TypeId, String> {
    match (table.kind(object), table.kind(index)) {
        (TypeKind::Object { fields, .. }, TypeKind::StringLiteral(name)) => fields
            .iter()
            .find(|field| field.name == *name)
            .map(|field| field.type_id)
            .ok_or_else(|| format!("`T[K]` reaches no field named `{name}`")),
        (
            TypeKind::Array(element) | TypeKind::ReadonlyArray(element),
            TypeKind::Number | TypeKind::NumberLiteral(_),
        ) => Ok(*element),
        (TypeKind::Object { indexes, .. }, key) if !indexes.is_empty() => indexes
            .iter()
            .find(|index| match table.kind(index.key) {
                TypeKind::String => {
                    matches!(key, TypeKind::String | TypeKind::StringLiteral(_))
                }
                TypeKind::Number => {
                    matches!(key, TypeKind::Number | TypeKind::NumberLiteral(_))
                }
                _ => false,
            })
            .map(|index| index.value)
            .ok_or_else(|| "`T[K]` has no index signature that takes this key".to_string()),
        (TypeKind::Tuple { elements, .. }, TypeKind::NumberLiteral(bits)) => {
            let wanted = f64::from_bits(*bits);
            elements
                .iter()
                .enumerate()
                .find_map(|(position, &element)| (position as f64 == wanted).then_some(element))
                .ok_or_else(|| "`T[K]` reaches outside the tuple".to_string())
        }
        _ => Err("`T[K]` over a shape Savitar cannot index yet".to_string()),
    }
}

#[must_use]
pub fn mentions_a_type_parameter(table: &TypeTable, id: TypeId) -> bool {
    let mut seen = HashSet::new();
    mentioned(table, id, &mut seen)
}

fn mentioned(table: &TypeTable, id: TypeId, seen: &mut HashSet<TypeId>) -> bool {
    if !seen.insert(id) {
        return false;
    }
    match table.kind(id) {
        TypeKind::TypeParameter(_) | TypeKind::Deferred(_) => true,
        TypeKind::Array(inner) | TypeKind::ReadonlyArray(inner) | TypeKind::Promise(inner) => {
            mentioned(table, *inner, seen)
        }
        TypeKind::Tuple { elements, .. } => elements
            .iter()
            .any(|&element| mentioned(table, element, seen)),
        TypeKind::Object { fields, indexes } => {
            fields
                .iter()
                .any(|field| mentioned(table, field.type_id, seen))
                || indexes.iter().any(|index| {
                    mentioned(table, index.key, seen) || mentioned(table, index.value, seen)
                })
        }
        TypeKind::Function {
            parameters,
            returns,
            ..
        } => {
            parameters
                .iter()
                .any(|parameter| mentioned(table, parameter.type_id, seen))
                || mentioned(table, *returns, seen)
        }
        TypeKind::Overloaded(forms) => forms.iter().any(|&form| mentioned(table, form, seen)),
        TypeKind::Conditional {
            check,
            bound,
            when_true,
            when_false,
        } => {
            mentioned(table, *check, seen)
                || mentioned(table, *bound, seen)
                || mentioned(table, *when_true, seen)
                || mentioned(table, *when_false, seen)
        }
        TypeKind::IndexedAccess { object, index } => {
            mentioned(table, *object, seen) || mentioned(table, *index, seen)
        }
        TypeKind::Union(members) => members.iter().any(|&member| mentioned(table, member, seen)),
        _ => false,
    }
}
