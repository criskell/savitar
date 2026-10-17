use std::process::ExitCode;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use savitar_diagnostics::{Diagnostic, SourceFile, Span, render};
use savitar_lower::{LowerReport, ModuleStanding, Standing, TwoModulesForOneSpecifier};
use savitar_modules::{Carries, ModuleGraph, ModuleId, Order, Target};
use savitar_typeinfo::TypeProvider;
use savitar_typeinfo_tsapi::TsApi;
use savitar_types::TypeTable;

fn usage() -> ExitCode {
    eprintln!("usage: savitar check [--types] [--ir] <file.ts>...");
    eprintln!("       savitar rules");
    eprintln!("       savitar config");
    eprintln!("       savitar divergences");
    ExitCode::from(2)
}

fn list_divergences() -> ExitCode {
    for divergence in savitar_types::INTENTIONAL_DIVERGENCES {
        println!("{}", divergence.id);
        println!("  tsc      {}", divergence.what_tsc_answers);
        println!("  savitar  {}", divergence.what_savitar_answers);
        println!("  because  {}", divergence.why_savitar_refuses_to_follow);
    }
    ExitCode::SUCCESS
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

fn list_config() -> ExitCode {
    let width = savitar_sound::CONFIG_REQUIREMENTS
        .iter()
        .map(|requirement| requirement.option.len())
        .max()
        .unwrap_or(0);
    for requirement in savitar_sound::CONFIG_REQUIREMENTS {
        let demand = match requirement.demand {
            savitar_sound::Demand::MustBeTrue => "true ",
            savitar_sound::Demand::MustNotBeTrue => "false",
        };
        println!(
            "{:<width$}  {}  {}",
            requirement.option,
            demand,
            requirement.reason,
            width = width
        );
    }
    ExitCode::SUCCESS
}

fn provider_binary() -> OsString {
    if let Some(from_env) = std::env::var_os("SAVITAR_TYPE_PROVIDER") {
        return from_env;
    }
    OsString::from("./node_modules/.bin/tsc-rs")
}

fn project_root(paths: &[PathBuf]) -> PathBuf {
    paths
        .first()
        .and_then(|path| path.parent())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn standing_of(
    graph: &ModuleGraph,
    order: &Order,
    id: ModuleId,
    published: &BTreeMap<ModuleId, BTreeSet<String>>,
) -> (ModuleStanding, Vec<Diagnostic>) {
    let mut standing = ModuleStanding::alone();
    let mut clashes = Vec::new();
    let place = order.position_of(id);
    for import in &graph.module(id).imports {
        if import.carries != Carries::Values {
            continue;
        }
        let Target::Module(other) = &import.target else {
            continue;
        };
        let other = *other;
        let ran = match (place, order.position_of(other)) {
            (Some(place), Some(before)) if before < place => match published.get(&other) {
                None => Standing::NotChecked,
                Some(names) => Standing::RanBeforeBinding(names.clone()),
            },
            _ => Standing::DidNotRunBefore,
        };
        let resolved = graph.module(other).path.display().to_string();
        if let Err(clash) = standing.note(&import.specifier, &resolved, ran) {
            clashes.push(two_modules_for_one_specifier(import.span, &clash));
        }
    }
    (standing, clashes)
}

fn two_modules_for_one_specifier(at: Span, clash: &TwoModulesForOneSpecifier) -> Diagnostic {
    let specifier = &clash.specifier;
    let first = &clash.first;
    let second = &clash.second;
    Diagnostic::error(
        "two-modules-for-one-specifier",
        at,
        format!(
            "`{specifier}` resolves to `{first}` and to `{second}` in this module, so the \
             evidence of evaluation order cannot say which one ran"
        ),
    )
    .with_help("the order evidence is looked up by the text the module writes, so that text has to name one module")
}

fn lower_one(
    path: &str,
    provider: &dyn TypeProvider,
    table: &mut TypeTable,
    standing: &ModuleStanding,
) -> Option<LowerReport> {
    let text = std::fs::read_to_string(path).ok()?;
    let file = SourceFile::new(path.to_string(), text);
    Some(savitar_lower::lower_in_order(
        &file, provider, table, standing,
    ))
}

struct LoweredGraph {
    asked_about: Vec<Option<LowerReport>>,
    modules_lowered: usize,
}

fn every_module_in_evaluation_order(graph: &ModuleGraph, order: &Order) -> Vec<ModuleId> {
    let mut walked = order.evaluated.clone();
    walked.extend(
        graph
            .modules()
            .map(|(id, _)| id)
            .filter(|&id| !order.holds(id)),
    );
    walked
}

fn lower_the_whole_graph(
    paths: &[String],
    absolute: &[PathBuf],
    graph: &ModuleGraph,
    order: &Order,
    provider: &dyn TypeProvider,
    table: &mut TypeTable,
) -> LoweredGraph {
    let of_index: Vec<Option<ModuleId>> = absolute.iter().map(|path| graph.id_of(path)).collect();
    let mut published: BTreeMap<ModuleId, BTreeSet<String>> = BTreeMap::new();
    let mut held: BTreeMap<ModuleId, LowerReport> = BTreeMap::new();

    for id in every_module_in_evaluation_order(graph, order) {
        let (standing, clashes) = standing_of(graph, order, id, &published);
        let Some(path) = graph.module(id).path.to_str() else {
            continue;
        };
        let Some(mut report) = lower_one(path, provider, table, &standing) else {
            continue;
        };
        report.diagnostics.extend(clashes);
        report
            .diagnostics
            .sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.rule));
        published.insert(id, report.published.clone());
        held.insert(id, report);
    }

    let modules_lowered = held.len();
    let asked_about = paths
        .iter()
        .enumerate()
        .map(|(index, path)| match of_index[index] {
            Some(id) => held.remove(&id),
            None => lower_one(path, provider, table, &ModuleStanding::alone()),
        })
        .collect();

    LoweredGraph {
        asked_about,
        modules_lowered,
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return usage();
    };
    if command == "rules" {
        return list_rules();
    }
    if command == "config" {
        return list_config();
    }
    if command == "divergences" {
        return list_divergences();
    }
    if command != "check" {
        eprintln!("savitar: unknown command `{command}`");
        return usage();
    }

    let mut report_coverage = false;
    let mut report_lowering = false;
    let mut paths: Vec<String> = Vec::new();
    for argument in args {
        if argument == "--types" {
            report_coverage = true;
        } else if argument == "--ir" {
            report_coverage = true;
            report_lowering = true;
        } else {
            paths.push(argument);
        }
    }
    if paths.is_empty() {
        return usage();
    }

    let mut rejected = false;
    let mut findings = 0usize;

    let absolute: Vec<PathBuf> = paths
        .iter()
        .map(|path| std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path)))
        .collect();
    let root = project_root(&absolute);
    let open: Vec<&Path> = absolute.iter().map(PathBuf::as_path).collect();
    let api = match TsApi::start(&provider_binary(), &root, &open) {
        Ok(api) => api,
        Err(error) => {
            eprintln!("savitar: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut table = TypeTable::new();

    let graph = ModuleGraph::build(&root, &absolute);
    let order = graph.evaluation_order();
    for finding in graph.findings().iter().chain(order.findings.iter()) {
        let module = graph.module(finding.module);
        let Ok(text) = std::fs::read_to_string(&module.path) else {
            continue;
        };
        let named = SourceFile::new(module.path.display().to_string(), text);
        print!(
            "{}",
            render(&named, std::slice::from_ref(&finding.diagnostic))
        );
        findings += 1;
        if finding.diagnostic.severity.rejects() {
            rejected = true;
        }
    }
    if report_lowering {
        println!(
            "savitar: {} of {} module(s) have a place in the evaluation order",
            order.evaluated.len(),
            graph.module_count()
        );
    }

    let walked = lower_the_whole_graph(&paths, &absolute, &graph, &order, &api, &mut table);
    let reports = walked.asked_about;
    if report_lowering {
        println!(
            "savitar: {} module(s) of the graph lowered to check the {} file(s) asked about",
            walked.modules_lowered,
            paths.len()
        );
    }

    for (index, path) in paths.iter().enumerate() {
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

        let types = savitar_sound::check_types(&file, &api, &mut table);
        findings += types.diagnostics.len();
        if types.diagnostics.iter().any(|d| d.severity.rejects()) {
            rejected = true;
        }
        print!("{}", render(&file, &types.diagnostics));
        if report_coverage {
            println!(
                "savitar: {} of {} bindings typed ({}%)",
                types.coverage.known,
                types.coverage.total(),
                types.coverage.percent_known()
            );
        }

        let Some(lowered) = reports[index].as_ref() else {
            continue;
        };
        findings += lowered.diagnostics.len();
        if lowered.diagnostics.iter().any(|d| d.severity.rejects()) {
            rejected = true;
        }
        print!("{}", render(&file, &lowered.diagnostics));
        if report_lowering {
            println!(
                "savitar: {} function(s) lowered, {} refused, {} module initializer",
                lowered.functions.len(),
                lowered.diagnostics.len(),
                if lowered.initializer.is_some() {
                    "one"
                } else {
                    "no"
                }
            );
        }
    }

    if findings == 0 {
        println!("savitar: {} file(s) sound", paths.len());
    } else {
        println!(
            "savitar: {findings} finding(s) in {} file(s); not sound",
            paths.len()
        );
    }

    if rejected {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
