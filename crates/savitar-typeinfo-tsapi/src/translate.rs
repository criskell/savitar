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
    Unsupported(&'static str),
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
        (TypeFlags::INDEXED_ACCESS, "`T[K]` is not modelled yet"),
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
    fn hard_type_level_constructs_are_unsupported_not_guessed() {
        for flag in [
            TypeFlags::CONDITIONAL,
            TypeFlags::INDEXED_ACCESS,
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
