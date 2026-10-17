use std::ffi::OsString;
use std::path::{Path, PathBuf};

use savitar_diagnostics::SourceFile;
use savitar_ir::{Dominance, Function, Terminator};
use savitar_lower::lower;
use savitar_modules::{Carries, ModuleGraph, ModuleId, Target};
use savitar_sound::{Coverage, check_types};
use savitar_typeinfo_tsapi::TsApi;
use savitar_types::TypeTable;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root should resolve")
}

fn provider_binary(root: &Path) -> Option<OsString> {
    let path = root.join("node_modules/.bin/tsc-rs");
    path.exists().then(|| path.into_os_string())
}

fn corpus_files(corpus: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(corpus)
        .expect("the corpus directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "ts"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "the corpus should hold TypeScript files");
    files
}

fn floor_path(corpus: &Path) -> PathBuf {
    corpus.join("coverage-floor.txt")
}

fn lowering_floor_path(corpus: &Path) -> PathBuf {
    corpus.join("lowering-floor.txt")
}

fn lowering_gap_ceiling_path(corpus: &Path) -> PathBuf {
    corpus.join("lowering-gap-ceiling.txt")
}

fn module_initializer_floor_path(corpus: &Path) -> PathBuf {
    corpus.join("module-initializer-floor.txt")
}

fn dead_end_floor_path(corpus: &Path) -> PathBuf {
    corpus.join("unreachable-terminator-floor.txt")
}

fn module_order_floor_path(corpus: &Path) -> PathBuf {
    corpus.join("module-order-floor.txt")
}

fn named(graph: &ModuleGraph, id: ModuleId) -> String {
    graph.module(id).path.file_name().map_or_else(
        || "an unnamed module".to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn assert_ceiling(ceiling_file: &Path, counted: usize, listed: &[String]) {
    let ceiling: usize = std::fs::read_to_string(ceiling_file)
        .expect("the ceiling should be recorded")
        .trim()
        .parse()
        .expect("the ceiling should be a whole number");

    assert!(
        counted <= ceiling,
        "the ceiling in {} rose from {ceiling} to {counted}; it counts:\n{}",
        ceiling_file.display(),
        listed.join("\n")
    );

    if counted < ceiling {
        panic!(
            "the ceiling in {} fell from {ceiling} to {counted}; it counts:\n{}\nLower it: echo {counted} > {}",
            ceiling_file.display(),
            listed.join("\n"),
            ceiling_file.display()
        );
    }
}

struct Measured {
    coverage: Coverage,
    gaps: Vec<String>,
    initializers: usize,
    dead_ends: Vec<String>,
}

fn measure(directory: &Path, binary: &OsString) -> Measured {
    let files = corpus_files(directory);
    let open: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    let api = TsApi::start(binary, directory, &open).expect("the provider should start");
    let mut table = TypeTable::new();
    let mut coverage = Coverage::default();
    let mut gaps: Vec<String> = Vec::new();

    for path in &files {
        let text = std::fs::read_to_string(path).expect("the file should be readable");
        let name = path.to_str().expect("utf-8 path").to_string();
        let file = SourceFile::new(name, text);
        let report = check_types(&file, &api, &mut table);
        coverage.known += report.coverage.known;
        coverage.unsupported += report.coverage.unsupported;
        coverage.out_of_scope += report.coverage.out_of_scope;
        for diagnostic in &report.diagnostics {
            let at = file.line_col(diagnostic.span.start);
            gaps.push(format!(
                "{}:{}:{} {}",
                file.name(),
                at.line,
                at.column,
                diagnostic.message
            ));
        }
    }

    Measured {
        coverage,
        gaps,
        initializers: 0,
        dead_ends: Vec::new(),
    }
}

fn assert_ratchet(directory: &Path, measured: &Measured, counted: usize) {
    let mut listed = vec![format!(
        "{} of {} bindings are known",
        measured.coverage.known,
        measured.coverage.total()
    )];
    listed.extend(measured.gaps.iter().cloned());
    assert_floor(&floor_path(directory), counted, &listed);
}

fn assert_floor(floor_file: &Path, counted: usize, listed: &[String]) {
    let floor: usize = std::fs::read_to_string(floor_file)
        .expect("the floor should be recorded")
        .trim()
        .parse()
        .expect("the floor should be a whole number");

    assert!(
        counted >= floor,
        "the floor in {} fell from {floor} to {counted}; it counts:\n{}",
        floor_file.display(),
        listed.join("\n")
    );

    if counted > floor {
        panic!(
            "the floor in {} rose from {floor} to {counted}; it counts:\n{}\nRaise it: echo {counted} > {}",
            floor_file.display(),
            listed.join("\n"),
            floor_file.display()
        );
    }
}

fn dead_ends_of(file: &str, function: &Function) -> Vec<String> {
    let dominance = Dominance::of(function);
    function
        .blocks()
        .filter(|(id, block)| {
            matches!(block.terminator, Terminator::Unreachable) && dominance.reachable(*id)
        })
        .map(|(id, _)| {
            format!(
                "{file}: `{}` block {} is reached and ends unreachable",
                function.name(),
                id.index()
            )
        })
        .collect()
}

fn assert_every_binding_is_answered(measured: &Measured) {
    assert_eq!(
        measured.coverage.out_of_scope,
        0,
        "the provider answered nothing for {} bindings; the gaps are:\n{}",
        measured.coverage.out_of_scope,
        measured.gaps.join("\n")
    );
}

fn lower_corpus(directory: &Path, binary: &OsString) -> Measured {
    let files = corpus_files(directory);
    let open: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    let api = TsApi::start(binary, directory, &open).expect("the provider should start");
    let mut table = TypeTable::new();
    let mut coverage = Coverage::default();
    let mut gaps: Vec<String> = Vec::new();
    let mut initializers = 0usize;
    let mut dead_ends: Vec<String> = Vec::new();

    for path in &files {
        let text = std::fs::read_to_string(path).expect("the file should be readable");
        let name = path.to_str().expect("utf-8 path").to_string();
        let file = SourceFile::new(name, text);
        let report = lower(&file, &api, &mut table);
        coverage.known += report.functions.len();
        coverage.unsupported += report.diagnostics.len();
        for function in &report.functions {
            dead_ends.extend(dead_ends_of(file.name(), function));
        }
        if let Some(initializer) = report.initializer.as_ref() {
            initializers += 1;
            assert_eq!(
                savitar_ir::verify(&initializer.function),
                vec![],
                "the module initializer of {} should be valid code",
                file.name()
            );
            dead_ends.extend(dead_ends_of(file.name(), &initializer.function));
            for closure in &initializer.closures {
                dead_ends.extend(dead_ends_of(file.name(), closure));
            }
        }
        for diagnostic in &report.diagnostics {
            let at = file.line_col(diagnostic.span.start);
            gaps.push(format!(
                "{}:{}:{} {}",
                file.name(),
                at.line,
                at.column,
                diagnostic.message
            ));
        }
    }

    dead_ends.sort();

    Measured {
        coverage,
        gaps,
        initializers,
        dead_ends,
    }
}

#[test]
fn type_coverage_of_the_corpus_never_falls() {
    let root = workspace_root();
    let corpus = root.join("test-files/corpus");
    let Some(binary) = provider_binary(&root) else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let measured = measure(&corpus, &binary);
    assert_every_binding_is_answered(&measured);
    assert_ratchet(
        &corpus,
        &measured,
        measured.coverage.percent_known() as usize,
    );
}

#[test]
fn the_number_of_functions_lowered_never_falls() {
    let root = workspace_root();
    let corpus = root.join("test-files/corpus");
    let Some(binary) = provider_binary(&root) else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let measured = lower_corpus(&corpus, &binary);
    assert_floor(
        &lowering_floor_path(&corpus),
        measured.coverage.known,
        &measured.gaps,
    );
    assert_ceiling(
        &lowering_gap_ceiling_path(&corpus),
        measured.gaps.len(),
        &measured.gaps,
    );
}

#[test]
fn the_number_of_reached_blocks_that_end_unreachable_never_changes() {
    let root = workspace_root();
    let corpus = root.join("test-files/corpus");
    let Some(binary) = provider_binary(&root) else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let measured = lower_corpus(&corpus, &binary);
    assert_floor(
        &dead_end_floor_path(&corpus),
        measured.dead_ends.len(),
        &measured.dead_ends,
    );
}

#[test]
fn the_number_of_module_initializers_lowered_never_falls() {
    let root = workspace_root();
    let corpus = root.join("test-files/corpus");
    let Some(binary) = provider_binary(&root) else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let measured = lower_corpus(&corpus, &binary);
    assert_floor(
        &module_initializer_floor_path(&corpus),
        measured.initializers,
        &measured.gaps,
    );
}

#[test]
fn the_number_of_value_imports_the_evaluation_order_honours_never_changes() {
    let root = workspace_root();
    let corpus = root.join("test-files/corpus");
    let files = corpus_files(&corpus);
    let graph = ModuleGraph::build(&corpus, &files);
    let order = graph.evaluation_order();

    let mut honoured: Vec<String> = Vec::new();
    for (importer, module) in graph.modules() {
        for import in &module.imports {
            if import.carries != Carries::Values {
                continue;
            }
            let Target::Module(imported) = import.target else {
                continue;
            };
            let runs_at = order.position_of(importer).unwrap_or_else(|| {
                panic!(
                    "{} has no place in the evaluation order",
                    named(&graph, importer)
                )
            });
            let needed_at = order.position_of(imported).unwrap_or_else(|| {
                panic!(
                    "{} has no place in the evaluation order",
                    named(&graph, imported)
                )
            });
            assert!(
                needed_at < runs_at,
                "{} imports values from {}, and the evaluation order runs it first",
                named(&graph, importer),
                named(&graph, imported)
            );
            honoured.push(format!(
                "{} runs after {}",
                named(&graph, importer),
                named(&graph, imported)
            ));
        }
    }

    honoured.sort();
    assert_floor(&module_order_floor_path(&corpus), honoured.len(), &honoured);
}

#[test]
fn the_number_of_standard_library_types_reached_never_falls() {
    let root = workspace_root();
    let surface = root.join("test-files/lib");
    let Some(binary) = provider_binary(&root) else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let measured = measure(&surface, &binary);
    assert_every_binding_is_answered(&measured);
    assert_ratchet(&surface, &measured, measured.coverage.known);
}
