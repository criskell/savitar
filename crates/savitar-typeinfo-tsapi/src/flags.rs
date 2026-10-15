#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TypeFlags(pub u32);

impl TypeFlags {
    pub const NONE: Self = Self(0);
    pub const ANY: Self = Self(1 << 0);
    pub const UNKNOWN: Self = Self(1 << 1);
    pub const UNDEFINED: Self = Self(1 << 2);
    pub const NULL: Self = Self(1 << 3);
    pub const VOID: Self = Self(1 << 4);
    pub const STRING: Self = Self(1 << 5);
    pub const NUMBER: Self = Self(1 << 6);
    pub const BIG_INT: Self = Self(1 << 7);
    pub const BOOLEAN: Self = Self(1 << 8);
    pub const ES_SYMBOL: Self = Self(1 << 9);
    pub const STRING_LITERAL: Self = Self(1 << 10);
    pub const NUMBER_LITERAL: Self = Self(1 << 11);
    pub const BIG_INT_LITERAL: Self = Self(1 << 12);
    pub const BOOLEAN_LITERAL: Self = Self(1 << 13);
    pub const UNIQUE_ES_SYMBOL: Self = Self(1 << 14);
    pub const ENUM_LITERAL: Self = Self(1 << 15);
    pub const ENUM: Self = Self(1 << 16);
    pub const NON_PRIMITIVE: Self = Self(1 << 17);
    pub const NEVER: Self = Self(1 << 18);
    pub const TYPE_PARAMETER: Self = Self(1 << 19);
    pub const OBJECT: Self = Self(1 << 20);
    pub const INDEX: Self = Self(1 << 21);
    pub const TEMPLATE_LITERAL: Self = Self(1 << 22);
    pub const STRING_MAPPING: Self = Self(1 << 23);
    pub const SUBSTITUTION: Self = Self(1 << 24);
    pub const INDEXED_ACCESS: Self = Self(1 << 25);
    pub const CONDITIONAL: Self = Self(1 << 26);
    pub const UNION: Self = Self(1 << 27);
    pub const INTERSECTION: Self = Self(1 << 28);

    #[must_use]
    pub const fn has(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObjectFlags(pub u32);

impl ObjectFlags {
    pub const CLASS: Self = Self(1 << 0);
    pub const INTERFACE: Self = Self(1 << 1);
    pub const REFERENCE: Self = Self(1 << 2);
    pub const ANONYMOUS: Self = Self(1 << 4);
    pub const MAPPED: Self = Self(1 << 5);

    #[must_use]
    pub const fn has(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SymbolFlags(pub u32);

impl SymbolFlags {
    pub const PROPERTY: Self = Self(1 << 2);
    pub const METHOD: Self = Self(1 << 13);
    pub const OPTIONAL: Self = Self(1 << 24);

    #[must_use]
    pub const fn has(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}
