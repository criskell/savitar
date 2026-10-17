use std::path::{Path, PathBuf};

use savitar_modules::{ModuleGraph, refusals_on_the_entry_surface, types_declared_behind_no_edge};

const A_REAL_PACKAGE_JSON: &str = "{\n  \"name\": \"service\",\n  \"private\": true,\n  \
                                   \"type\": \"module\",\n  \"dependencies\": { \"widget\": \
                                   \"1.4.2\" }\n}\n";

const THE_PACKAGE_MANIFEST: &str = "{\n  \"name\": \"widget\",\n  \"version\": \"1.4.2\",\n  \
                                    \"types\": \"index.d.ts\",\n  \"main\": \"index.js\"\n}\n";

const THE_PACKAGE_TYPES: &str = "export interface WidgetRequest {\n  weight: number;\n}\n";

const THE_PACKAGE_CODE: &str = "export function widgetName(request) {\n  return \
                                request.weight;\n}\n";

const A_STANDARD_LIBRARY_GLOBAL_SCRIPT: &str = "interface WidgetMap<K, V> {\n  size: number;\n}\n";

fn a_server_project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the project directory should be creatable");
    let mut every: Vec<(&str, &str)> = vec![
        ("package.json", A_REAL_PACKAGE_JSON),
        ("node_modules/widget/package.json", THE_PACKAGE_MANIFEST),
        ("node_modules/widget/index.d.ts", THE_PACKAGE_TYPES),
        ("node_modules/widget/index.js", THE_PACKAGE_CODE),
        (
            "node_modules/typescript/lib/lib.es5.d.ts",
            A_STANDARD_LIBRARY_GLOBAL_SCRIPT,
        ),
    ];
    every.extend_from_slice(files);
    for (path, text) in &every {
        let at = root.join(path);
        if let Some(parent) = at.parent() {
            std::fs::create_dir_all(parent).expect("the parent directory should be creatable");
        }
        std::fs::write(&at, text).expect("the file should be writable");
    }
    root.canonicalize()
        .expect("the project root should resolve")
}

fn entries(root: &Path, names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(|name| root.join(name)).collect()
}
fn refusals(root: &Path, asked_about: &[&str]) -> Vec<String> {
    let graph = ModuleGraph::build(root, &entries(root, asked_about));
    let declared = types_declared_behind_no_edge(root);
    refusals_on_the_entry_surface(&graph, &declared)
        .iter()
        .map(|finding| format!("{} {}", finding.diagnostic.rule, finding.diagnostic.message))
        .collect()
}
#[test]
fn a_parameter_typed_by_an_interface_a_project_declaration_file_declares_is_refused() {
    let root = a_server_project(
        "declared-parameter",
        &[
            (
                "ambient.d.ts",
                "interface Incoming {\n  path: string;\n  weight: number;\n}\n",
            ),
            (
                "handler.ts",
                "export function weightOf(incoming: Incoming): number {\n  return \
                 incoming.weight;\n}\n",
            ),
        ],
    );

    let said = refusals(&root, &["handler.ts"]);

    assert_eq!(
        said.len(),
        1,
        "the declared type comes in with no import to refuse: {said:?}"
    );
    assert!(
        said[0].starts_with("unsupported-declared-type-on-the-entry-surface"),
        "{said:?}"
    );
    assert!(said[0].contains("Incoming"), "{said:?}");
}

#[test]
fn a_parameter_typed_by_a_type_the_module_declares_itself_is_accepted() {
    let root = a_server_project(
        "own-type",
        &[
            (
                "ambient.d.ts",
                "interface Incoming {\n  weight: number;\n}\n",
            ),
            (
                "handler.ts",
                "export interface Checked {\n  weight: number;\n}\n\nexport function \
                 weightOf(checked: Checked): number {\n  return checked.weight;\n}\n",
            ),
        ],
    );

    assert_eq!(refusals(&root, &["handler.ts"]), Vec::<String>::new());
}

#[test]
fn a_parameter_of_a_function_the_module_does_not_export_is_accepted() {
    let root = a_server_project(
        "not-exported",
        &[
            (
                "ambient.d.ts",
                "interface Incoming {\n  weight: number;\n}\n",
            ),
            (
                "handler.ts",
                "function weightOf(incoming: Incoming): number {\n  return incoming.weight;\n}\n\
                 \nexport function total(): number {\n  return weightOf({ weight: 1 });\n}\n",
            ),
        ],
    );

    assert_eq!(refusals(&root, &["handler.ts"]), Vec::<String>::new());
}

#[test]
fn a_return_typed_by_a_declared_boundary_is_accepted_because_checked_code_makes_the_value() {
    let root = a_server_project(
        "declared-return",
        &[
            (
                "ambient.d.ts",
                "interface Incoming {\n  weight: number;\n}\n",
            ),
            (
                "handler.ts",
                "export function blank(): Incoming {\n  return { weight: 0 };\n}\n",
            ),
        ],
    );

    assert_eq!(refusals(&root, &["handler.ts"]), Vec::<String>::new());
}

