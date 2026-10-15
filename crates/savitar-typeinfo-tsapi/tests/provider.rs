use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use savitar_diagnostics::Span;
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_typeinfo_tsapi::TsApi;
use savitar_types::{Field, IndexSignature, Parameter, TypeKind, TypeTable};

const SOURCE: &str = concat!(
    "const n = 1;\n",
    "const s = \"hi\";\n",
    "const b = true;\n",
    "declare const u: number | string;\n",
    "declare const opt: number | undefined;\n",
    "declare const pt: { x: number };\n",
    "declare const tup: [number, string];\n",
    "declare const tupSolo: [number];\n",
    "declare const tupEmpty: [];\n",
    "declare const tupRo: readonly [number, string];\n",
    "declare const tupNested: [[number, string], boolean];\n",
    "declare const tupRest: [number, ...string[]];\n",
    "declare const tupLeading: [...string[], number];\n",
    "declare const tupOpt: [number, string?];\n",
    "declare const tupRoRest: readonly [number, ...string[]];\n",
    "declare const tupUndef: [number, string | undefined];\n",
    "declare const tupOptional: [number, string?];\n",
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
    "declare const fn: (a: number, b: string) => boolean;\n",
    "declare const loose: (a?: number) => void;\n",
    "declare const obj: { value: number; run(flag: boolean): void };\n",
    "declare function firstOf<T>(items: readonly T[]): T | undefined;\n",
    "declare function pick<K, V>(k: K, v: V): V;\n",
);

fn provider_binary() -> Option<OsString> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../node_modules/.bin/tsc-rs")
        .canonicalize()
        .ok()?;
    path.exists().then(|| path.into_os_string())
}

