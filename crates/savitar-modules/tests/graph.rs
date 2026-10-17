use std::path::{Path, PathBuf};

use savitar_modules::{Carries, DeclaredBoundary, ModuleGraph, Target};

fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the project directory should be creatable");
    for (path, text) in files {
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

fn named(graph: &ModuleGraph, root: &Path, name: &str) -> savitar_modules::ModuleId {
    graph
        .id_of(&root.join(name))
        .unwrap_or_else(|| panic!("`{name}` should be in the graph"))
}

fn rules_of(findings: &[savitar_modules::Finding]) -> Vec<&'static str> {
    findings
        .iter()
        .map(|finding| finding.diagnostic.rule)
        .collect()
}

#[test]
fn an_import_of_a_relative_file_becomes_an_edge() {
    let root = project(
        "an-edge",
        &[
            ("config.ts", "export const port = 8080;\n"),
            (
                "service.ts",
                "import { port } from \"./config.ts\";\nexport const used = port;\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["service.ts"]));

    let service = named(&graph, &root, "service.ts");
    let config = named(&graph, &root, "config.ts");
    assert_eq!(graph.module_count(), 2);
    assert_eq!(graph.module(service).imports.len(), 1);
    assert_eq!(
        graph.module(service).imports[0].target,
        Target::Module(config)
    );
}

#[test]
fn a_file_nobody_imports_is_still_in_the_graph() {
    let root = project(
        "lonely",
        &[
            ("alone.ts", "export function alone(): void {}\n"),
            ("other.ts", "export function other(): void {}\n"),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["alone.ts", "other.ts"]));

    assert_eq!(graph.module_count(), 2);
    assert!(
        graph
            .evaluation_order()
            .holds(named(&graph, &root, "alone.ts"))
    );
}

#[test]
fn an_import_without_an_extension_resolves_to_the_typescript_file() {
    let root = project(
        "no-extension",
        &[
            ("config.ts", "export const port = 8080;\n"),
            (
                "service.ts",
                "import { port } from \"./config\";\nexport const used = port;\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["service.ts"]));

    let config = named(&graph, &root, "config.ts");
    let service = named(&graph, &root, "service.ts");
    assert_eq!(
        graph.module(service).imports[0].target,
        Target::Module(config)
    );
}

#[test]
fn a_module_is_evaluated_after_the_modules_it_imports_values_from() {
    let root = project(
        "order",
        &[
            ("config.ts", "export const port = 8080;\n"),
            (
                "service.ts",
                "import { port } from \"./config.ts\";\nexport const used = port;\n",
            ),
            (
                "main.ts",
                "import { used } from \"./service.ts\";\nexport const shown = used;\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["main.ts"]));
    let order = graph.evaluation_order();

    let config = order
        .position_of(named(&graph, &root, "config.ts"))
        .expect("config should be placed");
    let service = order
        .position_of(named(&graph, &root, "service.ts"))
        .expect("service should be placed");
    let main = order
        .position_of(named(&graph, &root, "main.ts"))
        .expect("main should be placed");
    assert!(config < service, "config should run before service");
    assert!(service < main, "service should run before main");
    assert!(order.findings.is_empty());
}

#[test]
fn a_cycle_of_value_imports_is_refused_by_name() {
    let root = project(
        "value-cycle",
        &[
            (
                "left.ts",
                "import { right } from \"./right.ts\";\nexport const left = right;\n",
            ),
            (
                "right.ts",
                "import { left } from \"./left.ts\";\nexport const right = left;\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["left.ts", "right.ts"]));
    let order = graph.evaluation_order();

    assert!(order.evaluated.is_empty());
    assert_eq!(
        rules_of(&order.findings),
        vec!["unsupported-import-cycle", "unsupported-import-cycle"]
    );
}

#[test]
fn a_refused_cycle_names_the_line_and_column_of_the_import_that_closes_it() {
    let root = project(
        "cycle-position",
        &[
            (
                "left.ts",
                "export function first(): void {}\nimport { right } from \"./right.ts\";\nexport const left = right;\n",
            ),
            (
                "right.ts",
                "import { left } from \"./left.ts\";\nexport const right = left;\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["left.ts"]));
    let order = graph.evaluation_order();

    let left = named(&graph, &root, "left.ts");
    let finding = order
        .findings
        .iter()
        .find(|finding| finding.module == left)
        .expect("left should be refused");
    let text = std::fs::read_to_string(root.join("left.ts")).expect("left should be readable");
    let at = finding.diagnostic.span.start as usize;
    assert!(text[at..].starts_with("\"./right.ts\""));
}

#[test]
fn a_cycle_of_type_only_imports_keeps_a_defined_order() {
    let root = project(
        "type-cycle",
        &[
            (
                "left.ts",
                "import type { Right } from \"./right.ts\";\nexport interface Left {\n  right: Right | undefined;\n}\n",
            ),
            (
                "right.ts",
                "import type { Left } from \"./left.ts\";\nexport interface Right {\n  left: Left | undefined;\n}\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["left.ts", "right.ts"]));
    let order = graph.evaluation_order();

    assert_eq!(order.evaluated.len(), 2);
    assert!(order.findings.is_empty());
}

#[test]
fn an_import_whose_specifiers_are_all_types_carries_no_values() {
    let root = project(
        "inline-type-import",
        &[
            ("shape.ts", "export interface Shape {\n  side: number;\n}\n"),
            (
                "use.ts",
                "import { type Shape } from \"./shape.ts\";\nexport function side(shape: Shape): number {\n  return shape.side;\n}\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));

    let use_module = named(&graph, &root, "use.ts");
    assert_eq!(
        graph.module(use_module).imports[0].carries,
        Carries::TypesOnly
    );
}

#[test]
fn an_import_mixing_a_type_and_a_value_carries_values() {
    let root = project(
        "mixed-import",
        &[
            (
                "shape.ts",
                "export interface Shape {\n  side: number;\n}\nexport function make(): Shape {\n  return { side: 1 };\n}\n",
            ),
            (
                "use.ts",
                "import { type Shape, make } from \"./shape.ts\";\nexport function side(): number {\n  const shape: Shape = make();\n  return shape.side;\n}\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));

    let use_module = named(&graph, &root, "use.ts");
    assert_eq!(graph.module(use_module).imports[0].carries, Carries::Values);
}

#[test]
fn an_import_with_no_specifiers_carries_values_because_it_runs_the_module() {
    let root = project(
        "side-effect-import",
        &[
            ("setup.ts", "export const ready = true;\n"),
            ("use.ts", "import \"./setup.ts\";\n"),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));

    let use_module = named(&graph, &root, "use.ts");
    assert_eq!(graph.module(use_module).imports[0].carries, Carries::Values);
}

#[test]
fn a_re_export_from_another_file_becomes_an_edge() {
    let root = project(
        "re-export",
        &[
            ("inner.ts", "export const inner = 1;\n"),
            ("outer.ts", "export { inner } from \"./inner.ts\";\n"),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["outer.ts"]));

    let inner = named(&graph, &root, "inner.ts");
    let outer = named(&graph, &root, "outer.ts");
    assert_eq!(graph.module(outer).imports[0].target, Target::Module(inner));
}

#[test]
fn an_export_star_from_another_file_becomes_an_edge() {
    let root = project(
        "export-star",
        &[
            ("inner.ts", "export const inner = 1;\n"),
            ("outer.ts", "export * from \"./inner.ts\";\n"),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["outer.ts"]));

    let inner = named(&graph, &root, "inner.ts");
    let outer = named(&graph, &root, "outer.ts");
    assert_eq!(graph.module(outer).imports[0].target, Target::Module(inner));
}

#[test]
fn an_import_that_resolves_to_no_file_is_reported_where_the_specifier_sits() {
    let root = project(
        "unresolved",
        &[(
            "use.ts",
            "import { gone } from \"./gone.ts\";\nexport const used = gone;\n",
        )],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));

    assert_eq!(rules_of(graph.findings()), vec!["unresolved-import"]);
    let text = std::fs::read_to_string(root.join("use.ts")).expect("use should be readable");
    let at = graph.findings()[0].diagnostic.span.start as usize;
    assert!(text[at..].starts_with("\"./gone.ts\""));
    assert_eq!(
        graph.module(named(&graph, &root, "use.ts")).imports[0].target,
        Target::Unresolved
    );
}

#[test]
fn a_module_with_an_unresolved_value_import_has_no_place_in_the_order() {
    let root = project(
        "unresolved-order",
        &[(
            "use.ts",
            "import { gone } from \"./gone.ts\";\nexport const used = gone;\n",
        )],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));
    let order = graph.evaluation_order();

    assert!(order.evaluated.is_empty());
    assert_eq!(rules_of(&order.findings), vec!["unsupported-module-order"]);
}

#[test]
fn an_import_of_a_package_is_a_declared_boundary_and_is_not_crawled() {
    let root = project(
        "package-import",
        &[
            (
                "node_modules/widget/package.json",
                "{ \"name\": \"widget\", \"version\": \"1.0.0\", \"types\": \"index.d.ts\", \"main\": \"index.js\" }",
            ),
            (
                "node_modules/widget/index.d.ts",
                "import \"./deep.js\";\nexport declare const widget: number;\n",
            ),
            ("node_modules/widget/deep.d.ts", "export {};\n"),
            (
                "use.ts",
                "import { widget } from \"widget\";\nexport const used = widget;\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));

    assert_eq!(graph.module_count(), 1);
    assert_eq!(
        rules_of(graph.findings()),
        vec!["unsupported-declared-boundary"]
    );
    assert_eq!(
        graph.module(named(&graph, &root, "use.ts")).imports[0].target,
        Target::DeclaredBoundary(DeclaredBoundary::APackage)
    );
}

#[test]
fn a_relative_import_that_lands_on_a_declaration_file_is_a_declared_boundary_too() {
    let root = project(
        "relative-declaration-file",
        &[
            (
                "surface.d.ts",
                "export declare function go(n: number): number;\n",
            ),
            (
                "use.ts",
                "import { go } from \"./surface.js\";\nexport function run(n: number): number {\n  return go(n);\n}\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));

    assert_eq!(graph.module_count(), 1);
    assert_eq!(
        rules_of(graph.findings()),
        vec!["unsupported-declared-boundary"]
    );
    let Target::DeclaredBoundary(DeclaredBoundary::ADeclarationFile(path)) =
        &graph.module(named(&graph, &root, "use.ts")).imports[0].target
    else {
        panic!(
            "expected a declared boundary, got {:?}",
            graph.module(named(&graph, &root, "use.ts")).imports[0].target
        );
    };
    assert!(
        path.ends_with("surface.d.ts"),
        "the refusal should name the declaration file, got {}",
        path.display()
    );
}

#[test]
fn a_module_that_imports_values_from_a_declaration_file_has_no_place_in_the_order() {
    let root = project(
        "declaration-file-order",
        &[
            ("surface.d.ts", "export declare const port: number;\n"),
            (
                "use.ts",
                "import { port } from \"./surface.js\";\nexport const used = port;\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));
    let order = graph.evaluation_order();

    assert!(order.evaluated.is_empty());
    assert_eq!(rules_of(&order.findings), vec!["unsupported-module-order"]);
}

#[test]
fn an_import_type_of_a_declaration_file_is_refused_because_the_type_is_not_verified_either() {
    let root = project(
        "declaration-file-type-only",
        &[
            (
                "surface.d.ts",
                "export interface Shape {\n  size: number;\n}\n",
            ),
            (
                "use.ts",
                "import type { Shape } from \"./surface.js\";\nexport const used: number = 1;\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));
    let order = graph.evaluation_order();

    assert_eq!(
        rules_of(graph.findings()),
        vec!["unsupported-declared-boundary"]
    );
    assert_eq!(order.evaluated.len(), 1);
}

#[test]
fn a_module_that_imports_values_from_a_package_has_no_place_in_the_order() {
    let root = project(
        "package-order",
        &[
            (
                "node_modules/widget/package.json",
                "{ \"name\": \"widget\", \"version\": \"1.0.0\", \"types\": \"index.d.ts\" }",
            ),
            (
                "node_modules/widget/index.d.ts",
                "export declare const widget: number;\n",
            ),
            (
                "use.ts",
                "import { widget } from \"widget\";\nexport const used = widget;\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));
    let order = graph.evaluation_order();

    assert!(order.evaluated.is_empty());
    assert_eq!(rules_of(&order.findings), vec!["unsupported-module-order"]);
}

#[test]
fn a_type_only_import_of_a_package_leaves_the_order_standing() {
    let root = project(
        "package-type-order",
        &[
            (
                "node_modules/widget/package.json",
                "{ \"name\": \"widget\", \"version\": \"1.0.0\", \"types\": \"index.d.ts\" }",
            ),
            (
                "node_modules/widget/index.d.ts",
                "export interface Widget {\n  side: number;\n}\n",
            ),
            (
                "use.ts",
                "import type { Widget } from \"widget\";\nexport function side(widget: Widget): number {\n  return widget.side;\n}\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));
    let order = graph.evaluation_order();

    assert_eq!(order.evaluated.len(), 1);
    assert!(order.findings.is_empty());
}

#[test]
fn a_module_that_imports_a_module_in_a_cycle_has_no_place_in_the_order() {
    let root = project(
        "cycle-reach",
        &[
            (
                "left.ts",
                "import { right } from \"./right.ts\";\nexport const left = right;\n",
            ),
            (
                "right.ts",
                "import { left } from \"./left.ts\";\nexport const right = left;\n",
            ),
            (
                "outer.ts",
                "import { left } from \"./left.ts\";\nexport const outer = left;\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["outer.ts"]));
    let order = graph.evaluation_order();

    assert!(order.evaluated.is_empty());
    let outer = named(&graph, &root, "outer.ts");
    let finding = order
        .findings
        .iter()
        .find(|finding| finding.module == outer)
        .expect("outer should be refused");
    assert_eq!(finding.diagnostic.rule, "unsupported-module-order");
}

#[test]
fn a_module_that_imports_itself_is_a_cycle_of_one() {
    let root = project(
        "self-import",
        &[(
            "loop.ts",
            "import { loop } from \"./loop.ts\";\nexport const loop2 = loop;\nexport const loop = 1;\n",
        )],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["loop.ts"]));
    let order = graph.evaluation_order();

    assert!(order.evaluated.is_empty());
    assert_eq!(rules_of(&order.findings), vec!["unsupported-import-cycle"]);
}

#[test]
fn the_corpus_graph_puts_config_before_service() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-files/corpus")
        .canonicalize()
        .expect("the corpus should resolve");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("the corpus should be readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "ts"))
        .collect();
    files.sort();

    let graph = ModuleGraph::build(&root, &files);
    let order = graph.evaluation_order();

    let config = order
        .position_of(named(&graph, &root, "config.ts"))
        .expect("config should be placed");
    let service = order
        .position_of(named(&graph, &root, "service.ts"))
        .expect("service should be placed");
    assert!(config < service, "config should run before service");
}

#[test]
fn the_corpus_graph_resolves_every_import_and_leaves_no_module_out_of_the_order() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-files/corpus")
        .canonicalize()
        .expect("the corpus should resolve");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("the corpus should be readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "ts"))
        .collect();
    files.sort();

    let graph = ModuleGraph::build(&root, &files);
    let order = graph.evaluation_order();

    assert!(
        graph.findings().is_empty(),
        "{:?}",
        rules_of(graph.findings())
    );
    assert!(order.findings.is_empty(), "{:?}", rules_of(&order.findings));
    assert_eq!(order.evaluated.len(), graph.module_count());
    assert_eq!(graph.module_count(), files.len());
}

#[test]
fn a_type_only_import_does_not_decide_the_evaluation_order() {
    let root = project(
        "type-order",
        &[
            ("shape.ts", "export interface Shape {\n  side: number;\n}\n"),
            (
                "use.ts",
                "import type { Shape } from \"./shape.ts\";\nexport function side(shape: Shape): number {\n  return shape.side;\n}\n",
            ),
        ],
    );

    let graph = ModuleGraph::build(&root, &entries(&root, &["use.ts"]));
    let order = graph.evaluation_order();

    let shape = named(&graph, &root, "shape.ts");
    let use_module = named(&graph, &root, "use.ts");
    assert!(order.holds(shape));
    assert!(order.holds(use_module));
    assert!(order.position_of(use_module) < order.position_of(shape));
}
