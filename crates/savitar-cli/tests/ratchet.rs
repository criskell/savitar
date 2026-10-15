use std::ffi::OsString;
use std::path::{Path, PathBuf};

use savitar_diagnostics::SourceFile;
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

struct Measured {
    coverage: Coverage,
    gaps: Vec<String>,
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

    Measured { coverage, gaps }
}

fn assert_ratchet(directory: &Path, measured: &Measured, counted: usize) {
    let floor: usize = std::fs::read_to_string(floor_path(directory))
        .expect("the floor should be recorded")
        .trim()
        .parse()
        .expect("the floor should be a whole number");

    assert_eq!(
        measured.coverage.out_of_scope,
        0,
        "the provider answered nothing for {} bindings; the gaps are:\n{}",
        measured.coverage.out_of_scope,
        measured.gaps.join("\n")
    );

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
            floor_path(directory).display()
        );
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
    assert_ratchet(
        &corpus,
        &measured,
        measured.coverage.percent_known() as usize,
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
    assert_ratchet(&surface, &measured, measured.coverage.known);
}
