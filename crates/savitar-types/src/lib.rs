use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(u32);

impl TypeId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Field {
    pub name: String,
    pub type_id: TypeId,
    pub optional: bool,
    pub method: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Parameter {
    pub type_id: TypeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IndexSignature {
    pub key: TypeId,
    pub value: TypeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeKind {
    Never,
    Null,
    Undefined,
    Void,
    Boolean,
    Number,
    BigInt,
    String,
    Symbol,
    Unknown,
    StringLiteral(String),
    NumberLiteral(u64),
    BooleanLiteral(bool),
    Array(TypeId),
    ReadonlyArray(TypeId),
    Tuple {
        elements: Vec<TypeId>,
        readonly: bool,
    },
    Object {
        fields: Vec<Field>,
        indexes: Vec<IndexSignature>,
    },
    Function {
        parameters: Vec<Parameter>,
        returns: TypeId,
    },
    Union(Vec<TypeId>),
    Nominal(NominalId),
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NominalId(u32);

#[derive(Debug, Default)]
pub struct TypeTable {
    kinds: Vec<TypeKind>,
    interned: HashMap<TypeKind, TypeId>,
    nominal_names: Vec<String>,
}

impl TypeTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reserve(&mut self) -> TypeId {
        let id = TypeId(u32::try_from(self.kinds.len()).expect("type table overflow"));
        self.kinds.push(TypeKind::Unresolved);
        id
    }

    pub fn fill(&mut self, id: TypeId, kind: TypeKind) {
        self.kinds[id.0 as usize] = kind;
    }

    pub fn intern(&mut self, kind: TypeKind) -> TypeId {
        if let Some(&id) = self.interned.get(&kind) {
            return id;
        }
        let id = TypeId(u32::try_from(self.kinds.len()).expect("type table overflow"));
        self.kinds.push(kind.clone());
        self.interned.insert(kind, id);
        id
    }

    #[must_use]
    pub fn kind(&self, id: TypeId) -> &TypeKind {
        &self.kinds[id.0 as usize]
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    pub fn declare_nominal(&mut self, name: impl Into<String>) -> NominalId {
        let id = NominalId(u32::try_from(self.nominal_names.len()).expect("nominal overflow"));
        self.nominal_names.push(name.into());
        id
    }

    #[must_use]
    pub fn nominal_name(&self, id: NominalId) -> &str {
        &self.nominal_names[id.0 as usize]
    }

    pub fn union(&mut self, mut members: Vec<TypeId>) -> TypeId {
        members.sort_unstable();
        members.dedup();
        match members.len() {
            0 => self.intern(TypeKind::Never),
            1 => members[0],
            _ => self.intern(TypeKind::Union(members)),
        }
    }

    pub fn number_literal(&mut self, value: f64) -> TypeId {
        self.intern(TypeKind::NumberLiteral(value.to_bits()))
    }

    pub fn object(&mut self, fields: Vec<Field>) -> TypeId {
        self.object_with_indexes(fields, Vec::new())
    }

    pub fn object_with_indexes(
        &mut self,
        mut fields: Vec<Field>,
        mut indexes: Vec<IndexSignature>,
    ) -> TypeId {
        fields.sort_by(|left, right| left.name.cmp(&right.name));
        indexes.sort_by_key(|index| (index.key, index.value));
        self.intern(TypeKind::Object { fields, indexes })
    }

    #[must_use]
    pub fn admits_undefined(&self, id: TypeId) -> bool {
        match self.kind(id) {
            TypeKind::Undefined | TypeKind::Unknown => true,
            TypeKind::Union(members) => members.iter().any(|&member| self.admits_undefined(member)),
            _ => false,
        }
    }

    pub fn tuple(&mut self, elements: Vec<TypeId>, readonly: bool) -> TypeId {
        self.intern(TypeKind::Tuple { elements, readonly })
    }

    pub fn function(&mut self, parameters: Vec<Parameter>, returns: TypeId) -> TypeId {
        self.intern(TypeKind::Function {
            parameters,
            returns,
        })
    }

    pub fn optional(&mut self, inner: TypeId) -> TypeId {
        let undefined = self.intern(TypeKind::Undefined);
        self.union(vec![inner, undefined])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_stable_for_equal_kinds() {
        let mut table = TypeTable::new();
        let a = table.intern(TypeKind::Number);
        let b = table.intern(TypeKind::Number);
        assert_eq!(a, b);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn union_of_one_member_collapses() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        assert_eq!(table.union(vec![number, number]), number);
    }

    #[test]
    fn empty_union_is_never() {
        let mut table = TypeTable::new();
        let id = table.union(vec![]);
        assert_eq!(table.kind(id), &TypeKind::Never);
    }

    #[test]
    fn optional_number_is_a_union_with_undefined() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let undefined = table.intern(TypeKind::Undefined);
        let id = table.optional(number);
        let TypeKind::Union(members) = table.kind(id).clone() else {
            panic!("expected a union, got {:?}", table.kind(id));
        };
        assert!(members.contains(&number));
        assert!(members.contains(&undefined));
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn object_field_order_does_not_change_type_identity() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let text = table.intern(TypeKind::String);
        let x = Field {
            name: "x".to_string(),
            type_id: number,
            optional: false,
            method: false,
        };
        let y = Field {
            name: "y".to_string(),
            type_id: text,
            optional: true,
            method: false,
        };
        assert_eq!(
            table.object(vec![x.clone(), y.clone()]),
            table.object(vec![y, x])
        );
    }

    #[test]
    fn an_index_signature_changes_type_identity() {
        let mut table = TypeTable::new();
        let text = table.intern(TypeKind::String);
        let number = table.intern(TypeKind::Number);
        let plain = table.object(vec![]);
        let indexed = table.object_with_indexes(
            vec![],
            vec![IndexSignature {
                key: text,
                value: number,
            }],
        );
        assert_ne!(plain, indexed);
    }

    #[test]
    fn index_signature_order_does_not_change_type_identity() {
        let mut table = TypeTable::new();
        let text = table.intern(TypeKind::String);
        let number = table.intern(TypeKind::Number);
        let by_text = IndexSignature {
            key: text,
            value: number,
        };
        let by_number = IndexSignature {
            key: number,
            value: text,
        };
        assert_eq!(
            table.object_with_indexes(vec![], vec![by_text.clone(), by_number.clone()]),
            table.object_with_indexes(vec![], vec![by_number, by_text])
        );
    }

    #[test]
    fn an_optional_field_is_a_different_type() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let required = table.object(vec![Field {
            name: "x".to_string(),
            type_id: number,
            optional: false,
            method: false,
        }]);
        let optional = table.object(vec![Field {
            name: "x".to_string(),
            type_id: number,
            optional: true,
            method: false,
        }]);
        assert_ne!(required, optional);
    }

    #[test]
    fn a_method_is_a_different_type_from_a_field_of_the_same_function_type() {
        let mut table = TypeTable::new();
        let void = table.intern(TypeKind::Void);
        let signature = table.function(vec![], void);
        let as_field = table.object(vec![Field {
            name: "run".to_string(),
            type_id: signature,
            optional: false,
            method: false,
        }]);
        let as_method = table.object(vec![Field {
            name: "run".to_string(),
            type_id: signature,
            optional: false,
            method: true,
        }]);
        assert_ne!(as_field, as_method);
    }

    #[test]
    fn parameter_order_is_part_of_a_function_type() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let text = table.intern(TypeKind::String);
        let void = table.intern(TypeKind::Void);
        let left = Parameter { type_id: number };
        let right = Parameter { type_id: text };
        assert_ne!(
            table.function(vec![left.clone(), right.clone()], void),
            table.function(vec![right, left], void)
        );
    }

    #[test]
    fn element_order_is_part_of_a_tuple_type() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let text = table.intern(TypeKind::String);
        assert_ne!(
            table.tuple(vec![number, text], false),
            table.tuple(vec![text, number], false)
        );
    }

    #[test]
    fn a_readonly_tuple_is_a_different_type_from_a_mutable_one() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        assert_ne!(
            table.tuple(vec![number], false),
            table.tuple(vec![number], true)
        );
    }

