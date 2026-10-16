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
    Conditional,
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

    if flags.has(TypeFlags::INDEXED_ACCESS) {
        return Classified::IndexedAccess;
    }
    if flags.has(TypeFlags::CONDITIONAL) {
        return Classified::Conditional;
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

const CALL_SIGNATURE_KIND: u32 = 181;

#[must_use]
pub fn call_signature_kind() -> u32 {
    CALL_SIGNATURE_KIND
}

#[must_use]
pub fn optional_parameters(printed: &str) -> Vec<bool> {
    let Some(list) = parameter_list(printed) else {
        return Vec::new();
    };
    split_at_top_level(list)
        .into_iter()
        .map(|parameter| {
            let parameter = parameter.trim();
            if parameter.starts_with("...") {
                return false;
            }
            let name = match first_top_level_colon(parameter) {
                Some(at) => &parameter[..at],
                None => parameter,
            };
            name.trim_end().ends_with('?')
        })
        .collect()
}

fn parameter_list(printed: &str) -> Option<&str> {
    let mut angle = 0i32;
    let mut opened = None;
    let mut depth = 0i32;
    for (at, letter) in printed.char_indices() {
        match letter {
            '<' if opened.is_none() => angle += 1,
            '>' if opened.is_none() => angle -= 1,
            '(' if angle == 0 => {
                if opened.is_none() {
                    opened = Some(at + 1);
                }
                depth += 1;
            }
            ')' if opened.is_some() => {
                depth -= 1;
                if depth == 0 {
                    return Some(&printed[opened?..at]);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_at_top_level(list: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (at, letter) in nesting(list) {
        match letter {
            Nesting::Opens => depth += 1,
            Nesting::Closes => depth -= 1,
            Nesting::Colon => {}
            Nesting::Comma if depth <= 0 => {
                pieces.push(&list[start..at]);
                start = at + 1;
            }
            Nesting::Comma => {}
        }
    }
    if !list[start..].trim().is_empty() {
        pieces.push(&list[start..]);
    }
    pieces
}

fn first_top_level_colon(parameter: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (at, letter) in nesting(parameter) {
        match letter {
            Nesting::Opens => depth += 1,
            Nesting::Closes => depth -= 1,
            Nesting::Colon if depth <= 0 => return Some(at),
            _ => {}
        }
    }
    None
}

enum Nesting {
    Opens,
    Closes,
    Comma,
    Colon,
}

fn nesting(text: &str) -> Vec<(usize, Nesting)> {
    let mut marks = Vec::new();
    let mut letters = text.char_indices().peekable();
    while let Some((at, letter)) = letters.next() {
        match letter {
            '=' if letters.peek().map(|(_, next)| *next) == Some('>') => {
                letters.next();
            }
            '(' | '[' | '{' | '<' => marks.push((at, Nesting::Opens)),
            ')' | ']' | '}' | '>' => marks.push((at, Nesting::Closes)),
            ',' => marks.push((at, Nesting::Comma)),
            ':' => marks.push((at, Nesting::Colon)),
            _ => {}
        }
    }
    marks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_printed_signature_marks_which_parameters_may_be_left_out() {
        assert_eq!(
            optional_parameters("(a: number, b?: string | undefined): void;"),
            vec![false, true]
        );
        assert_eq!(
            optional_parameters("(a: number, b: string | undefined): void;"),
            vec![false, false],
            "a parameter whose type admits undefined is not the same as an optional one"
        );
        assert_eq!(
            optional_parameters("(a: number, ...rest: string[]): void;"),
            vec![false, false]
        );
        assert_eq!(optional_parameters("(): void;"), Vec::<bool>::new());
    }

    #[test]
    fn a_printed_signature_with_awkward_types_still_splits_by_parameter() {
        assert_eq!(
            optional_parameters("<T>(callback: (x: T, y?: T) => void, limit?: number): T;"),
            vec![false, true],
            "a `?` inside a parameter type belongs to that type, not to the parameter"
        );
        assert_eq!(
            optional_parameters("(pair: [number, string], shape?: { a?: number }): void;"),
            vec![false, true]
        );
        assert_eq!(
            optional_parameters("(picked: A extends B ? C : D): void;"),
            vec![false],
            "a conditional type in a parameter does not make the parameter optional"
        );
    }

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
    fn a_conditional_type_is_handed_to_the_caller_so_it_can_stay_deferred() {
        assert_eq!(classify(TypeFlags::CONDITIONAL), Classified::Conditional);
    }

    #[test]
    fn hard_type_level_constructs_are_unsupported_not_guessed() {
        for flag in [
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
