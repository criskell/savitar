use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "exactOptionalPropertyTypes": true,
    "useUnknownInCatchVariables": true,
    "noPropertyAccessFromIndexSignature": true,
    "noImplicitOverride": true,
    "target": "es2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "allowImportingTsExtensions": true,
    "noEmit": true
  },
  "include": ["."]
}
"#;

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

struct Bench {
    provider: PathBuf,
}

fn bench() -> Option<Bench> {
    let provider = provider_binary(&workspace_root())?;
    Some(Bench { provider })
}

fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the project directory should be creatable");
    std::fs::write(root.join("tsconfig.json"), TSCONFIG).expect("the tsconfig should be writable");
    for (path, text) in files {
        std::fs::write(root.join(path), text).expect("the file should be writable");
    }
    root.canonicalize()
        .expect("the project root should resolve")
}

fn check_project(bench: &Bench, root: &Path, names: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_savitar"));
    command.arg("check").arg("--ir");
    for name in names {
        command.arg(root.join(name));
    }
    command
        .env("SAVITAR_TYPE_PROVIDER", &bench.provider)
        .current_dir(root)
        .output()
        .expect("the command line should run")
}

fn the_type_checker_accepts(bench: &Bench, root: &Path) -> bool {
    Command::new(&bench.provider)
        .arg("--noEmit")
        .arg("-p")
        .arg(root.join("tsconfig.json"))
        .current_dir(root)
        .output()
        .expect("the type checker should run")
        .status
        .success()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn a_published_pair() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "settings.ts",
            "export const port = 8080;\nexport const host = \"0.0.0.0\";\n",
        ),
        (
            "server.ts",
            "import { host, port } from \"./settings.ts\";\n\nexport const endpoint = `${host}:${port}`;\n",
        ),
    ]
}

#[test]
fn an_initializer_reading_a_module_the_order_runs_first_is_announced_as_sound() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let root = project("order-published", &a_published_pair());
    assert!(the_type_checker_accepts(&bench, &root));

    let output = check_project(&bench, &root, &["settings.ts", "server.ts"]);
    let printed = stdout_of(&output);

    assert!(
        printed.contains("2 file(s) sound"),
        "both modules must be announced as sound; the output was:\n{printed}"
    );
    assert!(
        printed.matches("one module initializer").count() == 2,
        "both modules must keep their initializer; the output was:\n{printed}"
    );
    assert!(output.status.success());
}

#[test]
fn the_order_the_arguments_are_given_in_does_not_change_the_verdict() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let root = project("order-arguments", &a_published_pair());

    let dependency_first = check_project(&bench, &root, &["settings.ts", "server.ts"]);
    let dependent_first = check_project(&bench, &root, &["server.ts", "settings.ts"]);

    assert!(
        dependency_first.status.success() && dependent_first.status.success(),
        "the argument order must not decide the verdict; the outputs were:\n{}\n{}",
        stdout_of(&dependency_first),
        stdout_of(&dependent_first)
    );
}

#[test]
fn an_initializer_reading_a_module_this_run_does_not_lower_is_refused() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let root = project("order-unchecked", &a_published_pair());

    let output = check_project(&bench, &root, &["server.ts"]);
    let printed = stdout_of(&output);

    assert!(
        printed.contains("unsupported-import-unchecked"),
        "a module Savitar did not lower publishes nothing; the output was:\n{printed}"
    );
    assert!(
        printed.contains("server.ts:3:28"),
        "the refusal must carry file, line and column; the output was:\n{printed}"
    );
    assert!(
        printed.contains("no module initializer"),
        "the refused initializer must not be lowered; the output was:\n{printed}"
    );
    assert!(!output.status.success());
}

