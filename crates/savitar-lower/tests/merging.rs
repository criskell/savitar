use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use savitar_diagnostics::SourceFile;

use savitar_ir::{Function, Terminator, verify};

use savitar_lower::{LowerReport, lower};

use savitar_typeinfo_tsapi::TsApi;

use savitar_types::TypeTable;

fn provider_binary() -> Option<OsString> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../node_modules/.bin/tsc-rs")
        .canonicalize()
        .ok()?;
    path.exists().then(|| path.into_os_string())
}

fn scratch_project_of(files: &[(&str, &str)]) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before the epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("savitar-fused-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("could not create the scratch project");
    std::fs::write(
        dir.join("tsconfig.json"),
        "{ \"compilerOptions\": { \"strict\": true }, \"include\": [\".\"] }",
    )
    .expect("could not write tsconfig.json");
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("could not write a fixture file");
    }
    dir
}

struct Run {
    project: PathBuf,
    report: LowerReport,
}

impl Run {
    fn only_function(&self) -> &Function {
        assert!(
            self.report.diagnostics.is_empty(),
            "nothing in this fixture should be refused: {:?}",
            self.report.diagnostics
        );
        assert_eq!(self.report.functions.len(), 1);
        let function = &self.report.functions[0];
        assert_eq!(verify(function), vec![]);
        function
    }

    fn unreachable_terminators(&self) -> usize {
        let function = self.only_function();
        let dominance = savitar_ir::Dominance::of(function);
        function
            .blocks()
            .filter(|(id, block)| {
                dominance.reachable(*id) && matches!(block.terminator, Terminator::Unreachable(_))
            })
            .count()
    }

    fn returns_without_a_value(&self) -> usize {
        self.only_function()
            .blocks()
            .filter(|(_, block)| block.terminator == Terminator::Return(None))
            .count()
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.project).ok();
    }
}

fn lower_over_the_provider(files: &[(&str, &str)], main: &str) -> Option<Run> {
    let binary = provider_binary()?;
    let project = scratch_project_of(files);
    let main_path = project.join(main);
    let api = TsApi::start(&binary, &project, &[main_path.as_path()])
        .expect("the provider should start and create a snapshot");
    let mut table = TypeTable::new();
    let text = std::fs::read_to_string(&main_path).expect("the fixture should be readable");
    let file = SourceFile::new(main_path.to_str().expect("utf-8 path").to_string(), text);
    let report = lower(&file, &api, &mut table);
    Some(Run { project, report })
}

const COVERED_OVER_TWO_DECLARATIONS: &str = concat!(
    "interface Commands {\n",
    "  ping: { kind: \"ping\" };\n",
    "}\n",
    "interface Commands {\n",
    "  drain: { kind: \"drain\"; graceMillis: number };\n",
    "}\n",
    "type Command = Commands[keyof Commands];\n",
    "function route(command: Command): string {\n",
    "  switch (command.kind) {\n",
    "    case \"ping\":\n",
    "      return \"pong\";\n",
    "    case \"drain\":\n",
    "      return \"draining\";\n",
    "  }\n",
    "}\n",
);

#[test]
fn a_switch_covering_a_union_fused_from_two_declarations_leaves_no_unmatched_path() {
    let Some(run) = lower_over_the_provider(&[("a.ts", COVERED_OVER_TWO_DECLARATIONS)], "a.ts")
    else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    assert_eq!(
        run.returns_without_a_value(),
        0,
        "both declarations have a case, so no path leaves the function without a value"
    );
    assert_eq!(
        run.unreachable_terminators(),
        1,
        "the path no case matched is marked unreachable instead of joining"
    );
}

const UNCOVERED_SECOND_DECLARATION: &str = concat!(
    "interface Commands {\n",
    "  ping: { kind: \"ping\" };\n",
    "}\n",
    "interface Commands {\n",
    "  drain: { kind: \"drain\"; graceMillis: number };\n",
    "}\n",
    "type Command = Commands[keyof Commands];\n",
    "function route(command: Command): string {\n",
    "  switch (command.kind) {\n",
    "    case \"ping\":\n",
    "      return \"pong\";\n",
    "  }\n",
    "  return \"unknown\";\n",
    "}\n",
);

#[test]
fn a_member_only_the_second_declaration_names_keeps_the_unmatched_path_alive() {
    let Some(run) = lower_over_the_provider(&[("a.ts", UNCOVERED_SECOND_DECLARATION)], "a.ts")
    else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    assert_eq!(
        run.unreachable_terminators(),
        0,
        "`drain` has no case, so calling the unmatched path unreachable would be a lie"
    );
}

const REPEATED_INTERFACE_CARRIES_NO_CODE: &str = concat!(
    "interface Config {\n",
    "  host: string;\n",
    "}\n",
    "interface Config {\n",
    "  port: number;\n",
    "}\n",
    "function origin(config: Config): string {\n",
    "  return `${config.host}:${config.port}`;\n",
    "}\n",
);

#[test]
fn an_interface_declared_twice_refuses_nothing_and_types_both_members() {
    let Some(run) =
        lower_over_the_provider(&[("a.ts", REPEATED_INTERFACE_CARRIES_NO_CODE)], "a.ts")
    else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    assert_eq!(
        run.only_function().name(),
        "origin",
        "reading a member from each declaration must not be refused"
    );
}

const FUSED_FROM_ANOTHER_FILE: &str = concat!(
    "interface Commands {\n",
    "  ping: { kind: \"ping\" };\n",
    "}\n",
    "type Command = Commands[keyof Commands];\n",
    "function route(command: Command): string {\n",
    "  switch (command.kind) {\n",
    "    case \"ping\":\n",
    "      return \"pong\";\n",
    "  }\n",
    "  return \"unknown\";\n",
    "}\n",
);

#[test]
fn a_member_declared_in_another_file_keeps_the_unmatched_path_alive() {
    let Some(run) = lower_over_the_provider(
        &[
            ("a.ts", FUSED_FROM_ANOTHER_FILE),
            (
                "b.ts",
                "interface Commands {\n  drain: { kind: \"drain\"; graceMillis: number };\n}\n",
            ),
        ],
        "a.ts",
    ) else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    assert_eq!(
        run.unreachable_terminators(),
        0,
        "the member fused in from `b.ts` has no case, and no module graph of ours was needed to see it"
    );
}

const FUSED_FROM_ANOTHER_FILE_AND_COVERED: &str = concat!(
    "interface Commands {\n",
    "  ping: { kind: \"ping\" };\n",
    "}\n",
    "type Command = Commands[keyof Commands];\n",
    "function route(command: Command): string {\n",
    "  switch (command.kind) {\n",
    "    case \"ping\":\n",
    "      return \"pong\";\n",
    "    case \"drain\":\n",
    "      return \"draining\";\n",
    "  }\n",
    "}\n",
);

#[test]
fn a_switch_covering_a_union_fused_across_files_leaves_no_unmatched_path() {
    let Some(run) = lower_over_the_provider(
        &[
            ("a.ts", FUSED_FROM_ANOTHER_FILE_AND_COVERED),
            (
                "b.ts",
                "interface Commands {\n  drain: { kind: \"drain\"; graceMillis: number };\n}\n",
            ),
        ],
        "a.ts",
    ) else {
        eprintln!("skipped: node_modules/.bin/tsc-rs is not installed");
        return;
    };

    assert_eq!(
        run.returns_without_a_value(),
        0,
        "the case fused in from `b.ts` counts, so no path leaves the function without a value"
    );
    assert_eq!(run.unreachable_terminators(), 1);
}
