use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root should resolve")
}

fn provider_binary(root: &Path) -> Option<PathBuf> {
    let path = root.join("node_modules/.bin/tsc-rs");
    path.exists().then_some(path)
}

fn check(root: &Path, provider: &Path, flags: &[&str], fixture: &str) -> Output {
    check_in(root, provider, flags, "test-files/cli", fixture)
}

fn check_in(
    root: &Path,
    provider: &Path,
    flags: &[&str],
    directory: &str,
    fixture: &str,
) -> Output {
    let file = root.join(directory).join(fixture);
    let mut command = Command::new(env!("CARGO_BIN_EXE_savitar"));
    command.arg("check");
    for flag in flags {
        command.arg(flag);
    }
    command
        .arg(&file)
        .env("SAVITAR_TYPE_PROVIDER", provider)
        .current_dir(root)
        .output()
        .expect("the command line should run")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

struct Bench {
    root: PathBuf,
    provider: PathBuf,
}

fn bench() -> Option<Bench> {
    let root = workspace_root();
    let provider = provider_binary(&root)?;
    Some(Bench { root, provider })
}

#[test]
fn a_file_whose_function_the_lowering_refuses_is_not_announced_as_sound() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let output = check(&bench.root, &bench.provider, &[], "refused.ts");
    let printed = stdout_of(&output);
    assert!(
        !printed.contains("file(s) sound"),
        "a refused function must not be announced as sound; the output was:\n{printed}"
    );
    assert!(
        printed.contains("not sound"),
        "the verdict must name the findings; the output was:\n{printed}"
    );
}

#[test]
fn a_file_whose_function_the_lowering_refuses_prints_the_refusal_without_the_ir_flag() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let output = check(&bench.root, &bench.provider, &[], "refused.ts");
    let printed = stdout_of(&output);
    assert!(
        printed.contains("unsupported-function"),
        "the refusal must be printed with no flag asking for it; the output was:\n{printed}"
    );
    assert!(
        printed.contains("refused.ts:5:1"),
        "the refusal must carry file, line and column; the output was:\n{printed}"
    );
}

#[test]
fn a_file_whose_function_the_lowering_refuses_fails_the_run() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let output = check(&bench.root, &bench.provider, &[], "refused.ts");
    assert!(
        !output.status.success(),
        "a refusal rejects, so the run must fail; the output was:\n{}",
        stdout_of(&output)
    );
}

#[test]
fn a_file_the_lowering_gets_through_is_announced_as_sound() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let output = check(&bench.root, &bench.provider, &[], "lowers.ts");
    let printed = stdout_of(&output);
    assert!(
        printed.contains("1 file(s) sound"),
        "a file that gets through must be announced as sound; the output was:\n{printed}"
    );
    assert!(
        output.status.success(),
        "a sound file must not fail the run"
    );
}

#[test]
fn the_coverage_line_is_printed_only_when_types_is_asked_for() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let bare = stdout_of(&check(&bench.root, &bench.provider, &[], "lowers.ts"));
    let asked = stdout_of(&check(
        &bench.root,
        &bench.provider,
        &["--types"],
        "lowers.ts",
    ));
    assert!(
        !bare.contains("bindings typed"),
        "the bare run must not print the coverage line; the output was:\n{bare}"
    );
    assert!(
        asked.contains("bindings typed"),
        "`--types` must print the coverage line; the output was:\n{asked}"
    );
}

#[test]
fn the_lowering_line_is_printed_only_when_ir_is_asked_for() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let bare = stdout_of(&check(&bench.root, &bench.provider, &[], "lowers.ts"));
    let asked = stdout_of(&check(&bench.root, &bench.provider, &["--ir"], "lowers.ts"));
    assert!(
        !bare.contains("function(s) lowered"),
        "the bare run must not print the lowering line; the output was:\n{bare}"
    );
    assert!(
        asked.contains("function(s) lowered"),
        "`--ir` must print the lowering line; the output was:\n{asked}"
    );
    assert!(
        asked.contains("bindings typed"),
        "`--ir` must keep printing the coverage line; the output was:\n{asked}"
    );
}

#[test]
fn the_verdict_is_the_same_with_and_without_the_ir_flag() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let bare = check(&bench.root, &bench.provider, &[], "refused.ts");
    let asked = check(&bench.root, &bench.provider, &["--ir"], "refused.ts");
    assert_eq!(
        bare.status.success(),
        asked.status.success(),
        "the verdict must not depend on a reporting flag"
    );
    assert!(
        stdout_of(&asked).contains("not sound"),
        "`--ir` must reach the same verdict; the output was:\n{}",
        stdout_of(&asked)
    );
}

#[test]
fn the_evaluation_order_is_printed_only_when_ir_is_asked_for() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let bare = stdout_of(&check_in(
        &bench.root,
        &bench.provider,
        &[],
        "test-files/graph",
        "uses.ts",
    ));
    let asked = stdout_of(&check_in(
        &bench.root,
        &bench.provider,
        &["--ir"],
        "test-files/graph",
        "uses.ts",
    ));
    assert!(
        !bare.contains("evaluation order"),
        "the bare run must not print the evaluation order; the output was:\n{bare}"
    );
    assert!(
        asked.contains("2 of 2 module(s) have a place in the evaluation order"),
        "`--ir` must print the evaluation order of the whole graph; the output was:\n{asked}"
    );
}

#[test]
fn a_file_that_reads_an_import_the_graph_resolves_is_announced_as_sound() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let output = check_in(
        &bench.root,
        &bench.provider,
        &["--ir"],
        "test-files/graph",
        "uses.ts",
    );
    let printed = stdout_of(&output);
    assert!(
        printed.contains("1 file(s) sound"),
        "the file must be announced as sound; the output was:\n{printed}"
    );
    assert!(
        output.status.success(),
        "a sound file must not fail the run; the output was:\n{printed}"
    );
}

#[test]
fn an_import_the_graph_cannot_resolve_is_printed_and_fails_the_run() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let output = check_in(
        &bench.root,
        &bench.provider,
        &[],
        "test-files/graph-gone",
        "gone.ts",
    );
    let printed = stdout_of(&output);
    assert!(
        printed.contains("unresolved-import"),
        "the unresolved import must be printed; the output was:\n{printed}"
    );
    assert!(
        printed.contains("gone.ts:1:25"),
        "the unresolved import must carry file, line and column; the output was:\n{printed}"
    );
    assert!(
        !output.status.success(),
        "an unresolved import rejects, so the run must fail; the output was:\n{printed}"
    );
}

#[test]
fn a_module_initializer_is_reported_only_when_ir_is_asked_for() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let shared = stdout_of(&check_in(
        &bench.root,
        &bench.provider,
        &["--ir"],
        "test-files/graph",
        "shared.ts",
    ));
    let plain = stdout_of(&check(&bench.root, &bench.provider, &["--ir"], "lowers.ts"));
    assert!(
        shared.contains("one module initializer"),
        "a module with top-level code has an initializer; the output was:\n{shared}"
    );
    assert!(
        plain.contains("no module initializer"),
        "a module with no top-level code has none; the output was:\n{plain}"
    );
}
