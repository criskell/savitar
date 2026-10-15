use savitar_types::TypeKind;

use crate::flags::TypeFlags;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classified {
    Primitive(TypeKind),
    StringLiteral,
    NumberLiteral,
    BooleanLiteral,
    Union,
    Intersection,
    Object,
    TypeParameter,
    IndexedAccess,
    Unsupported(&'static str),
}

#[must_use]
pub fn describe(flags: TypeFlags) -> &'static str {
    for (flag, name) in [
        (TypeFlags::TYPE_PARAMETER, "a type parameter"),
        (TypeFlags::CONDITIONAL, "a conditional type"),
        (TypeFlags::INDEXED_ACCESS, "another indexed access"),
        (TypeFlags::INDEX, "a `keyof`"),
        (TypeFlags::SUBSTITUTION, "a substituted type parameter"),
        (TypeFlags::TEMPLATE_LITERAL, "a template literal type"),
        (TypeFlags::STRING_MAPPING, "a mapped string"),
        (TypeFlags::STRING_LITERAL, "a literal key"),
        (TypeFlags::NUMBER_LITERAL, "a literal key"),
        (TypeFlags::BIG_INT_LITERAL, "a literal key"),
        (TypeFlags::BOOLEAN_LITERAL, "a literal key"),
        (TypeFlags::UNIQUE_ES_SYMBOL, "a `unique symbol` key"),
        (TypeFlags::ENUM, "an enum"),
        (TypeFlags::ENUM_LITERAL, "an enum member"),
        (TypeFlags::STRING, "`string`"),
        (TypeFlags::NUMBER, "`number`"),
        (TypeFlags::BIG_INT, "`bigint`"),
        (TypeFlags::BOOLEAN, "`boolean`"),
        (TypeFlags::ES_SYMBOL, "`symbol`"),
        (TypeFlags::ANY, "`any`"),
        (TypeFlags::UNKNOWN, "`unknown`"),
        (TypeFlags::NEVER, "`never`"),
        (TypeFlags::UNDEFINED, "`undefined`"),
        (TypeFlags::NULL, "`null`"),
        (TypeFlags::VOID, "`void`"),
        (TypeFlags::NON_PRIMITIVE, "`object`"),
        (TypeFlags::UNION, "a union"),
        (TypeFlags::INTERSECTION, "an intersection"),
        (TypeFlags::OBJECT, "an object shape"),
    ] {
        if flags.has(flag) {
            return name;
        }
    }
    "a type Savitar does not recognise"
}

