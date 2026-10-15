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

fn recorded_floor(corpus: &Path) -> u32 {
    std::fs::read_to_string(floor_path(corpus))
        .expect("the coverage floor should be recorded")
        .trim()
        .parse()
        .expect("the coverage floor should be a whole percentage")
}

#[test]
fn type_coverage_of_the_corpus_never_falls() {
    let root = workspace_root();
    let corpus = root.join("test-files/corpus");
    let Some(binary) = provider_binary(&root) else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    let files = corpus_files(&corpus);
    let open: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    let api = TsApi::start(&binary, &corpus, &open).expect("the provider should start");
    let mut table = TypeTable::new();
    let mut total = Coverage::default();
    let mut gaps: Vec<String> = Vec::new();

    for path in &files {
        let text = std::fs::read_to_string(path).expect("the corpus file should be readable");
        let name = path.to_str().expect("utf-8 path").to_string();
        let file = SourceFile::new(name, text);
        let report = check_types(&file, &api, &mut table);
        total.known += report.coverage.known;
        total.unsupported += report.coverage.unsupported;
        total.out_of_scope += report.coverage.out_of_scope;
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

    let actual = total.percent_known();
    let floor = recorded_floor(&corpus);

    assert_eq!(
        total.out_of_scope,
        0,
        "the provider answered nothing for {} bindings; the first gaps are:\n{}",
        total.out_of_scope,
        gaps.join("\n")
    );

    assert!(
        actual >= floor,
        "type coverage fell from {floor}% to {actual}% ({} of {} bindings); the gaps are:\n{}",
        total.known,
        total.total(),
        gaps.join("\n")
    );

    if actual > floor {
        panic!(
            "type coverage rose from {floor}% to {actual}% ({} of {} bindings).\n\
             Raise the ratchet: echo {actual} > {}",
            total.known,
            total.total(),
            floor_path(&corpus).display()
        );
    }
}