fn scratch_project_with(options: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before the epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("savitar-tsapi-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("could not create the scratch project");
    std::fs::write(
        dir.join("tsconfig.json"),
        format!("{{ \"compilerOptions\": {{ {options} }} }}"),
    )
    .expect("could not write tsconfig.json");
    std::fs::write(dir.join("a.ts"), SOURCE).expect("could not write a.ts");
    dir
}

fn scratch_project() -> PathBuf {
    scratch_project_with(
        "\"strict\": true, \"noUncheckedIndexedAccess\": true, \"exactOptionalPropertyTypes\": true",
    )
}

fn position_of(name: &str) -> u32 {
    for prefix in ["const ", "function "] {
        let needle = format!("{prefix}{name}");
        if let Some(at) = SOURCE.find(&needle) {
            let offset = at + needle.len() - 1;
            return u32::try_from(offset).expect("fixture is small");
        }
    }
    panic!("binding not in the fixture: {name}");
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
    assert!(
        reason.contains("any"),
        "`Set` is refused because the standard library uses `any`, got: {reason}"
    );

    let object = api.type_at(&mut table, path, Span::new(position_of("pt"), 0));
    let Answer::Known(object) = object else {
        panic!("expected a known type for `pt`, got {object:?}");
    };
    let number = table.intern(TypeKind::Number);
    let expected = table.object(vec![Field {
        name: "x".to_string(),
        type_id: number,
        optional: false,
        method: false,
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
            method: false,
        },
        Field {
            name: "y".to_string(),
            type_id: text_kind,
            optional: false,
            method: false,
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
        method: false,
    }]);
    let expected = table.object(vec![Field {
        name: "inner".to_string(),
        type_id: inner,
        optional: false,
        method: false,
    }]);
    assert_eq!(boxed, expected);

    let chain = api.type_at(&mut table, path, Span::new(position_of("chain"), 0));
    let Answer::Known(chain) = chain else {
        panic!("expected a known type for `chain`, got {chain:?}");
    };
    let TypeKind::Object { fields, .. } = table.kind(chain).clone() else {
        panic!("expected an object, got {:?}", table.kind(chain));
    };
    let next = fields
        .iter()
        .find(|field| field.name == "next")
        .expect("the recursive field should be present");
    assert_eq!(
        next.type_id, chain,
        "the recursive field must point back at the same type"
    );

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

    let function = api.type_at(&mut table, path, Span::new(position_of("fn"), 0));
    let Answer::Known(function) = function else {
        panic!("expected a known type for `fn`, got {function:?}");
    };
    let boolean = table.intern(TypeKind::Boolean);
    let expected = table.function(
        vec![
            Parameter { type_id: number },
            Parameter { type_id: text_kind },
        ],
        boolean,
    );
    assert_eq!(function, expected, "a function type must keep its shape");

    let loose = api.type_at(&mut table, path, Span::new(position_of("loose"), 0));
    let Answer::Unsupported(reason) = loose else {
        panic!("an optional parameter must not be guessed, got {loose:?}");
    };
    assert!(reason.contains("optional"), "unexpected: {reason}");

    let with_method = api.type_at(&mut table, path, Span::new(position_of("obj"), 0));
    let Answer::Known(with_method) = with_method else {
        panic!("expected a known type for `obj`, got {with_method:?}");
    };
    let TypeKind::Object { fields, .. } = table.kind(with_method).clone() else {
        panic!("expected an object, got {:?}", table.kind(with_method));
    };
    let run = fields
        .iter()
        .find(|field| field.name == "run")
        .expect("the method should be a field");
    assert!(
        run.method,
        "`run` must be marked a method, not a plain field"
    );
    let value = fields
        .iter()
        .find(|field| field.name == "value")
        .expect("the plain field should be present");
    assert!(!value.method, "`value` must not be marked a method");
    let void = table.intern(TypeKind::Void);
    let run_signature = table.function(vec![Parameter { type_id: boolean }], void);
    assert_eq!(run.type_id, run_signature);

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

#[test]
fn a_fixed_tuple_keeps_its_elements_in_order() {
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

    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let boolean = table.intern(TypeKind::Boolean);

    let pair = api.type_at(&mut table, path, Span::new(position_of("tup"), 0));
    let Answer::Known(pair) = pair else {
        panic!("expected a known type for `tup`, got {pair:?}");
    };
    let expected = table.tuple(vec![number, text], false);
    assert_eq!(pair, expected);
    assert_ne!(
        pair,
        table.tuple(vec![text, number], false),
        "element order must be part of the identity"
    );

    let solo = api.type_at(&mut table, path, Span::new(position_of("tupSolo"), 0));
    let Answer::Known(solo) = solo else {
        panic!("expected a known type for `tupSolo`, got {solo:?}");
    };
    let expected = table.tuple(vec![number], false);
    assert_eq!(solo, expected);

    let empty = api.type_at(&mut table, path, Span::new(position_of("tupEmpty"), 0));
    let Answer::Known(empty) = empty else {
        panic!("expected a known type for `tupEmpty`, got {empty:?}");
    };
    let expected = table.tuple(vec![], false);
    assert_eq!(empty, expected);

    let nested = api.type_at(&mut table, path, Span::new(position_of("tupNested"), 0));
    let Answer::Known(nested) = nested else {
        panic!("expected a known type for `tupNested`, got {nested:?}");
    };
    let expected = table.tuple(vec![pair, boolean], false);
    assert_eq!(nested, expected);

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn a_readonly_tuple_differs_from_a_mutable_one_with_the_same_elements() {
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

    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);

    let frozen = api.type_at(&mut table, path, Span::new(position_of("tupRo"), 0));
    let Answer::Known(frozen) = frozen else {
        panic!("expected a known type for `tupRo`, got {frozen:?}");
    };
    let expected = table.tuple(vec![number, text], true);
    assert_eq!(frozen, expected);

    let mutable = api.type_at(&mut table, path, Span::new(position_of("tup"), 0));
    let Answer::Known(mutable) = mutable else {
        panic!("expected a known type for `tup`, got {mutable:?}");
    };
    assert_ne!(frozen, mutable);

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn a_tuple_with_a_rest_or_optional_element_is_refused() {
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

    for (name, needle) in [
        ("tupRest", "rest or variadic"),
        ("tupLeading", "rest or variadic"),
        ("tupRoRest", "rest or variadic"),
        ("tupOpt", "optional"),
    ] {
        let answer = api.type_at(&mut table, path, Span::new(position_of(name), 0));
        let Answer::Unsupported(reason) = answer else {
            panic!("`{name}` must not be guessed, got {answer:?}");
        };
        assert!(
            reason.contains(needle) && reason.contains("length"),
            "`{name}` should name what the API withheld, got: {reason}"
        );
    }

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn a_tuple_element_that_admits_undefined_keeps_its_literal_arity() {
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

    let answer = api.type_at(&mut table, path, Span::new(position_of("tupUndef"), 0));
    let Answer::Known(tuple) = answer else {
        panic!("an explicit `| undefined` element has a literal arity, got {answer:?}");
    };
    let TypeKind::Tuple { elements, .. } = table.kind(tuple).clone() else {
        panic!("expected a tuple, got {:?}", table.kind(tuple));
    };
    assert_eq!(elements.len(), 2);
    assert!(table.admits_undefined(elements[1]));

    let optional = api.type_at(&mut table, path, Span::new(position_of("tupOptional"), 0));
    let Answer::Unsupported(reason) = optional else {
        panic!("an optional element must not be guessed, got {optional:?}");
    };
    assert!(reason.contains("optional"), "unexpected: {reason}");
    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn without_the_array_library_a_tuple_is_refused_rather_than_called_readonly() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_with("\"strict\": true, \"noLib\": true");
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = file.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(&mut table, path, Span::new(position_of("tup"), 0));
    let Answer::Unsupported(reason) = answer else {
        panic!("a mutable tuple with no array members must not be guessed, got {answer:?}");
    };
    assert!(
        reason.contains("array members") && reason.contains("push"),
        "the refusal should name the missing evidence, got: {reason}"
    );

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn a_generic_signature_keeps_its_type_parameters() {
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

    let answer = api.type_at(&mut table, path, Span::new(position_of("firstOf"), 0));
    let Answer::Known(generic) = answer else {
        panic!("expected a known type for `firstOf`, got {answer:?}");
    };
    let TypeKind::Function {
        type_parameters,
        parameters,
        returns,
    } = table.kind(generic).clone()
    else {
        panic!("expected a function, got {:?}", table.kind(generic));
    };
    assert_eq!(type_parameters.len(), 1);
    let declared = type_parameters[0];
    assert!(matches!(table.kind(declared), TypeKind::TypeParameter(_)));

    assert_eq!(parameters.len(), 1);
    assert_eq!(
        table.kind(parameters[0].type_id),
        &TypeKind::ReadonlyArray(declared),
        "the parameter should be an array of the same declared parameter"
    );
    assert!(table.admits_undefined(returns));

    let two = api.type_at(&mut table, path, Span::new(position_of("pick"), 0));
    let Answer::Known(two) = two else {
        panic!("expected a known type for `pick`, got {two:?}");
    };
    let TypeKind::Function {
        type_parameters, ..
    } = table.kind(two).clone()
    else {
        panic!("expected a function, got {:?}", table.kind(two));
    };
    assert_eq!(type_parameters.len(), 2);
    assert_ne!(
        type_parameters[0], type_parameters[1],
        "two declared parameters must stay distinct"
    );
    std::fs::remove_dir_all(&project).ok();
}
