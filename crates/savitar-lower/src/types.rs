use savitar_ir::{BinaryOp, UnaryOp};
use savitar_types::{Field, IndexSignature, TypeId, TypeKind, TypeTable};

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

#[must_use]
pub fn may_be_undefined(table: &TypeTable, id: TypeId) -> bool {
    match table.kind(id) {
        TypeKind::Never
        | TypeKind::Null
        | TypeKind::Boolean
        | TypeKind::Number
        | TypeKind::BigInt
        | TypeKind::String
        | TypeKind::Symbol
        | TypeKind::NonPrimitive
        | TypeKind::StringLiteral(_)
        | TypeKind::NumberLiteral(_)
        | TypeKind::BooleanLiteral(_)
        | TypeKind::Array(_)
        | TypeKind::ReadonlyArray(_)
        | TypeKind::Promise(_)
        | TypeKind::Tuple { .. }
        | TypeKind::Object { .. }
        | TypeKind::Function { .. }
        | TypeKind::Overloaded(_)
        | TypeKind::Constructor(_) => false,
        TypeKind::Union(members) => members
            .iter()
            .any(|&member| may_be_undefined(table, member)),
        _ => true,
    }
}

#[must_use]
pub fn without_member(table: &mut TypeTable, id: TypeId, dropped: &TypeKind) -> Option<TypeId> {
    if table.kind(id) == dropped {
        return None;
    }
    let TypeKind::Union(members) = table.kind(id).clone() else {
        return Some(id);
    };
    let kept: Vec<TypeId> = members
        .into_iter()
        .filter(|&member| table.kind(member) != dropped)
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(table.union(kept))
}

#[must_use]
pub fn holds_member(table: &TypeTable, id: TypeId, wanted: &TypeKind) -> bool {
    match table.kind(id) {
        TypeKind::Union(members) => members.iter().any(|&member| table.kind(member) == wanted),
        held => held == wanted,
    }
}

#[must_use]
pub fn narrowing_typeof_word(word: &str) -> bool {
    matches!(
        word,
        "string" | "number" | "boolean" | "bigint" | "symbol" | "undefined" | "object" | "function"
    )
}

#[must_use]
pub fn with_only_typeof(table: &mut TypeTable, id: TypeId, word: &str) -> Option<TypeId> {
    keeping(table, id, |table, member| {
        typeof_word(table, member).is_none_or(|held| held == word)
    })
}

#[must_use]
pub fn without_typeof(table: &mut TypeTable, id: TypeId, word: &str) -> Option<TypeId> {
    keeping(table, id, |table, member| {
        typeof_word(table, member) != Some(word)
    })
}

#[must_use]
pub fn with_only_discriminant(
    table: &mut TypeTable,
    id: TypeId,
    field: &str,
    literals: &[TypeKind],
) -> Option<TypeId> {
    keeping(table, id, |table, member| {
        match decided_field_kinds(table, member, field) {
            Some(kinds) => kinds.iter().any(|kind| literals.contains(kind)),
            None => true,
        }
    })
}

#[must_use]
pub fn without_discriminant(
    table: &mut TypeTable,
    id: TypeId,
    field: &str,
    literals: &[TypeKind],
) -> Option<TypeId> {
    keeping(table, id, |table, member| {
        match decided_field_kinds(table, member, field) {
            Some(kinds) => kinds.iter().any(|kind| !literals.contains(kind)),
            None => true,
        }
    })
}

#[must_use]
pub fn with_only_literals(
    table: &mut TypeTable,
    id: TypeId,
    literals: &[TypeKind],
) -> Option<TypeId> {
    keeping(table, id, |table, member| {
        literals
            .iter()
            .any(|literal| may_hold_literal(table, member, literal))
    })
}

#[must_use]
pub fn without_literals(
    table: &mut TypeTable,
    id: TypeId,
    literals: &[TypeKind],
) -> Option<TypeId> {
    keeping(table, id, |table, member| {
        !literals.contains(table.kind(member))
    })
}

