use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use savitar_diagnostics::Span;
use savitar_typeinfo::{Answer, TypeProvider};
use savitar_typeinfo_tsapi::TsApi;
use savitar_types::{
    Field, IndexSignature, Parameter, StaticSide, TypeKind, TypeTable, assignable,
};

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
    "declare const explicitUndef: (a: number | undefined) => void;\n",
    "declare const obj: { value: number; run(flag: boolean): void };\n",
    "declare function firstOf<T>(items: readonly T[]): T | undefined;\n",
    "declare function pick<K, V>(k: K, v: V): V;\n",
    "declare const loose2: (x: any) => void;\n",
    "declare const merged: { a: number } & { b: string };\n",
    "declare const clashing: { a: number } & { a: string };\n",
    "interface Keyed { method: \"GET\" | \"POST\"; path: string }\n",
    "declare const keyedPath: Keyed[\"path\"];\n",
    "declare const keyedByAnyKey: Keyed[keyof Keyed];\n",
    "declare function indexAt<T, K extends keyof T>(t: T, k: K): T[K];\n",
    "declare function nameOf<T>(t: T): T extends string ? \"text\" : \"other\";\n",
    "declare const pattern: RegExp;\n",
    "declare const execArray: RegExpExecArray;\n",
    "interface Labelled extends Array<number> { label: string }\n",
    "declare const labelled: Labelled;\n",
    "declare const pending: Promise<number>;\n",
    "declare const asyncFn: () => Promise<string>;\n",
    "declare const joinAll: (first: string, ...rest: string[]) => string;\n",
    "declare const frozen: { x: number; readonly y: string };\n",
    "declare const frozenDict: { readonly [k: string]: number };\n",
    "declare const mappedFrozen: Readonly<{ p: number }>;\n",
    "const sum = 1 + 1;\n",
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

    let written_any = api.type_at(&mut table, path, Span::new(position_of("loose2"), 0));
    let Answer::Known(written_any) = written_any else {
        panic!("a declared `any` arrives as `unknown`, got {written_any:?}");
    };
    let TypeKind::Function { parameters, .. } = table.kind(written_any).clone() else {
        panic!("expected a function, got {:?}", table.kind(written_any));
    };
    assert_eq!(
        table.kind(parameters[0].type_id),
        &TypeKind::Unknown,
        "`any` must arrive as `unknown`, never as a proof"
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
        readonly: false,
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
            readonly: false,
        },
        Field {
            name: "y".to_string(),
            type_id: text_kind,
            optional: false,
            method: false,
            readonly: false,
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
        readonly: false,
    }]);
    let expected = table.object(vec![Field {
        name: "inner".to_string(),
        type_id: inner,
        optional: false,
        method: false,
        readonly: false,
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
            readonly: false,
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
            Parameter {
                type_id: number,
                rest: false,
                optional: false,
            },
            Parameter {
                type_id: text_kind,
                rest: false,
                optional: false,
            },
        ],
        boolean,
    );
    assert_eq!(function, expected, "a function type must keep its shape");

    let loose = api.type_at(&mut table, path, Span::new(position_of("loose"), 0));
    let Answer::Known(loose) = loose else {
        panic!("an optional parameter is modelled by its type, got {loose:?}");
    };
    let explicit = api.type_at(&mut table, path, Span::new(position_of("explicitUndef"), 0));
    let Answer::Known(explicit) = explicit else {
        panic!("expected a known type for `explicitUndef`, got {explicit:?}");
    };
    assert_ne!(
        loose, explicit,
        "the printed declaration marks `a?: T`, so it no longer collapses with `a: T | undefined`"
    );
    let TypeKind::Function {
        parameters: loose_parameters,
        ..
    } = table.kind(loose).clone()
    else {
        panic!("expected a function, got {:?}", table.kind(loose));
    };
    let TypeKind::Function {
        parameters: explicit_parameters,
        ..
    } = table.kind(explicit).clone()
    else {
        panic!("expected a function, got {:?}", table.kind(explicit));
    };
    assert!(loose_parameters[0].optional);
    assert!(!explicit_parameters[0].optional);
    assert_eq!(
        loose_parameters[0].type_id, explicit_parameters[0].type_id,
        "both still carry `T | undefined` as the type; only the mark differs"
    );

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
    let run_signature = table.function(
        vec![Parameter {
            type_id: boolean,
            rest: false,
            optional: false,
        }],
        void,
    );
    assert_eq!(run.type_id, run_signature);

    let merged = api.type_at(&mut table, path, Span::new(position_of("merged"), 0));
    let Answer::Known(merged) = merged else {
        panic!("expected a known type for `merged`, got {merged:?}");
    };
    let expected = table.object(vec![
        Field {
            name: "a".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "b".to_string(),
            type_id: text_kind,
            optional: false,
            method: false,
            readonly: false,
        },
    ]);
    assert_eq!(
        merged, expected,
        "an intersection of object shapes merges into one shape"
    );

    let clashing = api.type_at(&mut table, path, Span::new(position_of("clashing"), 0));
    assert!(
        matches!(clashing, Answer::Unsupported(_)),
        "a field declared twice with different types must not be merged, got {clashing:?}"
    );

    let pending = api.type_at(&mut table, path, Span::new(position_of("pending"), 0));
    let Answer::Known(pending) = pending else {
        panic!("expected a known type for `pending`, got {pending:?}");
    };
    assert_eq!(
        table.kind(pending),
        &TypeKind::Promise(number),
        "a promise is its own kind, not an object of `then` and `catch`"
    );

    let returning = api.type_at(&mut table, path, Span::new(position_of("asyncFn"), 0));
    let Answer::Known(returning) = returning else {
        panic!("expected a known type for `asyncFn`, got {returning:?}");
    };
    let TypeKind::Function { returns, .. } = table.kind(returning).clone() else {
        panic!("expected a function, got {:?}", table.kind(returning));
    };
    assert_eq!(table.kind(returns), &TypeKind::Promise(text_kind));

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

#[test]
fn a_concrete_indexed_access_arrives_already_resolved_by_the_checker() {
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

    for name in ["keyedPath", "keyedByAnyKey"] {
        let answer = api.type_at(&mut table, path, Span::new(position_of(name), 0));
        let Answer::Known(resolved) = answer else {
            panic!("`{name}` should reach Savitar already resolved, got {answer:?}");
        };
        assert_eq!(
            table.kind(resolved),
            &TypeKind::String,
            "`{name}` should be the property type, not an indexed access"
        );
    }

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn an_indexed_access_over_type_parameters_arrives_deferred_with_both_operands() {
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

    let answer = api.type_at(&mut table, path, Span::new(position_of("indexAt"), 0));
    let Answer::Known(signature) = answer else {
        panic!("expected a known signature for `indexAt`, got {answer:?}");
    };
    let TypeKind::Function {
        parameters,
        returns,
        ..
    } = table.kind(signature).clone()
    else {
        panic!("expected a function, got {:?}", table.kind(signature));
    };
    let TypeKind::IndexedAccess { object, index } = table.kind(returns).clone() else {
        panic!(
            "`T[K]` should arrive as a deferred indexed access, got {:?}",
            table.kind(returns)
        );
    };
    assert!(
        matches!(table.kind(object), TypeKind::TypeParameter(_)),
        "the object side is the type parameter the signature declares, got {:?}",
        table.kind(object)
    );
    assert!(
        matches!(table.kind(index), TypeKind::TypeParameter(_)),
        "the index side stays a type parameter, got {:?}",
        table.kind(index)
    );
    assert_eq!(
        parameters[0].type_id, object,
        "the deferred access points at the same type parameter the first parameter has"
    );

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn an_interface_that_extends_array_keeps_its_own_and_its_inherited_members() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_with("\"strict\": true, \"target\": \"es2018\"");
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = file.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(&mut table, path, Span::new(position_of("execArray"), 0));
    let Answer::Known(resolved) = answer else {
        panic!("`RegExpExecArray` has no member Savitar cannot model here, got {answer:?}");
    };
    let TypeKind::Object { fields, indexes } = table.kind(resolved).clone() else {
        panic!(
            "an interface that extends `Array` is a shape, not an array: {:?}",
            table.kind(resolved)
        );
    };

    let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    for own in ["index", "input", "0", "groups"] {
        assert!(names.contains(&own), "missing its own `{own}`: {names:?}");
    }
    for inherited in ["length", "push", "map", "concat"] {
        assert!(
            names.contains(&inherited),
            "missing the inherited `{inherited}`: {names:?}"
        );
    }

    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    assert_eq!(
        indexes,
        vec![IndexSignature {
            key: number,
            value: text,
            readonly: false,
        }],
        "the element type comes from the index signature `Array` declares"
    );

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn regexp_resolves_whole_once_the_library_predates_flat() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_with("\"strict\": true, \"target\": \"es2018\"");
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = file.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(&mut table, path, Span::new(position_of("pattern"), 0));
    let Answer::Known(resolved) = answer else {
        panic!("only `flat` blocks `RegExp`, and es2018 has no `flat`, got {answer:?}");
    };
    let TypeKind::Object { fields, .. } = table.kind(resolved).clone() else {
        panic!("`RegExp` is a shape, got {:?}", table.kind(resolved));
    };
    let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    for member in ["exec", "test", "source", "lastIndex"] {
        assert!(names.contains(&member), "missing `{member}`: {names:?}");
    }

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn dropping_the_inherited_members_would_accept_objects_the_interface_rejects() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_with("\"strict\": true, \"target\": \"es2018\"");
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = file.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(&mut table, path, Span::new(position_of("labelled"), 0));
    let Answer::Known(whole) = answer else {
        panic!("`Labelled` has no member Savitar cannot model here, got {answer:?}");
    };
    let TypeKind::Object { fields, indexes } = table.kind(whole).clone() else {
        panic!("`Labelled` is a shape, got {:?}", table.kind(whole));
    };

    let label = fields
        .iter()
        .find(|field| field.name == "label")
        .expect("`Labelled` declares `label`")
        .clone();
    let own_members_only = table.object_with_indexes(vec![label], indexes);

    assert!(
        assignable(&table, whole, own_members_only),
        "the whole shape still fits a shape holding only `label`"
    );
    assert!(
        !assignable(&table, own_members_only, whole),
        "a shape holding only `label` must not pass for `Labelled`, or the inherited members would stop being required"
    );

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn the_error_type_is_reported_as_no_answer_rather_than_unknown() {
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

    let operator = SOURCE
        .find("1 + 1")
        .expect("the fixture should hold an operator")
        + 2;
    let answer = api.type_at(
        &mut table,
        path,
        Span::new(u32::try_from(operator).expect("fixture is small"), 0),
    );
    assert_eq!(
        answer,
        Answer::OutOfScope,
        "the checker's error type means there is no node here, not that the type is unknown"
    );
    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn the_readonly_mark_of_a_field_comes_from_the_declaration() {
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

    let shape = api.type_at(&mut table, path, Span::new(position_of("frozen"), 0));
    let Answer::Known(shape) = shape else {
        panic!("expected a known type for `frozen`, got {shape:?}");
    };
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let expected = table.object(vec![
        Field {
            name: "x".to_string(),
            type_id: number,
            optional: false,
            method: false,
            readonly: false,
        },
        Field {
            name: "y".to_string(),
            type_id: text,
            optional: false,
            method: false,
            readonly: true,
        },
    ]);
    assert_eq!(shape, expected);

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn a_mapped_readonly_shape_marks_every_field_readonly() {
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

    let shape = api.type_at(&mut table, path, Span::new(position_of("mappedFrozen"), 0));
    let Answer::Known(shape) = shape else {
        panic!("expected a known type for `mappedFrozen`, got {shape:?}");
    };
    let number = table.intern(TypeKind::Number);
    let expected = table.object(vec![Field {
        name: "p".to_string(),
        type_id: number,
        optional: false,
        method: false,
        readonly: true,
    }]);
    assert_eq!(shape, expected);

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn a_readonly_index_signature_arrives_readonly() {
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

    let frozen = api.type_at(&mut table, path, Span::new(position_of("frozenDict"), 0));
    let Answer::Known(frozen) = frozen else {
        panic!("expected a known type for `frozenDict`, got {frozen:?}");
    };
    let mutable = api.type_at(&mut table, path, Span::new(position_of("dict"), 0));
    let Answer::Known(mutable) = mutable else {
        panic!("expected a known type for `dict`, got {mutable:?}");
    };
    let number = table.intern(TypeKind::Number);
    let text = table.intern(TypeKind::String);
    let expected = table.object_with_indexes(
        Vec::new(),
        vec![IndexSignature {
            key: text,
            value: number,
            readonly: true,
        }],
    );
    assert_eq!(frozen, expected);
    assert_ne!(
        frozen, mutable,
        "a readonly index signature is not the same type as a mutable one"
    );
    assert!(
        !assignable(&table, frozen, mutable),
        "a readonly index signature must not pass for a mutable one, or a write would reach it"
    );

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn a_rest_parameter_is_marked_as_one() {
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

    let signature = api.type_at(&mut table, path, Span::new(position_of("joinAll"), 0));
    let Answer::Known(signature) = signature else {
        panic!("expected a known type for `joinAll`, got {signature:?}");
    };
    let TypeKind::Function { parameters, .. } = table.kind(signature).clone() else {
        panic!("expected a function, got {:?}", table.kind(signature));
    };
    let text = table.intern(TypeKind::String);
    let texts = table.intern(TypeKind::Array(text));
    assert_eq!(
        parameters,
        vec![
            Parameter {
                type_id: text,
                rest: false,
                optional: false
            },
            Parameter {
                type_id: texts,
                rest: true,
                optional: false
            },
        ],
        "the rest parameter keeps the array type it declares and says it is a rest"
    );

    let plain = api.type_at(&mut table, path, Span::new(position_of("fn"), 0));
    let Answer::Known(plain) = plain else {
        panic!("expected a known type for `fn`, got {plain:?}");
    };
    let TypeKind::Function { parameters, .. } = table.kind(plain).clone() else {
        panic!("expected a function, got {:?}", table.kind(plain));
    };
    assert!(
        parameters.iter().all(|parameter| !parameter.rest),
        "a signature with no rest parameter marks none"
    );

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn a_conditional_type_arrives_deferred_with_its_four_operands() {
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

    let answer = api.type_at(&mut table, path, Span::new(position_of("nameOf"), 0));
    let Answer::Known(signature) = answer else {
        panic!("expected a known signature for `nameOf`, got {answer:?}");
    };
    let TypeKind::Function {
        parameters,
        returns,
        ..
    } = table.kind(signature).clone()
    else {
        panic!("expected a function, got {:?}", table.kind(signature));
    };
    let TypeKind::Conditional {
        check,
        bound,
        when_true,
        when_false,
    } = table.kind(returns).clone()
    else {
        panic!(
            "a conditional type should arrive deferred, got {:?}",
            table.kind(returns)
        );
    };
    assert!(
        matches!(table.kind(check), TypeKind::TypeParameter(_)),
        "the check side is a type parameter, got {:?}",
        table.kind(check)
    );
    assert_ne!(
        check, parameters[0].type_id,
        "measured: the checker hands the conditional a type parameter of its own, not the one the signature declares, so a call site cannot bind it"
    );
    assert_eq!(table.kind(bound), &TypeKind::String);
    assert_eq!(
        table.kind(when_true),
        &TypeKind::StringLiteral("text".to_string())
    );
    assert_eq!(
        table.kind(when_false),
        &TypeKind::StringLiteral("other".to_string())
    );

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn the_refusal_behind_regexp_names_the_recursion_it_cannot_unroll() {
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

    let answer = api.type_at(&mut table, path, Span::new(position_of("pattern"), 0));
    let Answer::Unsupported(reason) = answer else {
        panic!("`RegExp` should still be refused, got {answer:?}");
    };
    assert!(
        reason.starts_with("`exec`: `indices`:") && reason.contains("nests deeper"),
        "the refusal should name the member path and say the walk ran out of depth, got {reason}"
    );

    std::fs::remove_dir_all(&project).ok();
}

fn scratch_project_of(files: &[(&str, &str)]) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before the epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("savitar-merged-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("could not create the scratch project");
    std::fs::write(
        dir.join("tsconfig.json"),
        "{ \"compilerOptions\": { \"strict\": true }, \"include\": [\".\"] }",
    )
    .expect("could not write tsconfig.json");
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("could not write a fixture file");
    }
    dir
}

fn position_in(text: &str, needle: &str) -> u32 {
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("needle not in the fixture: {needle}"));
    u32::try_from(at + needle.len() - 1).expect("fixture is small")
}

fn literal_names(table: &TypeTable, id: savitar_types::TypeId) -> Vec<String> {
    let members = match table.kind(id).clone() {
        TypeKind::Union(members) => members,
        _ => vec![id],
    };
    let mut names: Vec<String> = members
        .iter()
        .map(|&member| match table.kind(member) {
            TypeKind::StringLiteral(text) => text.clone(),
            other => format!("{other:?}"),
        })
        .collect();
    names.sort();
    names
}

const DECLARED_TWICE: &str = concat!(
    "interface Config { host: string }\n",
    "interface Config { port: number }\n",
    "declare const config: Config;\n",
);

#[test]
fn an_interface_declared_twice_arrives_with_the_members_of_both_declarations() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_of(&[("a.ts", DECLARED_TWICE)]);
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = file.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(
        &mut table,
        path,
        Span::new(position_in(DECLARED_TWICE, "const config"), 0),
    );
    let Answer::Known(shape) = answer else {
        panic!("expected a known type for `config`, got {answer:?}");
    };
    let TypeKind::Object { fields, .. } = table.kind(shape).clone() else {
        panic!("expected an object, got {:?}", table.kind(shape));
    };
    let mut named: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    named.sort_unstable();
    assert_eq!(
        named,
        vec!["host", "port"],
        "the provider must fuse both declarations itself"
    );

    std::fs::remove_dir_all(&project).ok();
}

const UNION_IN_SEPARATE_DECLARATIONS: &str = concat!(
    "interface Shapes { circle: { kind: \"circle\"; radius: number } }\n",
    "interface Shapes { square: { kind: \"square\"; side: number } }\n",
    "type Shape = Shapes[keyof Shapes];\n",
    "declare const tag: Shape[\"kind\"];\n",
);

#[test]
fn a_union_assembled_in_separate_declarations_arrives_with_every_member() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_of(&[("a.ts", UNION_IN_SEPARATE_DECLARATIONS)]);
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = file.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(
        &mut table,
        path,
        Span::new(position_in(UNION_IN_SEPARATE_DECLARATIONS, "const tag"), 0),
    );
    let Answer::Known(tag) = answer else {
        panic!("expected a known type for `tag`, got {answer:?}");
    };
    assert_eq!(
        literal_names(&table, tag),
        vec!["circle".to_string(), "square".to_string()],
        "a discriminant built over two declarations must arrive whole"
    );

    std::fs::remove_dir_all(&project).ok();
}

const FUSED_FROM_ANOTHER_FILE: &str = concat!(
    "interface Shapes { circle: { kind: \"circle\"; radius: number } }\n",
    "declare const tag: Shapes[keyof Shapes][\"kind\"];\n",
);

#[test]
fn a_declaration_in_an_unopened_file_still_reaches_the_fused_type() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_of(&[
        ("a.ts", FUSED_FROM_ANOTHER_FILE),
        (
            "b.ts",
            "interface Shapes { square: { kind: \"square\"; side: number } }\n",
        ),
    ]);
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = file.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(
        &mut table,
        path,
        Span::new(position_in(FUSED_FROM_ANOTHER_FILE, "const tag"), 0),
    );
    let Answer::Known(tag) = answer else {
        panic!("expected a known type for `tag`, got {answer:?}");
    };
    assert_eq!(
        literal_names(&table, tag),
        vec!["circle".to_string(), "square".to_string()],
        "the provider's program spans the project, so no module graph of ours is needed"
    );

    std::fs::remove_dir_all(&project).ok();
}

const NEVER_OPENED: &str = concat!(
    "export const limit = 8;\n",
    "export function under(count: number): boolean {\n",
    "  return count < limit;\n",
    "}\n",
);

#[test]
fn a_position_in_a_file_the_snapshot_never_opened_still_answers() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_of(&[
        ("opened.ts", "export const here = 1;\n"),
        ("closed.ts", NEVER_OPENED),
    ]);
    let opened = project.join("opened.ts");
    let closed = project.join("closed.ts");
    let api = TsApi::start(&binary, &project, &[opened.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = closed.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(
        &mut table,
        path,
        Span::new(position_in(NEVER_OPENED, "const limit"), 0),
    );
    let Answer::Known(limit) = answer else {
        panic!("expected a known type for `limit` in the unopened file, got {answer:?}");
    };
    assert_eq!(
        *table.kind(limit),
        TypeKind::NumberLiteral(8.0_f64.to_bits()),
        "a position in a file outside the open list must answer, or the CLI cannot lower the graph"
    );

    let answer = api.type_at(
        &mut table,
        path,
        Span::new(position_in(NEVER_OPENED, "function under"), 0),
    );
    let Answer::Known(under) = answer else {
        panic!("expected a known type for `under` in the unopened file, got {answer:?}");
    };
    let TypeKind::Function {
        parameters,
        returns,
        ..
    } = table.kind(under).clone()
    else {
        panic!("`under` should arrive as a function");
    };
    assert_eq!(parameters.len(), 1);
    assert_eq!(*table.kind(returns), TypeKind::Boolean);

    std::fs::remove_dir_all(&project).ok();
}

fn scratch_project_including_only(folder: &str, files: &[(&str, &str)]) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before the epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("savitar-outside-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(dir.join(folder)).expect("could not create the scratch project");
    std::fs::write(
        dir.join("tsconfig.json"),
        format!("{{ \"compilerOptions\": {{ \"strict\": true }}, \"include\": [\"{folder}\"] }}"),
    )
    .expect("could not write tsconfig.json");
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("could not write a fixture file");
    }
    dir
}

#[test]
fn a_position_in_a_file_the_project_excludes_reports_a_file_the_provider_cannot_find() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_including_only(
        "src",
        &[
            ("src/opened.ts", "export const here = 1;\n"),
            ("outside.ts", NEVER_OPENED),
        ],
    );
    let opened = project.join("src/opened.ts");
    let outside = project.join("outside.ts");
    let api = TsApi::start(&binary, &project, &[opened.as_path()])
        .expect("the provider should start and create a snapshot");
    let mut table = TypeTable::new();

    let answer = api.type_at(
        &mut table,
        outside.to_str().expect("utf-8 path"),
        Span::new(position_in(NEVER_OPENED, "const limit"), 0),
    );
    let Answer::Unsupported(reason) = answer else {
        panic!("a file the tsconfig leaves out should not be typed, got {answer:?}");
    };
    assert!(
        reason.contains("source file not found"),
        "the provider names the file it cannot find; it said: {reason}"
    );

    std::fs::remove_dir_all(&project).ok();
}

const STATIC_MEMBERS: &str = concat!(
    "export class Base {\n",
    "  static tally: string | number;\n",
    "  static readonly label: string = \"b\";\n",
    "  static put(incoming: string | number): void {\n",
    "    Base.tally = incoming;\n",
    "  }\n",
    "}\n",
    "export class Derived extends Base {}\n",
);

#[test]
fn the_constructible_shape_at_a_base_name_carries_the_static_members_of_that_class() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_of(&[("a.ts", STATIC_MEMBERS)]);
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = file.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(
        &mut table,
        path,
        Span::new(position_in(STATIC_MEMBERS, "extends Base"), 0),
    );
    let Answer::Known(constructible) = answer else {
        panic!("expected a known type at the base name, got {answer:?}");
    };
    let TypeKind::Constructor { forms, statics } = table.kind(constructible).clone() else {
        panic!(
            "expected a constructible shape, got {:?}",
            table.kind(constructible)
        );
    };
    assert_eq!(forms.len(), 1, "a class declares one way to construct it");

    let StaticSide::Shaped(shape) = statics else {
        panic!("expected the static side to be shaped, got {statics:?}");
    };
    let TypeKind::Object { fields, .. } = table.kind(shape).clone() else {
        panic!("expected an object shape, got {:?}", table.kind(shape));
    };
    let named: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    assert_eq!(
        named,
        vec!["label", "put", "tally"],
        "the static side holds what the class declares static"
    );

    let tally = fields
        .iter()
        .find(|field| field.name == "tally")
        .expect("the static field is in the shape");
    assert!(!tally.readonly);
    assert!(!tally.method);
    let TypeKind::Union(_) = table.kind(tally.type_id) else {
        panic!(
            "`tally` should arrive as the declared union, got {:?}",
            table.kind(tally.type_id)
        );
    };

    let label = fields
        .iter()
        .find(|field| field.name == "label")
        .expect("the static readonly field is in the shape");
    assert!(label.readonly, "`static readonly` arrives readonly");

    let put = fields
        .iter()
        .find(|field| field.name == "put")
        .expect("the static method is in the shape");
    assert!(put.method, "a static method arrives as a method");

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn the_prototype_the_checker_synthesises_is_left_out_of_the_static_side() {
    let Some(binary) = provider_binary() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let project = scratch_project_of(&[("a.ts", STATIC_MEMBERS)]);
    let file = project.join("a.ts");
    let api = TsApi::start(&binary, &project, &[file.as_path()])
        .expect("the provider should start and create a snapshot");
    let path = file.to_str().expect("utf-8 path");
    let mut table = TypeTable::new();

    let answer = api.type_at(
        &mut table,
        path,
        Span::new(position_in(STATIC_MEMBERS, "extends Base"), 0),
    );
    let Answer::Known(constructible) = answer else {
        panic!("expected a known type at the base name, got {answer:?}");
    };
    let Some(StaticSide::Shaped(shape)) = table.static_side_of(constructible).cloned() else {
        panic!("expected the static side to be shaped");
    };
    let TypeKind::Object { fields, .. } = table.kind(shape).clone() else {
        panic!("expected an object shape, got {:?}", table.kind(shape));
    };
    assert!(
        !fields.iter().any(|field| field.name == "prototype"),
        "`prototype` holds the instance shape, which the instance rule already relates, and a \
         mutable `prototype` would demand invariance between instance shapes"
    );

    std::fs::remove_dir_all(&project).ok();
}
