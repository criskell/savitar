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
        UnaryOp::Not => Ok(table.intern(TypeKind::Boolean)),
        UnaryOp::TypeOf => Ok(table.intern(TypeKind::String)),
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