#[must_use]
pub fn with_only_declaring(table: &mut TypeTable, id: TypeId, field: &str) -> Option<TypeId> {
    keeping(table, id, |table, member| {
        declares_field(table, member, field) != Some(false)
    })
}

#[must_use]
pub fn without_declaring(table: &mut TypeTable, id: TypeId, field: &str) -> Option<TypeId> {
    keeping(table, id, |table, member| {
        declares_field(table, member, field) != Some(true)
    })
}

fn declares_field(table: &TypeTable, member: TypeId, field: &str) -> Option<bool> {
    let TypeKind::Object { fields, indexes } = table.kind(member) else {
        return None;
    };
    if !indexes.is_empty() {
        return None;
    }
    match fields.iter().find(|one| one.name == field) {
        Some(found) if found.optional => None,
        Some(_) => Some(true),
        None => Some(false),
    }
}

#[must_use]
pub fn without_always_false(table: &mut TypeTable, id: TypeId) -> Option<TypeId> {
    keeping(table, id, |table, member| !is_always_false(table, member))
}

fn is_always_false(table: &TypeTable, id: TypeId) -> bool {
    match table.kind(id) {
        TypeKind::Undefined | TypeKind::Null => true,
        TypeKind::BooleanLiteral(held) => !held,
        TypeKind::NumberLiteral(bits) => f64::from_bits(*bits) == 0.0,
        TypeKind::StringLiteral(held) => held.is_empty(),
        _ => false,
    }
}

#[must_use]
pub fn label_literal(table: &TypeTable, label: TypeId) -> Option<TypeKind> {
    match table.kind(label) {
        kind @ (TypeKind::StringLiteral(_)
        | TypeKind::NumberLiteral(_)
        | TypeKind::BooleanLiteral(_)
        | TypeKind::Null
        | TypeKind::Undefined) => Some(kind.clone()),
        _ => None,
    }
}

fn may_hold_literal(table: &TypeTable, member: TypeId, literal: &TypeKind) -> bool {
    let held = table.kind(member);
    held == literal
        || matches!(
            (held, literal),
            (TypeKind::String, TypeKind::StringLiteral(_))
                | (TypeKind::Number, TypeKind::NumberLiteral(_))
                | (TypeKind::Boolean, TypeKind::BooleanLiteral(_))
        )
}

fn keeping(
    table: &mut TypeTable,
    id: TypeId,
    keep: impl Fn(&TypeTable, TypeId) -> bool,
) -> Option<TypeId> {
    let members = match table.kind(id).clone() {
        TypeKind::Union(members) => members,
        _ => vec![id],
    };
    let kept: Vec<TypeId> = members
        .into_iter()
        .filter(|&member| keep(table, member))
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(table.union(kept))
}

fn typeof_word(table: &TypeTable, id: TypeId) -> Option<&'static str> {
    match table.kind(id) {
        TypeKind::String | TypeKind::StringLiteral(_) => Some("string"),
        TypeKind::Number | TypeKind::NumberLiteral(_) => Some("number"),
        TypeKind::Boolean | TypeKind::BooleanLiteral(_) => Some("boolean"),
        TypeKind::BigInt => Some("bigint"),
        TypeKind::Symbol => Some("symbol"),
        TypeKind::Undefined => Some("undefined"),
        TypeKind::Null
        | TypeKind::Array(_)
        | TypeKind::ReadonlyArray(_)
        | TypeKind::Promise(_)
        | TypeKind::Tuple { .. }
        | TypeKind::Object { .. } => Some("object"),
        TypeKind::Function { .. } | TypeKind::Overloaded(_) | TypeKind::Constructor(_) => {
            Some("function")
        }
        _ => None,
    }
}

