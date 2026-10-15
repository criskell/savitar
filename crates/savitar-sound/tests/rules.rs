use savitar_diagnostics::{Severity, SourceFile};
use savitar_sound::check;

fn rules_fired(source: &str) -> Vec<&'static str> {
    let file = SourceFile::new("fixture.ts", source);
    let report = check(&file);
    assert_eq!(
        report.count(Severity::Unsupported),
        0,
        "fixture hit an unsupported construct: {:?}",
        report.diagnostics
    );
    let mut ids: Vec<&'static str> = report.diagnostics.iter().map(|d| d.rule).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[test]
fn sound_program_is_accepted() {
    let source = r"
        export interface Point {
            x: number;
            y: number;
        }

        export function shift(point: Point, by: number): Point {
            return { x: point.x + by, y: point.y + by };
        }

        export function first(points: readonly Point[]): Point | undefined {
            return points.length > 0 ? points[0] : undefined;
        }
    ";
    let file = SourceFile::new("fixture.ts", source);
    let report = check(&file);
    assert!(
        !report.rejected(),
        "sound program was rejected: {}",
        savitar_diagnostics::render(&file, &report.diagnostics)
    );
    assert!(report.diagnostics.is_empty());
}

#[test]
fn any_is_rejected_in_annotation_and_in_assertion() {
    assert_eq!(rules_fired("let x: any = 1;"), vec!["no-any"]);
    let both = rules_fired("let x = y as any;");
    assert!(both.contains(&"no-any"));
    assert!(both.contains(&"no-type-assertion"));
}

#[test]
fn as_const_is_not_an_assertion() {
    assert!(rules_fired("const mode = 'fast' as const;").is_empty());
}

#[test]
fn widening_assertion_is_rejected() {
    assert_eq!(
        rules_fired("const point = input as Point;"),
        vec!["no-type-assertion"]
    );
    assert_eq!(
        rules_fired("const point = <Point>input;"),
        vec!["no-type-assertion"]
    );
}

#[test]
fn non_null_assertion_is_rejected() {
    assert_eq!(
        rules_fired("const name = user!.name;"),
        vec!["no-non-null-assertion"]
    );
}

#[test]
fn delete_is_rejected_but_other_unary_operators_are_not() {
    assert_eq!(rules_fired("delete point.x;"), vec!["no-delete"]);
    assert!(rules_fired("const n = -x; const b = !flag; const t = typeof x;").is_empty());
}

#[test]
fn eval_is_rejected() {
    assert_eq!(rules_fired("eval('1 + 1');"), vec!["no-eval"]);
}

#[test]
fn computed_import_specifier_is_rejected() {
    assert_eq!(
        rules_fired("const mod = await import(name);"),
        vec!["closed-world-import"]
    );
    assert!(rules_fired("const mod = await import('./real.ts');").is_empty());
}

#[test]
fn prototype_mutation_is_rejected() {
    for source in [
        "Object.setPrototypeOf(point, base);",
        "Reflect.setPrototypeOf(point, base);",
        "Object.defineProperty(point, 'x', { value: 1 });",
        "const proto = point.__proto__;",
    ] {
        assert_eq!(
            rules_fired(source),
            vec!["no-prototype-mutation"],
            "expected a prototype finding for: {source}"
        );
    }
}

#[test]
fn ordinary_object_calls_are_not_prototype_mutation() {
    assert!(rules_fired("const keys = Object.keys(point);").is_empty());
}

#[test]
fn with_statement_is_rejected() {
    let file = SourceFile::new("fixture.ts", "with (point) { return x; }");
    let report = check(&file);
    assert!(report.rejected());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.rule == "no-with" || d.rule == "parse")
    );
}

#[test]
fn every_rule_carries_a_help_line() {
    let source = "let a: any = b as C; delete a.x; eval('1'); Object.setPrototypeOf(a, b); a!.y;";
    let file = SourceFile::new("fixture.ts", source);
    let report = check(&file);
    assert!(!report.diagnostics.is_empty());
    for diagnostic in &report.diagnostics {
        assert!(
            diagnostic.help.is_some(),
            "rule `{}` has no help line",
            diagnostic.rule
        );
    }
}

#[test]
fn findings_are_sorted_by_position() {
    let source = "let a: any = 1;\ndelete a.x;\neval('1');\n";
    let file = SourceFile::new("fixture.ts", source);
    let report = check(&file);
    let starts: Vec<u32> = report.diagnostics.iter().map(|d| d.span.start).collect();
    let mut sorted = starts.clone();
    sorted.sort_unstable();
    assert_eq!(starts, sorted);
}

#[test]
fn satisfies_is_sound_and_accepted() {
    assert!(rules_fired("const config = { port: 8080 } satisfies Config;").is_empty());
}

#[test]
fn accessors_are_rejected_in_classes_and_object_literals() {
    assert_eq!(
        rules_fired("class C { get x(): number { return 1; } }"),
        vec!["no-accessor"]
    );
    assert_eq!(
        rules_fired("class C { set x(v: number) {} }"),
        vec!["no-accessor"]
    );
    assert_eq!(
        rules_fired("const o = { get x() { return 1; } };"),
        vec!["no-accessor"]
    );
    assert!(rules_fired("class C { x = 1; m(): number { return this.x; } }").is_empty());
    assert!(rules_fired("const o = { x: 1, m(): number { return 1; } };").is_empty());
}

#[test]
fn proxy_and_function_constructor_are_rejected() {
    assert_eq!(
        rules_fired("const p = new Proxy(target, handler);"),
        vec!["no-proxy"]
    );
    assert_eq!(
        rules_fired("const f = new Function('return 1');"),
        vec!["no-function-constructor"]
    );
    assert!(rules_fired("const m = new Map<string, number>();").is_empty());
}

#[test]
fn namespace_is_rejected() {
    assert_eq!(
        rules_fired("namespace Outer { export const x = 1; }"),
        vec!["no-namespace"]
    );
}

#[test]
fn arguments_is_rejected_and_rest_parameters_are_not() {
    assert_eq!(
        rules_fired("function f() { return arguments.length; }"),
        vec!["no-arguments"]
    );
    assert!(rules_fired("function f(...rest: number[]) { return rest.length; }").is_empty());
}

#[test]
fn every_rule_has_a_reason_and_a_unique_id() {
    let mut ids: Vec<&str> = savitar_sound::RULES.iter().map(|rule| rule.id).collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "duplicate rule id in RULES");
    for rule in savitar_sound::RULES {
        assert!(!rule.reason.is_empty(), "rule `{}` has no reason", rule.id);
    }
}
