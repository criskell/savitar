use savitar_types::{Field, TypeId, TypeKind, TypeTable, assignable};

fn field(name: &str, type_id: TypeId, readonly: bool) -> Field {
    Field {
        name: name.to_string(),
        type_id,
        optional: false,
        method: false,
        readonly,
    }
}

fn method(name: &str, type_id: TypeId) -> Field {
    Field {
        name: name.to_string(),
        type_id,
        optional: false,
        method: true,
        readonly: false,
    }
}

fn taking(table: &mut TypeTable, parameter: TypeId) -> TypeId {
    let void = table.intern(TypeKind::Void);
    table.function(
        vec![savitar_types::Parameter {
            type_id: parameter,
            rest: false,
            optional: false,
        }],
        void,
    )
}

#[test]
fn a_method_slot_is_covariant_because_no_write_over_a_method_is_lowered() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![number, text]);
    let takes_number = taking(&mut table, number);
    let takes_either = taking(&mut table, either);
    let narrow = table.object(vec![method("set", takes_number)]);
    let wide = table.object(vec![method("set", takes_either)]);
    assert!(
        assignable(&table, wide, narrow),
        "a method that handles more than the slot declares fits it, because the parameter is contravariant"
    );
    assert!(
        !assignable(&table, narrow, wide),
        "a method that only handles numbers cannot stand where one that handles strings too is declared"
    );
}

#[test]
fn a_function_valued_field_stays_invariant_because_it_takes_a_write() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let either = table.union(vec![number, text]);
    let takes_number = taking(&mut table, number);
    let takes_either = taking(&mut table, either);
    let narrow = table.object(vec![field("set", takes_number, false)]);
    let wide = table.object(vec![field("set", takes_either, false)]);
    assert!(!assignable(&table, wide, narrow));
    assert!(!assignable(&table, narrow, wide));
}

#[test]
fn a_method_does_not_fill_a_plain_mutable_field_slot() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let takes_number = taking(&mut table, number);
    let held = table.object(vec![method("set", takes_number)]);
    let wanted = table.object(vec![field("set", takes_number, false)]);
    assert!(
        !assignable(&table, held, wanted),
        "a method takes no write, so it cannot stand in a slot something will write"
    );
}

#[test]
fn a_method_slot_still_demands_the_return_fit() {
    let mut table = TypeTable::new();
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let gives_number = table.function(vec![], number);
    let gives_text = table.function(vec![], text);
    let held = table.object(vec![method("read", gives_number)]);
    let wanted = table.object(vec![method("read", gives_text)]);
    assert!(!assignable(&table, held, wanted));
}