fn decided_field_kinds(table: &TypeTable, member: TypeId, field: &str) -> Option<Vec<TypeKind>> {
    let TypeKind::Object { fields, .. } = table.kind(member) else {
        return None;
    };
    let found = fields.iter().find(|one| one.name == field)?;
    if found.optional {
        return None;
    }
    let held = match table.kind(found.type_id) {
        TypeKind::Union(members) => members.clone(),
        _ => vec![found.type_id],
    };
    held.into_iter()
        .map(|one| match table.kind(one) {
            kind @ (TypeKind::StringLiteral(_)
            | TypeKind::NumberLiteral(_)
            | TypeKind::BooleanLiteral(_)) => Some(kind.clone()),
            _ => None,
        })
        .collect()
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
        BinaryOp::In => {
            if !is_textual(table, left) {
                return Err("`in` takes the name of a field on the left".to_string());
            }
            if !holds_fields(table, right) {
                return Err("`in` needs a value with fields on the right".to_string());
            }
            Ok(table.intern(TypeKind::Boolean))
        }
    }
}

fn holds_fields(table: &TypeTable, id: TypeId) -> bool {
    match table.kind(id) {
        TypeKind::Object { .. }
        | TypeKind::NonPrimitive
        | TypeKind::Array(_)
        | TypeKind::ReadonlyArray(_)
        | TypeKind::Tuple { .. }
        | TypeKind::Promise(_)
        | TypeKind::Function { .. }
        | TypeKind::Overloaded(_)
        | TypeKind::Constructor(_) => true,
        TypeKind::Union(members) => members.iter().all(|&member| holds_fields(table, member)),
        _ => false,
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

#[must_use]
pub fn is_union(table: &TypeTable, id: TypeId) -> bool {
    matches!(table.kind(id), TypeKind::Union(_))
}

pub fn is_shaped(table: &TypeTable, id: TypeId) -> bool {
    matches!(table.kind(id), TypeKind::Object { .. })
}

pub fn field_result(table: &mut TypeTable, object: TypeId, name: &str) -> Result<TypeId, String> {
    if let TypeKind::Union(members) = table.kind(object).clone() {
        let mut held = Vec::with_capacity(members.len());
        for member in members {
            let one = field_result(table, member, name)
                .map_err(|reason| format!("one member of this union answers nothing: {reason}"))?;
            held.push(one);
        }
        return Ok(table.union(held));
    }
    let TypeKind::Object { fields, indexes } = table.kind(object).clone() else {
        return Err(format!(
            "`{name}` is read from something with no declared shape"
        ));
    };
    let Some(field) = fields.into_iter().find(|field| field.name == name) else {
        return index_result(table, &indexes, name);
    };
    if field.optional {
        return Ok(table.optional(field.type_id));
    }
    Ok(field.type_id)
}

fn index_result(
    table: &mut TypeTable,
    indexes: &[IndexSignature],
    name: &str,
) -> Result<TypeId, String> {
    let key = table.intern(TypeKind::StringLiteral(name.to_string()));
    let matched = indexes
        .iter()
        .find(|signature| signature.key == key || key_accepts(table, signature.key, key));
    match matched {
        Some(signature) => Ok(table.optional(signature.value)),
        None => Err(format!("the shape declares no field `{name}`")),
    }
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
        TypeKind::Tuple { elements, .. } => {
            if let TypeKind::NumberLiteral(bits) = *table.kind(index) {
                let wanted = f64::from_bits(bits);
                return elements
                    .iter()
                    .enumerate()
                    .find_map(|(position, &element)| (position as f64 == wanted).then_some(element))
                    .ok_or_else(|| "this index falls outside the tuple".to_string());
            }
            if !is_numeric(table, index) {
                return Err("a tuple is indexed by a number".to_string());
            }
            let held = table.union(elements);
            Ok(table.optional(held))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writing {
    Plainly,
    WhileConstructing,
}

pub fn field_slots(
    table: &mut TypeTable,
    object: TypeId,
    name: &str,
    writing: Writing,
) -> Result<Vec<TypeId>, SlotRefusal> {
    let TypeKind::Union(members) = table.kind(object).clone() else {
        return field_slot(table, object, name, writing).map(|slot| vec![slot]);
    };
    let mut slots: Vec<TypeId> = Vec::with_capacity(members.len());
    for member in members {
        for slot in
            field_slots(table, member, name, writing).map_err(|refusal| in_a_member(&refusal))?
        {
            if !slots.contains(&slot) {
                slots.push(slot);
            }
        }
    }
    Ok(slots)
}

fn in_a_member(refusal: &SlotRefusal) -> SlotRefusal {
    match refusal {
        SlotRefusal::Shapeless(reason) => {
            SlotRefusal::Shapeless(format!("one member of this union takes no write: {reason}"))
        }
        SlotRefusal::Declined(reason) => {
            SlotRefusal::Declined(format!("one member of this union takes no write: {reason}"))
        }
    }
}

pub fn field_slot(
    table: &mut TypeTable,
    object: TypeId,
    name: &str,
    writing: Writing,
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
        if field.readonly && writing == Writing::Plainly {
            return Err(SlotRefusal::Declined(format!(
                "`{name}` is declared readonly and admits no write outside the constructor of its class"
            )));
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
                Some(signature) if signature.readonly => {
                    Err("a readonly index signature admits no write".to_string())
                }
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

pub struct Given {
    pub name: String,
    pub literal: Option<TypeId>,
}

#[must_use]
pub fn leaving_without_a_value_fits(table: &TypeTable, returns: TypeId) -> bool {
    matches!(table.kind(returns), TypeKind::Void) || table.admits_undefined(returns)
}

pub fn awaited(table: &TypeTable, declared: TypeId) -> Result<TypeId, String> {
    match table.kind(declared) {
        TypeKind::Promise(held) => Ok(*held),
        _ => Err(
            "an async function whose declared return is not a promise is not lowered yet"
                .to_string(),
        ),
    }
}

pub fn declared_return(table: &TypeTable, signature: TypeId) -> Result<TypeId, String> {
    match table.kind(signature) {
        TypeKind::Function { returns, .. } => Ok(*returns),
        TypeKind::Overloaded(_) => {
            Err("a function with more than one signature is not lowered yet".to_string())
        }
        _ => Err("the provider gives no signature for this function".to_string()),
    }
}

pub fn declared_parameter(
    table: &TypeTable,
    signature: TypeId,
    position: usize,
    defaulted: bool,
) -> Result<TypeId, String> {
    let TypeKind::Function { parameters, .. } = table.kind(signature) else {
        return Err("the provider gives no signature for this function".to_string());
    };
    let Some(parameter) = parameters.get(position) else {
        return Err("the signature declares no type for this parameter".to_string());
    };
    if parameter.rest {
        return Err("a rest parameter bound by a pattern is not lowered yet".to_string());
    }
    if parameter.optional && !defaulted {
        return Err(
            "an optional parameter bound by a pattern with no default is not lowered yet"
                .to_string(),
        );
    }
    Ok(parameter.type_id)
}

#[must_use]
pub fn contextual_fields(
    table: &TypeTable,
    expected: TypeId,
    given: &[Given],
) -> Option<Vec<Field>> {
    if let TypeKind::Object { fields, .. } = table.kind(expected) {
        return Some(fields.clone());
    }
    let TypeKind::Union(members) = table.kind(expected) else {
        return None;
    };
    let mut fitting = members
        .iter()
        .filter_map(|&member| member_taking(table, member, given));
    let only = fitting.next()?;
    fitting.next().is_none().then_some(only)
}

fn member_taking(table: &TypeTable, member: TypeId, given: &[Given]) -> Option<Vec<Field>> {
    let TypeKind::Object { fields, .. } = table.kind(member) else {
        return None;
    };
    let every_required_field_is_given = fields
        .iter()
        .filter(|field| !field.optional)
        .all(|field| given.iter().any(|one| one.name == field.name));
    if !every_required_field_is_given {
        return None;
    }
    for one in given {
        let declared = fields.iter().find(|field| field.name == one.name)?;
        if let Some(literal) = one.literal
            && !savitar_types::assignable(table, literal, declared.type_id)
        {
            return None;
        }
    }
    Some(fields.clone())
}

#[must_use]
pub fn contextual_elements(
    table: &TypeTable,
    expected: TypeId,
    count: usize,
) -> Option<(Vec<TypeId>, bool)> {
    match table.kind(expected) {
        TypeKind::Tuple { elements, readonly } if elements.len() == count => {
            Some((elements.clone(), *readonly))
        }
        _ => None,
    }
}

#[must_use]
pub fn takes_type_arguments(table: &TypeTable, signature: TypeId) -> bool {
    matches!(
        table.kind(signature),
        TypeKind::Function {
            type_parameters,
            ..
        } if !type_parameters.is_empty()
    )
}

pub fn object_rest_of(
    table: &mut TypeTable,
    source: TypeId,
    taken: &[String],
) -> Result<TypeId, String> {
    if let TypeKind::Union(members) = table.kind(source).clone() {
        let mut held = Vec::with_capacity(members.len());
        for member in members {
            let one = object_rest_of(table, member, taken).map_err(|reason| {
                format!("one member of this union keeps no rest of its own: {reason}")
            })?;
            held.push(one);
        }
        return Ok(table.union(held));
    }
    let TypeKind::Object { fields, indexes } = table.kind(source).clone() else {
        return Err("a rest in an object pattern needs a declared shape to take from".to_string());
    };
    if !indexes.is_empty() {
        return Err(
            "a rest over a shape with an index signature is not lowered yet, because the keys it carries are not named".to_string(),
        );
    }
    let kept: Vec<Field> = fields
        .into_iter()
        .filter(|field| !taken.contains(&field.name))
        .collect();
    if let Some(method) = kept.iter().find(|field| field.method) {
        return Err(format!(
            "`{}` is a method, and a rest cannot show a method is copied",
            method.name
        ));
    }
    Ok(table.object(kept))
}

pub fn rest_of(table: &mut TypeTable, source: TypeId, from: u32) -> Result<TypeId, String> {
    match table.kind(source).clone() {
        TypeKind::Array(element) | TypeKind::ReadonlyArray(element) => {
            Ok(table.intern(TypeKind::Array(element)))
        }
        TypeKind::Tuple { elements, .. } => {
            let kept: Vec<TypeId> = elements.into_iter().skip(from as usize).collect();
            Ok(table.tuple(kept, false))
        }
        _ => Err("a rest binding needs an array or a tuple to take from".to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fitted {
    pub slots: Vec<TypeId>,
    pub returns: TypeId,
}

pub fn callable_forms(table: &TypeTable, signature: TypeId) -> Result<Vec<TypeId>, String> {
    match table.kind(signature) {
        TypeKind::Function { .. } => Ok(vec![signature]),
        TypeKind::Overloaded(forms) => Ok(forms.clone()),
        _ => Err("this value is not callable".to_string()),
    }
}

#[must_use]
pub fn fittings_agree(table: &TypeTable, held: &Fitted, other: &Fitted) -> bool {
    held.slots.len() == other.slots.len()
        && alike(table, held.returns, other.returns)
        && held
            .slots
            .iter()
            .zip(other.slots.iter())
            .all(|(&left, &right)| alike(table, left, right))
}

fn alike(table: &TypeTable, left: TypeId, right: TypeId) -> bool {
    savitar_types::assignable(table, left, right) && savitar_types::assignable(table, right, left)
}

pub fn constructible_forms(table: &TypeTable, signature: TypeId) -> Result<Vec<TypeId>, String> {
    match table.kind(signature) {
        TypeKind::Constructor(forms) => Ok(forms.clone()),
        _ => Err("this value declares no way to construct it".to_string()),
    }
}

pub fn fits_call_with(
    table: &mut TypeTable,
    form: TypeId,
    held: &[TypeId],
    written: &[TypeId],
) -> Result<Fitted, String> {
    let wanted = argument_types(table, form, held.len())?;
    let returns = call_result(table, form)?;
    let declared = match table.kind(form) {
        TypeKind::Function {
            type_parameters, ..
        } => type_parameters.clone(),
        _ => Vec::new(),
    };
    let (slots, returns) = if !written.is_empty() {
        if declared.len() != written.len() {
            return Err(
                "this form declares a different number of type parameters than the call writes"
                    .to_string(),
            );
        }
        let bindings: savitar_types::Bindings = declared
            .iter()
            .copied()
            .zip(written.iter().copied())
            .collect();
        let mut slots = Vec::with_capacity(wanted.len());
        for &slot in &wanted {
            slots.push(savitar_types::substitute(table, slot, &bindings)?);
        }
        (slots, savitar_types::substitute(table, returns, &bindings)?)
    } else if takes_type_arguments(table, form) {
        let mut bindings = savitar_types::Bindings::new();
        for (position, &given) in held.iter().enumerate() {
            savitar_types::infer(table, wanted[position], given, &mut bindings)?;
        }
        let mut slots = Vec::with_capacity(wanted.len());
        for &slot in &wanted {
            slots.push(savitar_types::substitute(table, slot, &bindings)?);
        }
        (slots, savitar_types::substitute(table, returns, &bindings)?)
    } else {
        (wanted, returns)
    };
    for (position, &given) in held.iter().enumerate() {
        if !savitar_types::assignable(table, given, slots[position]) {
            return Err(format!(
                "argument {} does not fit the parameter it is passed to",
                position + 1
            ));
        }
    }
    Ok(Fitted { slots, returns })
}

pub fn argument_types(
    table: &TypeTable,
    callee: TypeId,
    given: usize,
) -> Result<Vec<TypeId>, String> {
    let TypeKind::Function { parameters, .. } = table.kind(callee) else {
        return Err("this value is not callable".to_string());
    };
    let rest = parameters.last().filter(|parameter| parameter.rest);
    let fixed = parameters.len() - usize::from(rest.is_some());
    if given < fixed {
        let left_out = parameters[given..fixed]
            .iter()
            .find(|parameter| !parameter.optional);
        if left_out.is_some() {
            return Err(
                "this call leaves out an argument the signature does not mark optional".to_string(),
            );
        }
    }
    let Some(rest) = rest else {
        if given > parameters.len() {
            return Err("this call passes more arguments than the signature takes".to_string());
        }
        return Ok(parameters
            .iter()
            .take(given)
            .map(|parameter| parameter.type_id)
            .collect());
    };
    let (TypeKind::Array(element) | TypeKind::ReadonlyArray(element)) = table.kind(rest.type_id)
    else {
        return Err("a rest parameter whose type is not an array is not lowered yet".to_string());
    };
    let mut wanted: Vec<TypeId> = parameters
        .iter()
        .take(fixed)
        .map(|parameter| parameter.type_id)
        .collect();
    wanted.resize(given, *element);
    Ok(wanted)
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

#[must_use]
pub fn exhausted(table: &TypeTable, discriminant: TypeId, labels: &[TypeId]) -> bool {
    let members = match table.kind(discriminant) {
        TypeKind::Union(members) => members.clone(),
        _ => vec![discriminant],
    };
    let covered: Vec<&TypeKind> = labels.iter().map(|&id| table.kind(id)).collect();
    !members.is_empty()
        && members
            .iter()
            .all(|&member| member_is_covered(table, member, &covered))
}

fn member_is_covered(table: &TypeTable, member: TypeId, covered: &[&TypeKind]) -> bool {
    match table.kind(member) {
        TypeKind::Boolean => {
            covered.contains(&&TypeKind::BooleanLiteral(true))
                && covered.contains(&&TypeKind::BooleanLiteral(false))
        }
        kind @ (TypeKind::StringLiteral(_)
        | TypeKind::NumberLiteral(_)
        | TypeKind::BooleanLiteral(_)
        | TypeKind::Null
        | TypeKind::Undefined) => covered.contains(&kind),
        _ => false,
    }
}