#[test]
fn an_initializer_reading_a_module_in_a_cycle_with_it_is_refused_by_the_order() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let root = project(
        "order-cycle",
        &[
            (
                "left.ts",
                "import { right } from \"./right.ts\";\n\nexport const left = 1;\nexport const sum = right + 1;\n",
            ),
            (
                "right.ts",
                "import { left } from \"./left.ts\";\n\nexport const right = left + 1;\n",
            ),
        ],
    );
    assert!(
        the_type_checker_accepts(&bench, &root),
        "the type checker accepts a cycle, which is why Savitar has to refuse it"
    );

    let output = check_project(&bench, &root, &["left.ts", "right.ts"]);
    let printed = stdout_of(&output);

    assert!(
        printed.contains("unsupported-import-order"),
        "the lowering must refuse the read, not only the graph the cycle; the output was:\n{printed}"
    );
    assert!(
        !printed.contains("one module initializer"),
        "no module in a cycle may keep an initializer; the output was:\n{printed}"
    );
    assert!(!output.status.success());
}

#[test]
fn an_initializer_reading_a_module_that_only_reaches_a_cycle_is_refused_by_the_order() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let root = project(
        "order-reaches-cycle",
        &[
            (
                "left.ts",
                "import { right } from \"./right.ts\";\n\nexport const left: number = 1;\nexport const sum = right + left;\n",
            ),
            (
                "right.ts",
                "import { left } from \"./left.ts\";\n\nexport const right = left + 1;\n",
            ),
            (
                "outside.ts",
                "import { sum } from \"./left.ts\";\n\nexport const outside = sum + 1;\n",
            ),
        ],
    );
    assert!(the_type_checker_accepts(&bench, &root));

    let output = check_project(&bench, &root, &["outside.ts"]);
    let printed = stdout_of(&output);

    assert!(
        printed.contains("outside.ts") && printed.contains("unsupported-import-order"),
        "a module that only reaches a cycle has no place either; the output was:\n{printed}"
    );
    assert!(!output.status.success());
}

#[test]
fn an_initializer_reading_a_module_whose_own_module_code_is_refused_is_refused() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let root = project(
        "order-unpublished",
        &[
            (
                "settings.ts",
                "const defaults = { port: 8080 };\n\nfunction readPort(): number {\n  return defaults.port;\n}\n\nexport const port = readPort();\n",
            ),
            (
                "server.ts",
                "import { port } from \"./settings.ts\";\n\nexport const label = port + 1;\n",
            ),
        ],
    );
    assert!(the_type_checker_accepts(&bench, &root));

    let output = check_project(&bench, &root, &["settings.ts", "server.ts"]);
    let printed = stdout_of(&output);

    assert!(
        printed.contains("unsupported-import-unpublished"),
        "a dependency whose initializer is refused binds nothing; the output was:\n{printed}"
    );
    assert!(!output.status.success());
}

#[test]
fn a_module_reading_an_import_only_inside_a_function_is_sound_on_its_own() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let root = project(
        "order-function-read",
        &[
            ("settings.ts", "export const port = 8080;\n"),
            (
                "server.ts",
                "import { port } from \"./settings.ts\";\n\nexport function chosen(): number {\n  return port;\n}\n",
            ),
        ],
    );
    assert!(the_type_checker_accepts(&bench, &root));

    let output = check_project(&bench, &root, &["server.ts"]);
    let printed = stdout_of(&output);

    assert!(
        printed.contains("1 file(s) sound"),
        "a function body runs after every initializer, so the read needs no order; the output was:\n{printed}"
    );
    assert!(output.status.success());
}

#[test]
fn a_single_file_that_belongs_to_no_project_still_lowers_its_initializer() {
    let Some(bench) = bench() else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };
    let root = project(
        "order-alone",
        &[("alone.ts", "export const port = 8080;\n")],
    );
    assert!(the_type_checker_accepts(&bench, &root));

    let output = check_project(&bench, &root, &["alone.ts"]);
    let printed = stdout_of(&output);

    assert!(
        printed.contains("1 file(s) sound") && printed.contains("one module initializer"),
        "a file checked alone must keep answering; the output was:\n{printed}"
    );
    assert!(output.status.success());
}
