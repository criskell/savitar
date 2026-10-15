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
    Array(TypeId),
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
    fn nominal_ids_keep_their_names() {
        let mut table = TypeTable::new();
        let point = table.declare_nominal("Point");
        let user = table.declare_nominal("User");
        assert_eq!(table.nominal_name(point), "Point");
        assert_eq!(table.nominal_name(user), "User");
        assert_ne!(point, user);
    }
}