    #[test]
    fn a_tuple_of_one_element_is_not_an_array_of_it() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let array = table.intern(TypeKind::Array(number));
        assert_ne!(table.tuple(vec![number], false), array);
    }

    #[test]
    fn a_repeated_element_is_not_deduplicated_in_a_tuple() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let pair = table.tuple(vec![number, number], false);
        let TypeKind::Tuple { elements, readonly } = table.kind(pair).clone() else {
            panic!("expected a tuple, got {:?}", table.kind(pair));
        };
        assert_eq!(elements, vec![number, number]);
        assert!(!readonly);
    }

    #[test]
    fn an_empty_tuple_has_its_own_identity() {
        let mut table = TypeTable::new();
        let empty = table.tuple(vec![], false);
        let readonly_empty = table.tuple(vec![], true);
        let nothing = table.object(vec![]);
        assert_ne!(empty, readonly_empty);
        assert_ne!(empty, nothing);
    }

    #[test]
    fn a_tuple_does_not_admit_undefined_even_when_an_element_does() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let maybe = table.optional(number);
        let pair = table.tuple(vec![number, maybe], false);
        assert!(!table.admits_undefined(pair));
    }

    #[test]
    fn a_reserved_slot_starts_unresolved_and_can_be_filled_later() {
        let mut table = TypeTable::new();
        let slot = table.reserve();
        assert_eq!(table.kind(slot), &TypeKind::Unresolved);
        let number = table.intern(TypeKind::Number);
        table.fill(slot, TypeKind::Array(slot));
        assert_eq!(table.kind(slot), &TypeKind::Array(slot));
        assert_ne!(slot, number);
    }

    #[test]
    fn a_reserved_slot_is_never_handed_out_by_interning() {
        let mut table = TypeTable::new();
        let slot = table.reserve();
        table.fill(slot, TypeKind::Number);
        let interned = table.intern(TypeKind::Number);
        assert_ne!(
            slot, interned,
            "a filled slot keeps its own identity, so recursion cannot be deduplicated"
        );
    }

    #[test]
    fn an_optional_type_admits_undefined_and_a_plain_one_does_not() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        assert!(!table.admits_undefined(number));
        let optional = table.optional(number);
        assert!(table.admits_undefined(optional));
        let undefined = table.intern(TypeKind::Undefined);
        assert!(table.admits_undefined(undefined));
    }

    #[test]
    fn nominal_ids_keep_their_names() {
        let mut table = TypeTable::new();
        let point = table.declare_nominal("Point");
        let user = table.declare_nominal("User");
        assert_eq!(table.nominal_name(point), "Point");
        assert_eq!(table.nominal_name(user), "User");
        assert_ne!(point, user);
    }
}
