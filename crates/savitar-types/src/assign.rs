use std::collections::HashSet;

use crate::{Field, IndexSignature, TypeId, TypeKind, TypeTable};

#[must_use]
pub fn assignable(table: &TypeTable, from: TypeId, to: TypeId) -> bool {
    let mut seen = HashSet::new();
    relate(table, from, to, &mut seen)
}

fn relate(
    table: &TypeTable,
    from: TypeId,
    to: TypeId,
    seen: &mut HashSet<(TypeId, TypeId)>,
) -> bool {
    if from == to {
        return true;
    }
    if !seen.insert((from, to)) {
        return true;
    }
    let answer = decide(table, from, to, seen);
    seen.remove(&(from, to));
    answer
}

fn decide(
    table: &TypeTable,
    from: TypeId,
    to: TypeId,
    seen: &mut HashSet<(TypeId, TypeId)>,
) -> bool {
    if matches!(table.kind(from), TypeKind::Never) || matches!(table.kind(to), TypeKind::Unknown) {
        return true;
    }
    if let TypeKind::Union(members) = table.kind(from) {
        return members
            .iter()
            .all(|&member| relate(table, member, to, seen));
    }
    if let TypeKind::Union(members) = table.kind(to) {
        return members
            .iter()
            .any(|&member| relate(table, from, member, seen));
    }
    if matches!(table.kind(to), TypeKind::NonPrimitive) {
        return is_object_like(table, from);
    }

    match (table.kind(from), table.kind(to)) {
        (TypeKind::StringLiteral(_), TypeKind::String)
        | (TypeKind::NumberLiteral(_), TypeKind::Number)
        | (TypeKind::BooleanLiteral(_), TypeKind::Boolean) => true,
        (TypeKind::Array(element), TypeKind::Array(wanted)) => element == wanted,
        (
            TypeKind::Array(element) | TypeKind::ReadonlyArray(element),
            TypeKind::ReadonlyArray(wanted),
        ) => relate(table, *element, *wanted, seen),
        (TypeKind::Tuple { elements, readonly }, TypeKind::Array(wanted)) => {
            !readonly && elements.iter().all(|element| element == wanted)
        }
        (TypeKind::Tuple { elements, .. }, TypeKind::ReadonlyArray(wanted)) => elements
            .iter()
            .all(|&element| relate(table, element, *wanted, seen)),
        (
            TypeKind::Tuple {
                elements,
                readonly: source_readonly,
            },
            TypeKind::Tuple {
                elements: wanted,
                readonly: target_readonly,
            },
        ) => {
            if elements.len() != wanted.len() || (*source_readonly && !target_readonly) {
                return false;
            }
            elements
                .iter()
                .zip(wanted.iter())
                .all(|(&element, &wanted)| {
                    if *target_readonly {
                        relate(table, element, wanted, seen)
                    } else {
                        element == wanted
                    }
                })
        }
        (TypeKind::Promise(held), TypeKind::Promise(wanted)) => relate(table, *held, *wanted, seen),
        (
            TypeKind::Object { fields, indexes },
            TypeKind::Object {
                fields: wanted,
                indexes: wanted_indexes,
            },
        ) => object_fits(table, fields, indexes, wanted, wanted_indexes),
        (
            TypeKind::Function {
                type_parameters,
                parameters,
                returns,
            },
            TypeKind::Function {
                type_parameters: wanted_parameters,
                parameters: wanted,
                returns: wanted_returns,
            },
        ) => {
            if !type_parameters.is_empty() || !wanted_parameters.is_empty() {
                return false;
            }
            if parameters.len() > wanted.len() {
                return false;
            }
            let contravariant = parameters
                .iter()
                .zip(wanted.iter())
                .all(|(parameter, wanted)| relate(table, wanted.type_id, parameter.type_id, seen));
            contravariant
                && (matches!(table.kind(*wanted_returns), TypeKind::Void)
                    || relate(table, *returns, *wanted_returns, seen))
        }
        _ => false,
    }
}

fn object_fits(
    table: &TypeTable,
    fields: &[Field],
    indexes: &[IndexSignature],
    wanted: &[Field],
    wanted_indexes: &[IndexSignature],
) -> bool {
    for want in wanted {
        match fields.iter().find(|field| field.name == want.name) {
            Some(field) => {
                if field.optional && !want.optional {
                    return false;
                }
                if field.type_id != want.type_id {
                    return false;
                }
            }
            None => {
                if !want.optional {
                    return false;
                }
            }
        }
    }
    for want in wanted_indexes {
        let covered = indexes
            .iter()
            .any(|index| index.key == want.key && index.value == want.value);
        if covered {
            continue;
        }
        let reaches = fields
            .iter()
            .filter(|field| key_covers(table, want.key, &field.name))
            .all(|field| field.type_id == want.value);
        if !reaches {
            return false;
        }
    }
    true
}

fn key_covers(table: &TypeTable, key: TypeId, name: &str) -> bool {
    match table.kind(key) {
        TypeKind::String => true,
        TypeKind::Number => name.parse::<f64>().is_ok(),
        _ => false,
    }
}

fn is_object_like(table: &TypeTable, id: TypeId) -> bool {
    matches!(
        table.kind(id),
        TypeKind::Array(_)
            | TypeKind::ReadonlyArray(_)
            | TypeKind::Promise(_)
            | TypeKind::Tuple { .. }
            | TypeKind::Object { .. }
            | TypeKind::Function { .. }
            | TypeKind::Overloaded(_)
            | TypeKind::Nominal(_)
    )
}
