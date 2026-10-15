use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root should resolve")
}

fn typecheck(root: &Path, directory: &str) {
    let binary = root.join("node_modules/.bin/tsc-rs");
    if !binary.exists() {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    }
    let project = root.join("test-files").join(directory);

    let run = Command::new(&binary)
        .arg("--noEmit")
        .arg("-p")
        .arg(".")
        .current_dir(&project)
        .output()
        .expect("the checker should run");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success() && stdout.trim().is_empty(),
        "`test-files/{directory}` must be valid TypeScript, or every measurement over it is \
         meaningless.\nstatus: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        run.status.code()
    );
}

#[test]
fn the_corpus_is_valid_typescript() {
    typecheck(&workspace_root(), "corpus");
}

#[test]
fn the_library_surface_is_valid_typescript() {
    typecheck(&workspace_root(), "lib");
}
