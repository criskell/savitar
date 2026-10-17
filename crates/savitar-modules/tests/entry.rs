use std::path::{Path, PathBuf};

use savitar_modules::{ModuleGraph, WhyUncheckedCodeReachesTheExports};

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

#[test]
fn the_file_savitar_is_asked_about_stands_on_the_entry_surface() {
    let root = a_server_project(
        "entered-here",
        &[(
            "server.ts",
            "export function port(): number {\n  return 8080;\n}\n",
        )],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["server.ts"]));
    let server = graph
        .id_of(&root.join("server.ts"))
        .expect("`server.ts` should be in the graph");

    assert_eq!(
        graph.on_the_entry_surface(server),
        Some(WhyUncheckedCodeReachesTheExports::TheProgramIsEnteredHere)
    );
}

#[test]
fn a_module_only_checked_code_imports_is_not_on_the_entry_surface() {
    let root = a_server_project(
        "imported-only",
        &[
            (
                "server.ts",
                "import { weight } from \"./inner.ts\";\nexport const used = weight;\n",
            ),
            ("inner.ts", "export const weight = 3;\n"),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["server.ts"]));
    let inner = graph
        .id_of(&root.join("inner.ts"))
        .expect("`inner.ts` should be in the graph");

    assert_eq!(graph.on_the_entry_surface(inner), None);
}

#[test]
fn a_module_nobody_in_the_program_imports_stands_on_the_entry_surface() {
    let root = a_server_project(
        "nobody-imports",
        &[
            (
                "server.ts",
                "import { weight } from \"./inner.ts\";\nexport const used = weight;\n",
            ),
            ("inner.ts", "export const weight = 3;\n"),
            (
                "routes.ts",
                "export function route(): string {\n  return \"/\";\n}\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["server.ts", "routes.ts"]));
    let routes = graph
        .id_of(&root.join("routes.ts"))
        .expect("`routes.ts` should be in the graph");

    assert_eq!(
        graph.on_the_entry_surface(routes),
        Some(WhyUncheckedCodeReachesTheExports::TheProgramIsEnteredHere)
    );
}

#[test]
fn the_entry_surface_holds_every_module_the_program_is_entered_through() {
    let root = a_server_project(
        "whole-surface",
        &[
            (
                "server.ts",
                "import { weight } from \"./inner.ts\";\nexport const used = weight;\n",
            ),
            ("inner.ts", "export const weight = 3;\n"),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["server.ts"]));

    assert_eq!(graph.entry_surface().len(), 1);
}
