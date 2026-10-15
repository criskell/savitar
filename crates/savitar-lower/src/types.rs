use savitar_ir::{BinaryOp, UnaryOp};
use savitar_types::{TypeId, TypeKind, TypeTable};

#[must_use]
pub fn is_numeric(table: &TypeTable, id: TypeId) -> bool {
    matches!(
        table.kind(id),
        TypeKind::Number | TypeKind::NumberLiteral(_)
    )
}

#[must_use]
pub fn is_textual(table: &TypeTable, id: TypeId) -> bool {
    matches!(
        table.kind(id),
        TypeKind::String | TypeKind::StringLiteral(_)
    )
}

#[must_use]
pub fn is_text_convertible(table: &TypeTable, id: TypeId) -> bool {
    match table.kind(id) {
        TypeKind::Null
        | TypeKind::Undefined
        | TypeKind::Boolean
        | TypeKind::Number
        | TypeKind::BigInt
        | TypeKind::String
        | TypeKind::StringLiteral(_)
        | TypeKind::NumberLiteral(_)
        | TypeKind::BooleanLiteral(_) => true,
        TypeKind::Union(members) => members
            .iter()
            .all(|&member| is_text_convertible(table, member)),
        _ => false,
    }
}

#[must_use]
pub fn without_nullish(table: &mut TypeTable, id: TypeId) -> Option<TypeId> {
    if is_nullish(table, id) {
        return None;
    }
    let TypeKind::Union(members) = table.kind(id).clone() else {
        return Some(id);
    };
    let kept: Vec<TypeId> = members
        .into_iter()
        .filter(|&member| !is_nullish(table, member))
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(table.union(kept))
}

fn is_nullish(table: &TypeTable, id: TypeId) -> bool {
    matches!(table.kind(id), TypeKind::Null | TypeKind::Undefined)
}

pub fn binary_result(
    table: &mut TypeTable,
    op: BinaryOp,
    left: TypeId,
    right: TypeId,
) -> Result<TypeId, String> {
    match op {
        BinaryOp::Add => {
            if is_numeric(table, left) && is_numeric(table, right) {
                Ok(table.intern(TypeKind::Number))
            } else if is_textual(table, left) && is_textual(table, right) {
                Ok(table.intern(TypeKind::String))
            } else {
                Err("`+` only adds two numbers or joins two strings".to_string())
            }
        }
        BinaryOp::Subtract
        | BinaryOp::Multiply
        | BinaryOp::Divide
        | BinaryOp::Remainder
        | BinaryOp::Less
        | BinaryOp::LessOrEqual
        | BinaryOp::Greater
        | BinaryOp::GreaterOrEqual => {
            if !is_numeric(table, left) || !is_numeric(table, right) {
                return Err("this operator needs two numbers".to_string());
            }
            Ok(match op {
                BinaryOp::Less
                | BinaryOp::LessOrEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterOrEqual => table.intern(TypeKind::Boolean),
                _ => table.intern(TypeKind::Number),
            })
        }
        BinaryOp::StrictEqual | BinaryOp::StrictNotEqual => Ok(table.intern(TypeKind::Boolean)),
    }
}

pub fn unary_result(table: &mut TypeTable, op: UnaryOp, operand: TypeId) -> Result<TypeId, String> {
    match op {
        UnaryOp::Negate => {
            if is_numeric(table, operand) {
                Ok(table.intern(TypeKind::Number))
            } else {
                Err("`-` needs a number".to_string())
            }
        }
        UnaryOp::Not | UnaryOp::IsNullish => Ok(table.intern(TypeKind::Boolean)),
        UnaryOp::TypeOf => Ok(table.intern(TypeKind::String)),
        UnaryOp::ToText => {
            if is_text_convertible(table, operand) {
                Ok(table.intern(TypeKind::String))
            } else {
                Err(
                    "turning a value that is not a primitive into text is not lowered yet"
                        .to_string(),
                )
            }
        }
    }
}

pub fn field_result(table: &mut TypeTable, object: TypeId, name: &str) -> Result<TypeId, String> {
    let TypeKind::Object { fields, .. } = table.kind(object).clone() else {
        return Err(format!(
            "`{name}` is read from something with no declared shape"
        ));
    };
    let Some(field) = fields.into_iter().find(|field| field.name == name) else {
        return Err(format!("the shape declares no field `{name}`"));
    };
    if field.optional {
        return Ok(table.optional(field.type_id));
    }
    Ok(field.type_id)
}