#[must_use]
pub fn classify(flags: TypeFlags) -> Classified {
    if flags.has(TypeFlags::ANY) {
        return Classified::Primitive(TypeKind::Unknown);
    }
    if flags.has(TypeFlags::ENUM) || flags.has(TypeFlags::ENUM_LITERAL) {
        return Classified::Unsupported("enums are not modelled yet: reverse mapping is unsound");
    }
    if flags.has(TypeFlags::BOOLEAN) {
        return Classified::Primitive(TypeKind::Boolean);
    }
    if flags.has(TypeFlags::TYPE_PARAMETER) {
        return Classified::TypeParameter;
    }

    for (flag, kind) in [
        (TypeFlags::UNKNOWN, TypeKind::Unknown),
        (TypeFlags::NEVER, TypeKind::Never),
        (TypeFlags::UNDEFINED, TypeKind::Undefined),
        (TypeFlags::NULL, TypeKind::Null),
        (TypeFlags::VOID, TypeKind::Void),
        (TypeFlags::STRING, TypeKind::String),
        (TypeFlags::NUMBER, TypeKind::Number),
        (TypeFlags::BIG_INT, TypeKind::BigInt),
        (TypeFlags::ES_SYMBOL, TypeKind::Symbol),
        (TypeFlags::NON_PRIMITIVE, TypeKind::NonPrimitive),
    ] {
        if flags.has(flag) {
            return Classified::Primitive(kind);
        }
    }

    for (flag, classified) in [
        (TypeFlags::STRING_LITERAL, Classified::StringLiteral),
        (TypeFlags::NUMBER_LITERAL, Classified::NumberLiteral),
        (TypeFlags::BOOLEAN_LITERAL, Classified::BooleanLiteral),
    ] {
        if flags.has(flag) {
            return classified;
        }
    }

    if flags.has(TypeFlags::INDEXED_ACCESS) {
        return Classified::IndexedAccess;
    }

    for (flag, reason) in [
        (
            TypeFlags::BIG_INT_LITERAL,
            "bigint literals are not modelled yet",
        ),
        (
            TypeFlags::UNIQUE_ES_SYMBOL,
            "`unique symbol` is not modelled yet",
        ),
        (TypeFlags::INDEX, "`keyof T` is not modelled yet"),
        (
            TypeFlags::TEMPLATE_LITERAL,
            "template literal types are not modelled yet",
        ),
        (
            TypeFlags::STRING_MAPPING,
            "`Uppercase`/`Lowercase` are not modelled yet",
        ),
        (
            TypeFlags::SUBSTITUTION,
            "type parameter substitution is not modelled yet",
        ),
        (
            TypeFlags::CONDITIONAL,
            "conditional types need the type evaluator",
        ),
    ] {
        if flags.has(flag) {
            return Classified::Unsupported(reason);
        }
    }

    if flags.has(TypeFlags::UNION) {
        return Classified::Union;
    }
    if flags.has(TypeFlags::INTERSECTION) {
        return Classified::Intersection;
    }
    if flags.has(TypeFlags::OBJECT) {
        return Classified::Object;
    }
    Classified::Unsupported("the checker answered a type Savitar does not recognise")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_from_a_declaration_becomes_unknown_so_it_must_be_narrowed() {
        assert_eq!(
            classify(TypeFlags::ANY),
            Classified::Primitive(TypeKind::Unknown)
        );
    }

    #[test]
    fn primitives_map_directly() {
        assert_eq!(
            classify(TypeFlags::NUMBER),
            Classified::Primitive(TypeKind::Number)
        );
        assert_eq!(
            classify(TypeFlags::STRING),
            Classified::Primitive(TypeKind::String)
        );
        assert_eq!(
            classify(TypeFlags::VOID),
            Classified::Primitive(TypeKind::Void)
        );
        assert_eq!(
            classify(TypeFlags::NEVER),
            Classified::Primitive(TypeKind::Never)
        );
    }

    #[test]
    fn boolean_wins_over_the_union_flag_it_carries() {
        let boolean = TypeFlags(TypeFlags::BOOLEAN.0 | TypeFlags::UNION.0);
        assert_eq!(classify(boolean), Classified::Primitive(TypeKind::Boolean));
    }

    #[test]
    fn enum_literal_wins_over_the_union_flag_it_carries() {
        let numeric_enum = TypeFlags(TypeFlags::ENUM_LITERAL.0 | TypeFlags::UNION.0);
        let Classified::Unsupported(reason) = classify(numeric_enum) else {
            panic!("expected an enum to be unsupported");
        };
        assert!(reason.contains("reverse mapping"));
    }

    #[test]
    fn a_type_parameter_is_recognised_rather_than_refused() {
        assert_eq!(
            classify(TypeFlags::TYPE_PARAMETER),
            Classified::TypeParameter
        );
    }

    #[test]
    fn an_intersection_is_walked_by_the_caller() {
        assert_eq!(classify(TypeFlags::INTERSECTION), Classified::Intersection);
    }

    #[test]
    fn a_plain_union_is_walked_by_the_caller() {
        assert_eq!(classify(TypeFlags::UNION), Classified::Union);
    }

    #[test]
    fn an_object_is_resolved_by_the_caller() {
        assert_eq!(classify(TypeFlags::OBJECT), Classified::Object);
    }

    #[test]
    fn literal_types_are_modelled() {
        assert_eq!(
            classify(TypeFlags::STRING_LITERAL),
            Classified::StringLiteral
        );
        assert_eq!(
            classify(TypeFlags::NUMBER_LITERAL),
            Classified::NumberLiteral
        );
        assert_eq!(
            classify(TypeFlags::BOOLEAN_LITERAL),
            Classified::BooleanLiteral
        );
    }

    #[test]
    fn an_indexed_access_is_handed_to_the_caller_so_its_operands_can_be_named() {
        assert_eq!(
            classify(TypeFlags::INDEXED_ACCESS),
            Classified::IndexedAccess
        );
    }

    #[test]
    fn an_operand_of_an_indexed_access_is_described_by_what_it_is() {
        assert_eq!(describe(TypeFlags::TYPE_PARAMETER), "a type parameter");
        assert_eq!(describe(TypeFlags::CONDITIONAL), "a conditional type");
        assert_eq!(describe(TypeFlags::OBJECT), "an object shape");
        assert_eq!(describe(TypeFlags::STRING_LITERAL), "a literal key");
        assert_eq!(describe(TypeFlags::INDEX), "a `keyof`");
    }

    #[test]
    fn an_operand_with_no_flags_is_not_given_a_plausible_name() {
        assert_eq!(
            describe(TypeFlags::NONE),
            "a type Savitar does not recognise"
        );
    }

    #[test]
    fn hard_type_level_constructs_are_unsupported_not_guessed() {
        for flag in [
            TypeFlags::CONDITIONAL,
            TypeFlags::INDEX,
            TypeFlags::TEMPLATE_LITERAL,
            TypeFlags::BIG_INT_LITERAL,
        ] {
            assert!(
                matches!(classify(flag), Classified::Unsupported(_)),
                "flag {flag:?} should be unsupported"
            );
        }
    }

    #[test]
    fn no_flags_is_unsupported() {
        assert!(matches!(
            classify(TypeFlags::NONE),
            Classified::Unsupported(_)
        ));
    }
}
