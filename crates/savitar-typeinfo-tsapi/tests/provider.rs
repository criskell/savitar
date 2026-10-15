use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use savitar_diagnostics::Span;
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_typeinfo_tsapi::TsApi;
use savitar_types::{Field, IndexSignature, TypeKind, TypeTable};

const SOURCE: &str = concat!(
    "const n = 1;\n",
    "const s = \"hi\";\n",
    "const b = true;\n",
    "declare const u: number | string;\n",
    "declare const opt: number | undefined;\n",
    "declare const pt: { x: number };\n",
    "declare const tup: [number, string];\n",
    "declare const arr: number[];\n",
    "declare const ro: readonly string[];\n",
    "declare const nested: number[][];\n",
    "declare const st: Set<number>;\n",
    "declare const shape: { y: string; x: number };\n",
    "declare const maybe: { x: number; tag?: string };\n",
    "interface Boxed { inner: { deep: number } }\n",
    "declare const boxed: Boxed;\n",
    "interface Chain { next: Chain }\n",
    "declare const chain: Chain;\n",
    "declare const dict: Record<string, number>;\n",
    "declare const mixed: { a: number; [k: string]: number };\n",
);

fn provider_binary() -> Option<OsString> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../node_modules/.bin/tsc-rs")
        .canonicalize()
        .ok()?;
    path.exists().then(|| path.into_os_string())
}

fn scratch_project() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before the epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("savitar-tsapi-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("could not create the scratch project");
    std::fs::write(
        dir.join("tsconfig.json"),
        "{ \"compilerOptions\": { \"strict\": true, \"noUncheckedIndexedAccess\": true, \"exactOptionalPropertyTypes\": true } }",
    )
    .expect("could not write tsconfig.json");
    std::fs::write(dir.join("a.ts"), SOURCE).expect("could not write a.ts");
    dir
}

fn position_of(name: &str) -> u32 {
    let needle = format!("const {name}");
    let offset = SOURCE.find(&needle).expect("binding not in the fixture") + needle.len() - 1;
    u32::try_from(offset).expect("fixture is small")
}

