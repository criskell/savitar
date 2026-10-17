use std::ffi::OsString;
use std::path::{Path, PathBuf};

use savitar_diagnostics::SourceFile;
use savitar_lower::lower;
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

fn assert_ceiling(ceiling_file: &Path, measured: &Measured, counted: usize) {
    let ceiling: usize = std::fs::read_to_string(ceiling_file)
        .expect("the ceiling should be recorded")
        .trim()
        .parse()
        .expect("the ceiling should be a whole number");

    assert!(
        counted <= ceiling,
        "the gaps rose from {ceiling} to {counted}; they are:\n{}",
        measured.gaps.join("\n")
    );

    if counted < ceiling {
        panic!(
            "the gaps fell from {ceiling} to {counted}.\nLower it: echo {counted} > {}",
            ceiling_file.display()
        );
    }
}

struct Measured {
    coverage: Coverage,
    gaps: Vec<String>,
    initializers: usize,
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
    }
}

fn assert_ratchet(directory: &Path, measured: &Measured, counted: usize) {
    assert_floor(&floor_path(directory), measured, counted);
}

fn assert_floor(floor_file: &Path, measured: &Measured, counted: usize) {
    let floor: usize = std::fs::read_to_string(floor_file)
        .expect("the floor should be recorded")
        .trim()
        .parse()
        .expect("the floor should be a whole number");

    assert!(
        counted >= floor,
        "the ratchet fell from {floor} to {counted} of {}; the gaps are:\n{}",
        measured.coverage.total(),
        measured.gaps.join("\n")
    );

    if counted > floor {
        panic!(
            "the ratchet rose from {floor} to {counted} of {}.\nRaise it: echo {counted} > {}",
            measured.coverage.total(),
            floor_file.display()
        );
    }
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

    for path in &files {
        let text = std::fs::read_to_string(path).expect("the file should be readable");
        let name = path.to_str().expect("utf-8 path").to_string();
        let file = SourceFile::new(name, text);
        let report = lower(&file, &api, &mut table);
        coverage.known += report.functions.len();
        coverage.unsupported += report.diagnostics.len();
        if let Some(initializer) = report.initializer.as_ref() {
            initializers += 1;
            assert_eq!(
                savitar_ir::verify(&initializer.function),
                vec![],
                "the module initializer of {} should be valid code",
                file.name()
            );
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

    Measured {
        coverage,
        gaps,
        initializers,
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
        &measured,
        measured.coverage.known,
    );
    assert_ceiling(
        &lowering_gap_ceiling_path(&corpus),
        &measured,
        measured.gaps.len(),
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
        &measured,
        measured.initializers,
    );
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
