use savitar_diagnostics::{Severity, SourceFile};
use savitar_sound::check;

const SOUND: &str = r#"{
  "compilerOptions": {
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "exactOptionalPropertyTypes": true,
    "noPropertyAccessFromIndexSignature": true,
    "noImplicitOverride": true
  }
}"#;

fn findings(source: &str) -> Vec<(&'static str, Severity)> {
    let file = SourceFile::new("tsconfig.json", source);
    check(&file)
        .diagnostics
        .into_iter()
        .map(|diagnostic| (diagnostic.rule, diagnostic.severity))
        .collect()
}

#[test]
fn a_sound_config_is_accepted() {
    assert_eq!(findings(SOUND), vec![]);
}

#[test]
fn comments_and_trailing_commas_are_tolerated() {
    let source = r#"{
  "compilerOptions": {
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "exactOptionalPropertyTypes": true,
    "noPropertyAccessFromIndexSignature": true,
    "noImplicitOverride": true,
  }
}"#;
    assert_eq!(findings(source), vec![]);
}

#[test]
fn use_unknown_in_catch_is_implied_by_strict_but_cannot_be_disabled() {
    assert_eq!(findings(SOUND), vec![]);
    let source = SOUND.replace(
        "\"strict\": true,",
        "\"strict\": true,\n    \"useUnknownInCatchVariables\": false,",
    );
    assert_eq!(
        findings(&source),
        vec![("tsconfig-option", Severity::Error)]
    );
}

#[test]
fn missing_unchecked_indexed_access_is_an_error_even_under_strict() {
    let source = SOUND.replace("    \"noUncheckedIndexedAccess\": true,\n", "");
    assert_eq!(
        findings(&source),
        vec![("tsconfig-option", Severity::Error)]
    );
}

#[test]
fn suppress_flags_are_rejected() {
    let source = SOUND.replace(
        "\"strict\": true,",
        "\"strict\": true,\n    \"suppressExcessPropertyErrors\": true,",
    );
    assert_eq!(
        findings(&source),
        vec![("tsconfig-option", Severity::Error)]
    );
}

#[test]
fn skip_lib_check_is_a_warning_and_does_not_reject() {
    let source = SOUND.replace(
        "\"strict\": true,",
        "\"strict\": true,\n    \"skipLibCheck\": true,",
    );
    let file = SourceFile::new("tsconfig.json", source);
    let report = check(&file);
    assert_eq!(report.count(Severity::Warning), 1);
    assert!(!report.rejected());
}

#[test]
fn missing_compiler_options_is_rejected() {
    assert_eq!(
        findings("{ \"include\": [\"src\"] }"),
        vec![("tsconfig-options", Severity::Error)]
    );
}

#[test]
fn extends_is_unsupported_rather_than_guessed() {
    let source = r#"{ "extends": "./base.json", "compilerOptions": { "strict": true } }"#;
    assert_eq!(
        findings(source),
        vec![("tsconfig-extends", Severity::Unsupported)]
    );
    let file = SourceFile::new("tsconfig.json", source);
    assert!(check(&file).rejected());
}

#[test]
fn a_typescript_file_is_not_treated_as_a_config() {
    let file = SourceFile::new("src/tsconfig.json.ts", "let x: any = 1;");
    let rules: Vec<&str> = check(&file)
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.rule)
        .collect();
    assert_eq!(rules, vec!["no-any"]);
}

#[test]
fn every_requirement_has_a_reason() {
    for requirement in savitar_sound::CONFIG_REQUIREMENTS {
        assert!(
            !requirement.reason.is_empty(),
            "requirement `{}` has no reason",
            requirement.option
        );
    }
}