#[test]
fn reads_literals_unions_and_arrays_from_the_provider() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project();
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = file.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let number = api.type_at(&mut table, path, Span::new(position_of("n"), 0));
    let Answer::Known(number) = number else {
        panic!("expected a known type for `n`, got {number:?}");
    };
    assert_eq!(
        table.kind(number),
        &TypeKind::NumberLiteral(1.0f64.to_bits())
    );

    let text = api.type_at(&mut table, path, Span::new(position_of("s"), 0));
    let Answer::Known(text) = text else {
        panic!("expected a known type for `s`, got {text:?}");
    };
    assert_eq!(table.kind(text), &TypeKind::StringLiteral("hi".to_string()));

    let boolean = api.type_at(&mut table, path, Span::new(position_of("b"), 0));
    let Answer::Known(boolean) = boolean else {
        panic!("expected a known type for `b`, got {boolean:?}");
    };
    assert_eq!(table.kind(boolean), &TypeKind::BooleanLiteral(true));

    let union = api.type_at(&mut table, path, Span::new(position_of("u"), 0));
    let Answer::Known(union) = union else {
        panic!("expected a known type for `u`, got {union:?}");
    };
    let TypeKind::Union(members) = table.kind(union).clone() else {
        panic!("expected a union, got {:?}", table.kind(union));
    };
    assert_eq!(members.len(), 2);
    let kinds: Vec<&TypeKind> = members.iter().map(|&id| table.kind(id)).collect();
    assert!(
        kinds.contains(&&TypeKind::Number),
        "missing number: {kinds:?}"
    );
    assert!(
        kinds.contains(&&TypeKind::String),
        "missing string: {kinds:?}"
    );

    let optional = api.type_at(&mut table, path, Span::new(position_of("opt"), 0));
    let Answer::Known(optional) = optional else {
        panic!("expected a known type for `opt`, got {optional:?}");
    };
    let number = table.intern(TypeKind::Number);
    assert_eq!(optional, table.optional(number));

    let tuple = api.type_at(&mut table, path, Span::new(position_of("tup"), 0));
    let Answer::Unsupported(reason) = tuple else {
        panic!("a tuple must not be guessed, got {tuple:?}");
    };
    assert!(reason.contains("tuple"), "unexpected: {reason}");

    let array = api.type_at(&mut table, path, Span::new(position_of("arr"), 0));
    let Answer::Known(array) = array else {
        panic!("expected a known type for `arr`, got {array:?}");
    };
    let number = table.intern(TypeKind::Number);
    assert_eq!(table.kind(array), &TypeKind::Array(number));

    let readonly = api.type_at(&mut table, path, Span::new(position_of("ro"), 0));
    let Answer::Known(readonly) = readonly else {
        panic!("expected a known type for `ro`, got {readonly:?}");
    };
    let text_kind = table.intern(TypeKind::String);
    assert_eq!(table.kind(readonly), &TypeKind::ReadonlyArray(text_kind));

    let nested = api.type_at(&mut table, path, Span::new(position_of("nested"), 0));
    let Answer::Known(nested) = nested else {
        panic!("expected a known type for `nested`, got {nested:?}");
    };
    assert_eq!(table.kind(nested), &TypeKind::Array(array));

    let other = api.type_at(&mut table, path, Span::new(position_of("st"), 0));
    let Answer::Unsupported(reason) = other else {
        panic!("`Set` must not be guessed, got {other:?}");
    };
    assert!(reason.contains("method"), "unexpected: {reason}");

    let object = api.type_at(&mut table, path, Span::new(position_of("pt"), 0));
    let Answer::Known(object) = object else {
        panic!("expected a known type for `pt`, got {object:?}");
    };
    let number = table.intern(TypeKind::Number);
    let expected = table.object(vec![Field {
        name: "x".to_string(),
        type_id: number,
        optional: false,
    }]);
    assert_eq!(object, expected);

    let shape = api.type_at(&mut table, path, Span::new(position_of("shape"), 0));
    let Answer::Known(shape) = shape else {
        panic!("expected a known type for `shape`, got {shape:?}");
    };
    let text_kind = table.intern(TypeKind::String);
    let sorted = table.object(vec![
        Field {
            name: "x".to_string(),
            type_id: number,
            optional: false,
        },
        Field {
            name: "y".to_string(),
            type_id: text_kind,
            optional: false,
        },
    ]);
    assert_eq!(shape, sorted, "declaration order must not change identity");

    let maybe = api.type_at(&mut table, path, Span::new(position_of("maybe"), 0));
    let Answer::Known(maybe) = maybe else {
        panic!("expected a known type for `maybe`, got {maybe:?}");
    };
    let TypeKind::Object { fields, .. } = table.kind(maybe).clone() else {
        panic!("expected an object, got {:?}", table.kind(maybe));
    };
    let tag = fields
        .iter()
        .find(|field| field.name == "tag")
        .expect("the optional field should be present");
    assert!(tag.optional, "`tag?` must be marked optional");
    assert_eq!(table.kind(tag.type_id), &TypeKind::String);

    let boxed = api.type_at(&mut table, path, Span::new(position_of("boxed"), 0));
    let Answer::Known(boxed) = boxed else {
        panic!("expected a known type for `boxed`, got {boxed:?}");
    };
    let inner = table.object(vec![Field {
        name: "deep".to_string(),
        type_id: number,
        optional: false,
    }]);
    let expected = table.object(vec![Field {
        name: "inner".to_string(),
        type_id: inner,
        optional: false,
    }]);
    assert_eq!(boxed, expected);

    let chain = api.type_at(&mut table, path, Span::new(position_of("chain"), 0));
    let Answer::Unsupported(reason) = chain else {
        panic!("a recursive type must not be guessed, got {chain:?}");
    };
    assert!(reason.contains("recursive"), "unexpected: {reason}");

    let dict = api.type_at(&mut table, path, Span::new(position_of("dict"), 0));
    let Answer::Known(dict) = dict else {
        panic!("expected a known type for `dict`, got {dict:?}");
    };
    let expected = table.object_with_indexes(
        vec![],
        vec![IndexSignature {
            key: text_kind,
            value: number,
        }],
    );
    assert_eq!(dict, expected, "Record should become an index signature");

    let mixed = api.type_at(&mut table, path, Span::new(position_of("mixed"), 0));
    let Answer::Known(mixed) = mixed else {
        panic!("expected a known type for `mixed`, got {mixed:?}");
    };
    let TypeKind::Object { fields, indexes } = table.kind(mixed).clone() else {
        panic!("expected an object, got {:?}", table.kind(mixed));
    };
    assert_eq!(
        fields.len(),
        1,
        "the named field should survive: {fields:?}"
    );
    assert_eq!(fields[0].name, "a");
    assert_eq!(indexes.len(), 1, "the index signature should survive");
    assert_eq!(table.kind(indexes[0].key), &TypeKind::String);
    assert_eq!(table.kind(indexes[0].value), &TypeKind::Number);

    assert_eq!(api.name(), "tsapi");
    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn a_missing_binary_is_reported_not_guessed() {
    let project = scratch_project();
    let started = TsApi::start(
        &OsString::from("/nonexistent/savitar-type-provider"),
        &project,
        &[],
    );
    let Err(error) = started else {
        panic!("a missing binary must fail");
    };
    assert!(
        error.to_string().contains("could not start"),
        "unexpected error: {error}"
    );
    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn a_non_canonical_path_still_reaches_the_provider() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project();
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");

    let detoured = project.join("nowhere/../a.ts");
    let detoured = detoured.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(&mut table, detoured, Span::new(position_of("n"), 0));
    let Answer::Known(number) = answer else {
        panic!("a path with `..` should still resolve, got {answer:?}");
    };
    assert_eq!(
        table.kind(number),
        &TypeKind::NumberLiteral(1.0f64.to_bits())
    );
    std::fs::remove_dir_all(&project).ok();
}
