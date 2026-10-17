use savitar_types::{
    Field, INTENTIONAL_DIVERGENCES, IndexSignature, TypeId, TypeKind, TypeTable, assignable,
    is_registered,
};

fn field(name: &str, type_id: TypeId, readonly: bool) -> Field {
    Field {
        name: name.to_string(),
        type_id,
        optional: false,
        method: false,
        readonly,
    }
}

#[test]
fn the_list_of_intentional_divergences_is_exactly_what_is_registered() {
    let registered: Vec<&str> = INTENTIONAL_DIVERGENCES
        .iter()
        .map(|divergence| divergence.id)
        .collect();
    assert_eq!(
        registered,
        vec![
            "unknown-where-tsc-answers-any",
            "readonly-counts-in-assignability",
            "mutable-position-is-invariant",
            "inherited-member-must-fit-the-base",
        ],
        "a divergence from tsc that is not on this list is indistinguishable from a bug"
    );
}

#[test]
fn every_registered_divergence_says_both_what_tsc_answers_and_what_savitar_answers() {
    for divergence in INTENTIONAL_DIVERGENCES {
        assert!(!divergence.id.is_empty());
        assert!(
            !divergence.what_tsc_answers.is_empty(),
            "`{}` does not say what tsc answers",
            divergence.id
        );
        assert!(
            !divergence.what_savitar_answers.is_empty(),
            "`{}` does not say what Savitar answers",
            divergence.id
        );
        assert!(
            !divergence.why_savitar_refuses_to_follow.is_empty(),
            "`{}` does not say why Savitar refuses to follow",
            divergence.id
        );
    }
}

#[test]
fn no_divergence_is_registered_twice_under_the_same_id() {
    let mut seen: Vec<&str> = Vec::new();
    for divergence in INTENTIONAL_DIVERGENCES {
        assert!(
            !seen.contains(&divergence.id),
            "`{}` is registered twice",
            divergence.id
        );
        seen.push(divergence.id);
    }
}

#[test]
fn a_readonly_source_not_fitting_a_mutable_slot_is_a_divergence_on_purpose() {
    assert!(is_registered("readonly-counts-in-assignability"));
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let frozen = table.object(vec![field("count", number, true)]);
    let writable = table.object(vec![field("count", number, false)]);
    assert!(
        !assignable(&table, frozen, writable),
        "tsc lets a readonly field fill a mutable slot; Savitar does not"
    );
    assert!(
        assignable(&table, writable, frozen),
        "a mutable field still fills a readonly slot"
    );
}

#[test]
fn a_readonly_index_signature_not_fitting_a_mutable_one_is_a_divergence_on_purpose() {
    assert!(is_registered("readonly-counts-in-assignability"));
    let mut table = TypeTable::new();
    let text = table.intern(TypeKind::String);
    let number = table.intern(TypeKind::Number);
    let frozen = table.object_with_indexes(
        vec![],
        vec![IndexSignature {
            key: text,
            value: number,
            readonly: true,
        }],
    );
    let writable = table.object_with_indexes(
        vec![],
        vec![IndexSignature {
            key: text,
            value: number,
            readonly: false,
        }],
    );
    assert!(!assignable(&table, frozen, writable));
    assert!(assignable(&table, writable, frozen));
}

#[test]
fn an_array_of_a_narrower_element_not_fitting_a_wider_array_is_a_divergence_on_purpose() {
    assert!(is_registered("mutable-position-is-invariant"));
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![number, text]);
    let narrow = table.intern(TypeKind::Array(number));
    let wide = table.intern(TypeKind::Array(either));
    assert!(
        !assignable(&table, narrow, wide),
        "tsc accepts `number[]` where `(number | string)[]` is wanted, and a write of a string then lands in it"
    );
}

#[test]
fn an_array_of_a_narrower_element_still_fits_a_readonly_array() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![number, text]);
    let narrow = table.intern(TypeKind::Array(number));
    let wide = table.intern(TypeKind::ReadonlyArray(either));
    assert!(assignable(&table, narrow, wide));
}

#[test]
fn a_narrower_mutable_field_not_fitting_a_wider_one_is_a_divergence_on_purpose() {
    assert!(is_registered("mutable-position-is-invariant"));
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![number, text]);
    let narrow = table.object(vec![field("value", number, false)]);
    let wide = table.object(vec![field("value", either, false)]);
    assert!(!assignable(&table, narrow, wide));
}

#[test]
fn a_narrower_readonly_field_still_fits_a_wider_readonly_one() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![number, text]);
    let narrow = table.object(vec![field("value", number, true)]);
    let wide = table.object(vec![field("value", either, true)]);
    assert!(assignable(&table, narrow, wide));
}
