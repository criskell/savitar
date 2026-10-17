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

const STRICT_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "exactOptionalPropertyTypes": true,
    "useUnknownInCatchVariables": true,
    "noPropertyAccessFromIndexSignature": true,
    "noImplicitOverride": true,
    "target": "es2022",
    "module": "nodenext",
    "moduleResolution": "nodenext",
    "noEmit": true
  },
  "include": ["."]
}
"#;

fn laid_out(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the project directory should be creatable");
    std::fs::write(root.join("tsconfig.json"), STRICT_TSCONFIG)
        .expect("the tsconfig should be writable");
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

fn check_laid_out(project: &Path, provider: &Path, entry: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_savitar"))
        .arg("check")
        .arg(project.join(entry))
        .env("SAVITAR_TYPE_PROVIDER", provider)
        .current_dir(project)
        .output()
        .expect("the command line should run")
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
    let plain = stdout_of(&check(
        &bench.root,
        &bench.provider,
        &["--ir"],
        "declares.ts",
    ));
    assert!(
        shared.contains("one module initializer"),
        "a module with top-level code has an initializer; the output was:\n{shared}"
    );
    assert!(
        plain.contains("no module initializer"),
        "a module that binds no name has none; the output was:\n{plain}"
    );
}

#[test]
fn a_module_whose_only_top_level_code_is_a_declaration_still_has_an_initializer() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let printed = stdout_of(&check(&bench.root, &bench.provider, &["--ir"], "lowers.ts"));
    assert!(
        printed.contains("one module initializer"),
        "a function declaration binds a name, so something has to bind it; the output was:\n{printed}"
    );
}

#[test]
fn a_handler_that_imports_a_real_installed_package_is_not_announced_as_sound() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let project = laid_out(
        "declared-boundary-package",
        &[
            (
                "package.json",
                "{ \"name\": \"service\", \"version\": \"1.0.0\", \"private\": true, \"type\": \"module\", \"dependencies\": { \"shipping\": \"2.0.0\" } }\n",
            ),
            (
                "node_modules/shipping/package.json",
                "{ \"name\": \"shipping\", \"version\": \"2.0.0\", \"types\": \"index.d.ts\", \"main\": \"index.js\" }\n",
            ),
            (
                "node_modules/shipping/index.d.ts",
                "export declare function quote(weight: number): number;\n",
            ),
            (
                "node_modules/shipping/index.js",
                "export function quote(weight) {\n  return weight;\n}\n",
            ),
            (
                "handler.ts",
                "import { quote } from \"shipping\";\n\nexport function priceOf(weight: number): number {\n  return quote(weight);\n}\n",
            ),
        ],
    );

    let output = check_laid_out(&project, &bench.provider, "handler.ts");
    let printed = stdout_of(&output);

    assert!(
        printed.contains("unsupported-declared-boundary"),
        "the declared boundary must be refused by name; the output was:\n{printed}"
    );
    assert!(
        printed.contains("handler.ts:1:23"),
        "the refusal must carry file, line and column; the output was:\n{printed}"
    );
    assert!(
        printed.contains("not sound"),
        "nothing behind the boundary is checked, so the verdict cannot be sound; the output was:\n{printed}"
    );
    assert!(
        !output.status.success(),
        "a declared boundary rejects, so the run must fail; the output was:\n{printed}"
    );
}

#[test]
fn a_handler_that_imports_a_hand_written_declaration_file_is_not_announced_as_sound() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let project = laid_out(
        "declared-boundary-shim",
        &[
            (
                "package.json",
                "{ \"name\": \"service\", \"version\": \"1.0.0\", \"private\": true, \"type\": \"module\" }\n",
            ),
            ("env.d.ts", "export declare function readPort(): number;\n"),
            (
                "handler.ts",
                "import { readPort } from \"./env.js\";\n\nexport function portOf(): number {\n  return readPort();\n}\n",
            ),
        ],
    );

    let output = check_laid_out(&project, &bench.provider, "handler.ts");
    let printed = stdout_of(&output);

    assert!(
        printed.contains("unsupported-declared-boundary"),
        "a relative import that lands on a declaration file is the same boundary; the output was:\n{printed}"
    );
    assert!(
        printed.contains("env.d.ts"),
        "the refusal must name the declaration file it resolved to; the output was:\n{printed}"
    );
    assert!(
        printed.contains("not sound"),
        "the verdict cannot be sound; the output was:\n{printed}"
    );
    assert!(
        !output.status.success(),
        "a declared boundary rejects, so the run must fail; the output was:\n{printed}"
    );
}

#[test]
fn a_declaration_file_given_to_the_command_is_not_announced_as_sound() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let project = laid_out(
        "declared-boundary-alone",
        &[(
            "surface.d.ts",
            "export declare function go(n: number): number;\nexport declare class Holder {\n  size: number;\n}\n",
        )],
    );

    let output = check_laid_out(&project, &bench.provider, "surface.d.ts");
    let printed = stdout_of(&output);

    assert!(
        printed.contains("unsupported-ambient-declaration"),
        "a file that is nothing but declarations proves nothing; the output was:\n{printed}"
    );
    assert!(
        printed.contains("surface.d.ts:1:1"),
        "the refusal must carry file, line and column; the output was:\n{printed}"
    );
    assert!(
        printed.contains("not sound"),
        "a file where nothing was proved cannot be announced as sound; the output was:\n{printed}"
    );
    assert!(
        !output.status.success(),
        "the run must fail; the output was:\n{printed}"
    );
}