pub fn element_result(
    table: &mut TypeTable,
    array: TypeId,
    index: TypeId,
) -> Result<TypeId, String> {
    match table.kind(array).clone() {
        TypeKind::Array(element) | TypeKind::ReadonlyArray(element) => {
            if !is_numeric(table, index) {
                return Err("an array is indexed by a number".to_string());
            }
            Ok(table.optional(element))
        }
        TypeKind::Object { indexes, .. } if !indexes.is_empty() => {
            let matched = indexes.iter().find(|signature| {
                signature.key == index || key_accepts(table, signature.key, index)
            });
            match matched {
                Some(signature) => Ok(table.optional(signature.value)),
                None => Err("no index signature accepts this key".to_string()),
            }
        }
        _ => Err("this value has no element to read".to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotRefusal {
    Shapeless(String),
    Declined(String),
}

pub fn field_slot(
    table: &mut TypeTable,
    object: TypeId,
    name: &str,
) -> Result<TypeId, SlotRefusal> {
    let TypeKind::Object { fields, .. } = table.kind(object).clone() else {
        return Err(SlotRefusal::Shapeless(format!(
            "`{name}` is written on something with no declared shape"
        )));
    };
    if let Some(field) = fields.iter().find(|field| field.name == name) {
        if field.method {
            return Err(SlotRefusal::Declined(format!(
                "`{name}` is a method, and writing over a method is not lowered yet"
            )));
        }
        if field.optional {
            return Ok(table.optional(field.type_id));
        }
        return Ok(field.type_id);
    }
    Err(SlotRefusal::Declined(format!(
        "the shape declares no field `{name}`"
    )))
}

pub fn element_slot(table: &mut TypeTable, array: TypeId, index: TypeId) -> Result<TypeId, String> {
    match table.kind(array).clone() {
        TypeKind::Array(element) => {
            if !is_numeric(table, index) {
                return Err("an array is indexed by a number".to_string());
            }
            Ok(element)
        }
        TypeKind::ReadonlyArray(_) => {
            Err("a readonly array admits no write to its elements".to_string())
        }
        TypeKind::Tuple { readonly: true, .. } => {
            Err("a readonly tuple admits no write to its elements".to_string())
        }
        TypeKind::Tuple {
            elements,
            readonly: false,
        } => {
            let TypeKind::NumberLiteral(bits) = table.kind(index) else {
                return Err(
                    "a tuple is written only at an index the compiler knows exactly".to_string(),
                );
            };
            let wanted = f64::from_bits(*bits);
            elements
                .iter()
                .enumerate()
                .find_map(|(position, &element)| (position as f64 == wanted).then_some(element))
                .ok_or_else(|| "this index falls outside the tuple".to_string())
        }
        TypeKind::Object { indexes, .. } if !indexes.is_empty() => {
            let matched = indexes.iter().find(|signature| {
                signature.key == index || key_accepts(table, signature.key, index)
            });
            match matched {
                Some(signature) => Ok(signature.value),
                None => Err("no index signature accepts this key".to_string()),
            }
        }
        _ => Err("this value has no element to write".to_string()),
    }
}

fn key_accepts(table: &TypeTable, declared: TypeId, given: TypeId) -> bool {
    match table.kind(declared) {
        TypeKind::String => is_textual(table, given),
        TypeKind::Number => is_numeric(table, given),
        _ => false,
    }
}

pub fn call_result(table: &TypeTable, callee: TypeId) -> Result<TypeId, String> {
    match table.kind(callee) {
        TypeKind::Function { returns, .. } => Ok(*returns),
        TypeKind::Overloaded(_) => Err("an overloaded call is not resolved yet".to_string()),
        _ => Err("this value is not callable".to_string()),
    }
}

pub fn widen(table: &mut TypeTable, id: TypeId) -> TypeId {
    match table.kind(id).clone() {
        TypeKind::NumberLiteral(_) => table.intern(TypeKind::Number),
        TypeKind::StringLiteral(_) => table.intern(TypeKind::String),
        TypeKind::BooleanLiteral(_) => table.intern(TypeKind::Boolean),
        TypeKind::Union(members) => {
            let widened: Vec<TypeId> = members
                .into_iter()
                .map(|member| widen(table, member))
                .collect();
            table.union(widened)
        }
        _ => id,
    }
}