#[test]
fn a_parameter_typed_by_a_declared_boundary_is_accepted_where_only_checked_code_calls_it() {
    let root = a_server_project(
        "behind-the-surface",
        &[
            (
                "ambient.d.ts",
                "interface Incoming {\n  weight: number;\n}\n",
            ),
            (
                "server.ts",
                "import { weightOf } from \"./inner.ts\";\nexport const used = weightOf({ \
                 weight: 1 });\n",
            ),
            (
                "inner.ts",
                "export function weightOf(incoming: Incoming): number {\n  return \
                 incoming.weight;\n}\n",
            ),
        ],
    );

    assert_eq!(refusals(&root, &["server.ts"]), Vec::<String>::new());
}

#[test]
fn a_declaration_file_that_exports_is_a_module_whose_names_need_an_import() {
    let root = a_server_project(
        "declaration-module",
        &[
            (
                "shapes.d.ts",
                "export interface Incoming {\n  weight: number;\n}\n",
            ),
            (
                "handler.ts",
                "export function plain(weight: number): number {\n  return weight;\n}\n",
            ),
        ],
    );

    let declared = types_declared_behind_no_edge(&root);

    assert!(
        declared.declaring("Incoming").is_none(),
        "a declaration file with exports is a module, and its names come in by an import the \
         boundary rule already refuses"
    );
}

#[test]
fn a_parameter_typed_by_a_global_a_project_declaration_file_augments_is_refused() {
    let root = a_server_project(
        "augmented-global",
        &[
            (
                "augment.d.ts",
                "export {};\n\ndeclare global {\n  interface Incoming {\n    weight: number;\n  \
                 }\n}\n",
            ),
            (
                "handler.ts",
                "export function weightOf(incoming: Incoming): number {\n  return \
                 incoming.weight;\n}\n",
            ),
        ],
    );

    let said = refusals(&root, &["handler.ts"]);

    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("Incoming"), "{said:?}");
}

#[test]
fn the_standard_library_and_a_package_are_not_types_declared_behind_no_edge() {
    let root = a_server_project(
        "library-trusted",
        &[(
            "handler.ts",
            "export function tally(values: readonly number[]): Map<number, number> {\n  return \
             new Map();\n}\n",
        )],
    );

    let declared = types_declared_behind_no_edge(&root);

    assert!(
        declared.declaring("WidgetMap").is_none(),
        "the standard library is a global script too, so reading `node_modules` would make every \
         parameter typed by `Map` or `Promise` a refusal"
    );
    assert!(declared.declaring("string").is_none());
    assert!(
        declared.declaring("WidgetRequest").is_none(),
        "a package is a declared boundary the edge rule refuses, and the walk never reads \
         `node_modules`"
    );
    assert_eq!(refusals(&root, &["handler.ts"]), Vec::<String>::new());
}

#[test]
fn a_parameter_typed_by_a_declared_boundary_on_an_exported_arrow_is_refused() {
    let root = a_server_project(
        "exported-arrow",
        &[
            (
                "ambient.d.ts",
                "interface Incoming {\n  weight: number;\n}\n",
            ),
            (
                "handler.ts",
                "export const weightOf = (incoming: Incoming): number => incoming.weight;\n",
            ),
        ],
    );

    let said = refusals(&root, &["handler.ts"]);

    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("Incoming"), "{said:?}");
}

#[test]
fn a_parameter_typed_by_a_declared_boundary_on_an_exported_method_is_refused() {
    let root = a_server_project(
        "exported-method",
        &[
            (
                "ambient.d.ts",
                "interface Incoming {\n  weight: number;\n}\n",
            ),
            (
                "handler.ts",
                "export class Router {\n  weightOf(incoming: Incoming): number {\n    return \
                 incoming.weight;\n  }\n}\n",
            ),
        ],
    );

    let said = refusals(&root, &["handler.ts"]);

    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("Incoming"), "{said:?}");
}

#[test]
fn a_declared_boundary_written_inside_a_composed_type_is_refused() {
    let root = a_server_project(
        "composed-type",
        &[
            (
                "ambient.d.ts",
                "interface Incoming {\n  weight: number;\n}\n",
            ),
            (
                "handler.ts",
                "export function total(batch: readonly Incoming[]): number {\n  return \
                 batch.length;\n}\n",
            ),
        ],
    );

    let said = refusals(&root, &["handler.ts"]);

    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("Incoming"), "{said:?}");
}

#[test]
fn a_project_with_no_declaration_file_of_its_own_costs_nothing() {
    let root = a_server_project(
        "no-declarations",
        &[(
            "handler.ts",
            "export function plain(weight: number): number {\n  return weight;\n}\n",
        )],
    );

    let declared = types_declared_behind_no_edge(&root);

    assert!(declared.is_empty());
    assert_eq!(refusals(&root, &["handler.ts"]), Vec::<String>::new());
}
