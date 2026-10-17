mod assign;
mod divergence;
mod generic;

use std::collections::HashMap;

pub use assign::assignable;
pub use divergence::{Divergence, INTENTIONAL_DIVERGENCES, is_registered};
pub use generic::{Bindings, infer, mentions_a_type_parameter, substitute};

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
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Parameter {
    pub type_id: TypeId,
    pub rest: bool,
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IndexSignature {
    pub key: TypeId,
    pub value: TypeId,
    pub readonly: bool,
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
    NonPrimitive,
    StringLiteral(String),
    NumberLiteral(u64),
    BooleanLiteral(bool),
    Array(TypeId),
    ReadonlyArray(TypeId),
    Promise(TypeId),
    Tuple {
        elements: Vec<TypeId>,
        readonly: bool,
    },
    Object {
        fields: Vec<Field>,
        indexes: Vec<IndexSignature>,
    },
    Function {
        type_parameters: Vec<TypeId>,
        parameters: Vec<Parameter>,
        returns: TypeId,
    },
    Overloaded(Vec<TypeId>),
    Constructor(Vec<TypeId>),
    Union(Vec<TypeId>),
    Nominal(NominalId),
    TypeParameter(u32),
    Conditional {
        check: TypeId,
        bound: TypeId,
        when_true: TypeId,
        when_false: TypeId,
    },
    IndexedAccess {
        object: TypeId,
        index: TypeId,
    },
    Deferred(NominalId),
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NominalId(u32);

fn without_repeats(forms: Vec<TypeId>) -> Vec<TypeId> {
    let mut kept: Vec<TypeId> = Vec::with_capacity(forms.len());
    for form in forms {
        if !kept.contains(&form) {
            kept.push(form);
        }
    }
    kept
}

#[derive(Debug, Default)]
pub struct TypeTable {
    kinds: Vec<TypeKind>,
    interned: HashMap<TypeKind, TypeId>,
    nominal_names: Vec<String>,
    type_parameters: u32,
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

    pub fn union(&mut self, members: Vec<TypeId>) -> TypeId {
        let mut members = self.reduced(self.flattened(members));
        members.sort_unstable();
        members.dedup();
        match members.len() {
            0 => self.intern(TypeKind::Never),
            1 => members[0],
            _ => self.intern(TypeKind::Union(members)),
        }
    }

    fn flattened(&self, members: Vec<TypeId>) -> Vec<TypeId> {
        let mut flat = Vec::with_capacity(members.len());
        for member in members {
            match self.kind(member) {
                TypeKind::Union(inner) => flat.extend(inner.iter().copied()),
                TypeKind::Never => {}
                _ => flat.push(member),
            }
        }
        flat
    }

    fn reduced(&self, members: Vec<TypeId>) -> Vec<TypeId> {
        if members
            .iter()
            .any(|&member| matches!(self.kind(member), TypeKind::Unknown))
        {
            return vec![
                members
                    .into_iter()
                    .find(|&member| matches!(self.kind(member), TypeKind::Unknown))
                    .expect("the unknown member was just found"),
            ];
        }
        let holds = |wanted: &TypeKind| members.iter().any(|&member| self.kind(member) == wanted);
        let has_number = holds(&TypeKind::Number);
        let has_string = holds(&TypeKind::String);
        let has_boolean = holds(&TypeKind::Boolean);
        members
            .into_iter()
            .filter(|&member| match self.kind(member) {
                TypeKind::NumberLiteral(_) => !has_number,
                TypeKind::StringLiteral(_) => !has_string,
                TypeKind::BooleanLiteral(_) => !has_boolean,
                _ => true,
            })
            .collect()
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
        indexes.sort_by_key(|index| (index.key, index.value, index.readonly));
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

    pub fn overloaded(&mut self, forms: Vec<TypeId>) -> TypeId {
        let forms = without_repeats(forms);
        if let [only] = forms.as_slice() {
            return *only;
        }
        self.intern(TypeKind::Overloaded(forms))
    }

    pub fn constructor(&mut self, forms: Vec<TypeId>) -> TypeId {
        self.intern(TypeKind::Constructor(without_repeats(forms)))
    }

    pub fn fresh_type_parameter(&mut self) -> TypeId {
        let ordinal = self.type_parameters;
        self.type_parameters += 1;
        self.intern(TypeKind::TypeParameter(ordinal))
    }

    pub fn function(&mut self, parameters: Vec<Parameter>, returns: TypeId) -> TypeId {
        self.generic_function(Vec::new(), parameters, returns)
    }

    pub fn generic_function(
        &mut self,
        type_parameters: Vec<TypeId>,
        parameters: Vec<Parameter>,
        returns: TypeId,
    ) -> TypeId {
        self.intern(TypeKind::Function {
            type_parameters,
            parameters,
            returns,
        })
    }

    pub fn conditional(
        &mut self,
        check: TypeId,
        bound: TypeId,
        when_true: TypeId,
        when_false: TypeId,
    ) -> TypeId {
        self.intern(TypeKind::Conditional {
            check,
            bound,
            when_true,
            when_false,
        })
    }

    pub fn deferred(&mut self, name: impl Into<String>) -> TypeId {
        let named = self.declare_nominal(name);
        self.intern(TypeKind::Deferred(named))
    }

    pub fn indexed_access(&mut self, object: TypeId, index: TypeId) -> TypeId {
        self.intern(TypeKind::IndexedAccess { object, index })
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
            readonly: false,
        };
        let y = Field {
            name: "y".to_string(),
            type_id: text,
            optional: true,
            method: false,
            readonly: false,
        };
        assert_eq!(
            table.object(vec![x.clone(), y.clone()]),
            table.object(vec![y, x])
        );
    }

    #[test]
    fn a_promise_is_not_an_array_of_the_same_element() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        assert_ne!(
            table.intern(TypeKind::Promise(number)),
            table.intern(TypeKind::Array(number))
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
                readonly: false,
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
            readonly: false,
        };
        let by_number = IndexSignature {
            key: number,
            value: text,
            readonly: false,
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
            readonly: false,
        }]);
        let optional = table.object(vec![Field {
            name: "x".to_string(),
            type_id: number,
            optional: true,
            method: false,
            readonly: false,
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
            readonly: false,
        }]);
        let as_method = table.object(vec![Field {
            name: "run".to_string(),
            type_id: signature,
            optional: false,
            method: true,
            readonly: false,
        }]);
        assert_ne!(as_field, as_method);
    }

    #[test]
    fn overload_order_is_part_of_the_type_and_one_form_collapses() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let text = table.intern(TypeKind::String);
        let from_number = table.function(
            vec![Parameter {
                type_id: number,
                rest: false,
                optional: false,
            }],
            number,
        );
        let from_text = table.function(
            vec![Parameter {
                type_id: text,
                rest: false,
                optional: false,
            }],
            number,
        );
        assert_eq!(table.overloaded(vec![from_number]), from_number);
        assert_ne!(
            table.overloaded(vec![from_number, from_text]),
            table.overloaded(vec![from_text, from_number])
        );
    }

    #[test]
    fn each_type_parameter_gets_its_own_identity() {
        let mut table = TypeTable::new();
        let first = table.fresh_type_parameter();
        let second = table.fresh_type_parameter();
        assert_ne!(
            first, second,
            "two declared parameters are distinct even when both are written `T`"
        );
    }

    #[test]
    fn a_generic_function_differs_from_the_same_shape_without_parameters() {
        let mut table = TypeTable::new();
        let parameter = table.fresh_type_parameter();
        let plain = table.function(
            vec![Parameter {
                type_id: parameter,
                rest: false,
                optional: false,
            }],
            parameter,
        );
        let generic = table.generic_function(
            vec![parameter],
            vec![Parameter {
                type_id: parameter,
                rest: false,
                optional: false,
            }],
            parameter,
        );
        assert_ne!(plain, generic);
    }

    #[test]
    fn parameter_order_is_part_of_a_function_type() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let text = table.intern(TypeKind::String);
        let void = table.intern(TypeKind::Void);
        let left = Parameter {
            type_id: number,
            rest: false,
            optional: false,
        };
        let right = Parameter {
            type_id: text,
            rest: false,
            optional: false,
        };
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

    #[test]
    fn a_union_of_a_union_stays_flat() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let text = table.intern(TypeKind::String);
        let undefined = table.intern(TypeKind::Undefined);
        let maybe = table.union(vec![number, undefined]);

        let widened = table.union(vec![maybe, text]);

        let TypeKind::Union(members) = table.kind(widened).clone() else {
            panic!("expected a union, got {:?}", table.kind(widened));
        };
        assert_eq!(members.len(), 3);
        assert_eq!(table.union(vec![maybe, number]), maybe);
    }

    #[test]
    fn a_union_drops_a_literal_its_base_already_covers() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let zero = table.number_literal(0.0);

        assert_eq!(table.union(vec![number, zero]), number);

        let never = table.intern(TypeKind::Never);
        assert_eq!(table.union(vec![never, zero]), zero);

        let unknown = table.intern(TypeKind::Unknown);
        assert_eq!(table.union(vec![unknown, number]), unknown);
    }

    #[test]
    fn a_literal_fits_its_base_but_not_the_other_way() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let zero = table.number_literal(0.0);

        assert!(assignable(&table, zero, number));
        assert!(!assignable(&table, number, zero));
    }

    #[test]
    fn a_value_fits_a_union_that_holds_it() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let undefined = table.intern(TypeKind::Undefined);
        let maybe = table.union(vec![number, undefined]);

        assert!(assignable(&table, undefined, maybe));
        assert!(assignable(&table, maybe, maybe));
        assert!(!assignable(&table, maybe, number));
    }

    #[test]
    fn a_mutable_array_keeps_its_element_type_exactly() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let undefined = table.intern(TypeKind::Undefined);
        let maybe = table.union(vec![number, undefined]);
        let numbers = table.intern(TypeKind::Array(number));
        let maybes = table.intern(TypeKind::Array(maybe));
        let readonly_maybes = table.intern(TypeKind::ReadonlyArray(maybe));

        assert!(!assignable(&table, numbers, maybes));
        assert!(assignable(&table, numbers, readonly_maybes));
    }

    #[test]
    fn a_shape_fits_an_index_signature_when_every_field_matches() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let text = table.intern(TypeKind::String);
        let empty = table.object(Vec::new());
        let dictionary = table.object_with_indexes(
            Vec::new(),
            vec![IndexSignature {
                key: text,
                value: number,
                readonly: false,
            }],
        );
        let counted = table.object(vec![Field {
            name: "hits".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        }]);
        let named = table.object(vec![Field {
            name: "hits".to_string(),
            type_id: text,
            optional: false,
            method: false,
            readonly: false,
        }]);

        assert!(assignable(&table, empty, dictionary));
        assert!(assignable(&table, counted, dictionary));
        assert!(!assignable(&table, named, dictionary));
    }

    #[test]
    fn a_missing_field_only_fits_an_optional_one() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let empty = table.object(Vec::new());
        let required = table.object(vec![Field {
            name: "x".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        }]);
        let optional = table.object(vec![Field {
            name: "x".to_string(),
            type_id: number,
            optional: true,
            method: false,
            readonly: false,
        }]);

        assert!(!assignable(&table, empty, required));
        assert!(assignable(&table, empty, optional));
        assert!(assignable(&table, required, optional));
        assert!(!assignable(&table, optional, required));
    }

    #[test]
    fn a_readonly_field_takes_a_narrower_type_and_a_mutable_one_does_not() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let zero = table.number_literal(0.0);
        let mutable_zero = table.object(vec![Field {
            name: "x".to_string(),
            type_id: zero,
            optional: false,
            method: false,
            readonly: false,
        }]);
        let mutable_number = table.object(vec![Field {
            name: "x".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        }]);
        let frozen_number = table.object(vec![Field {
            name: "x".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: true,
        }]);

        assert!(assignable(&table, mutable_zero, frozen_number));
        assert!(!assignable(&table, mutable_zero, mutable_number));
        assert!(assignable(&table, mutable_number, frozen_number));
    }

    #[test]
    fn a_readonly_field_does_not_fit_a_mutable_one() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let frozen = table.object(vec![Field {
            name: "x".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: true,
        }]);
        let mutable = table.object(vec![Field {
            name: "x".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        }]);

        assert!(!assignable(&table, frozen, mutable));
    }

    #[test]
    fn a_readonly_index_signature_does_not_fit_a_mutable_one() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let text = table.intern(TypeKind::String);
        let zero = table.number_literal(0.0);
        let frozen = table.object_with_indexes(
            Vec::new(),
            vec![IndexSignature {
                key: text,
                value: number,
                readonly: true,
            }],
        );
        let mutable = table.object_with_indexes(
            Vec::new(),
            vec![IndexSignature {
                key: text,
                value: number,
                readonly: false,
            }],
        );
        let frozen_zeroes = table.object_with_indexes(
            Vec::new(),
            vec![IndexSignature {
                key: text,
                value: zero,
                readonly: true,
            }],
        );

        assert!(!assignable(&table, frozen, mutable));
        assert!(assignable(&table, mutable, frozen));
        assert!(assignable(&table, frozen_zeroes, frozen));
        assert!(!assignable(&table, frozen_zeroes, mutable));
    }

    #[test]
    fn substituting_a_recursive_shape_keeps_the_knot_and_replaces_the_parameter() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let held = table.fresh_type_parameter();
        let chain = table.reserve();
        let node = table.object(vec![
            Field {
                name: "value".to_string(),
                type_id: held,
                optional: false,
                method: false,
                readonly: false,
            },
            Field {
                name: "next".to_string(),
                type_id: chain,
                optional: false,
                method: false,
                readonly: false,
            },
        ]);
        table.fill(chain, table.kind(node).clone());
        let mut bindings = Bindings::new();
        bindings.insert(held, number);

        let built = substitute(&mut table, chain, &bindings)
            .expect("a recursive shape is tied back to itself instead of unrolled forever");
        let TypeKind::Object { fields, .. } = table.kind(built).clone() else {
            panic!("expected an object, got {:?}", table.kind(built));
        };
        let value = fields
            .iter()
            .find(|field| field.name == "value")
            .expect("the shape keeps its value field");
        assert_eq!(
            value.type_id, number,
            "the parameter stands for `number` now"
        );
        let next = fields
            .iter()
            .find(|field| field.name == "next")
            .expect("the shape keeps its next field");
        assert!(
            assignable(&table, next.type_id, built) && assignable(&table, built, next.type_id),
            "the knot points back at the substituted shape"
        );
        assert!(
            !assignable(&table, built, chain),
            "the shape before substitution held a type parameter, so it is not the same type"
        );
    }

    #[test]
    fn inference_walks_into_an_array_and_a_shape() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let held = table.fresh_type_parameter();
        let of_held = table.intern(TypeKind::ReadonlyArray(held));
        let numbers = table.intern(TypeKind::Array(number));
        let mut bindings = Bindings::new();

        assert!(infer(&table, of_held, numbers, &mut bindings).is_ok());
        assert_eq!(bindings.get(&held), Some(&number));

        let boxed_held = table.object(vec![Field {
            name: "value".to_string(),
            type_id: held,
            optional: false,
            method: false,
            readonly: false,
        }]);
        let boxed_number = table.object(vec![Field {
            name: "value".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        }]);
        let mut bindings = Bindings::new();
        assert!(infer(&table, boxed_held, boxed_number, &mut bindings).is_ok());
        assert_eq!(bindings.get(&held), Some(&number));
    }

    #[test]
    fn a_recursive_shape_fits_itself() {
        let mut table = TypeTable::new();
        let chain = table.reserve();
        let node = table.object(vec![Field {
            name: "next".to_string(),
            type_id: chain,
            optional: false,
            method: false,
            readonly: false,
        }]);
        table.fill(chain, table.kind(node).clone());

        assert!(assignable(&table, chain, node));
    }

    #[test]
    fn a_function_takes_a_wider_parameter_and_returns_a_narrower_value() {
        let mut table = TypeTable::new();
        let number = table.intern(TypeKind::Number);
        let undefined = table.intern(TypeKind::Undefined);
        let maybe = table.union(vec![number, undefined]);
        let zero = table.number_literal(0.0);
        let wide = table.intern(TypeKind::Function {
            type_parameters: Vec::new(),
            parameters: vec![Parameter {
                type_id: maybe,
                rest: false,
                optional: false,
            }],
            returns: zero,
        });
        let narrow = table.intern(TypeKind::Function {
            type_parameters: Vec::new(),
            parameters: vec![Parameter {
                type_id: number,
                rest: false,
                optional: false,
            }],
            returns: number,
        });

        assert!(assignable(&table, wide, narrow));
        assert!(!assignable(&table, narrow, wide));
    }
}
