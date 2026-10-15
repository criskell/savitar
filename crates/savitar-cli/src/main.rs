use std::process::ExitCode;

use savitar_diagnostics::{SourceFile, render};

fn usage() -> ExitCode {
    eprintln!("usage: savitar check <file.ts>...");
    eprintln!("       savitar rules");
    ExitCode::from(2)
}

fn list_rules() -> ExitCode {
    let width = savitar_sound::RULES
        .iter()
        .map(|rule| rule.id.len())
        .max()
        .unwrap_or(0);
    for rule in savitar_sound::RULES {
        println!("{:<width$}  {}", rule.id, rule.reason, width = width);
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return usage();
    };
    if command == "rules" {
        return list_rules();
    }
    if command != "check" {
        eprintln!("savitar: unknown command `{command}`");
        return usage();
    }

    let paths: Vec<String> = args.collect();
    if paths.is_empty() {
        return usage();
    }

    let mut rejected = false;
    let mut findings = 0usize;

    for path in &paths {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("savitar: cannot read `{path}`: {error}");
                rejected = true;
                continue;
            }
        };

        let file = SourceFile::new(path.clone(), text);
        let report = savitar_sound::check(&file);
        findings += report.diagnostics.len();
        if report.rejected() {
            rejected = true;
        }
        print!("{}", render(&file, &report.diagnostics));
    }

    if findings == 0 {
        println!("savitar: {} file(s) sound", paths.len());
    }

    if rejected {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
