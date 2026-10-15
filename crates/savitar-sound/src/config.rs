use jsonc_parser::ast::{Object, ObjectProp, Value};
use jsonc_parser::{CollectOptions, ParseOptions, parse_to_ast};
use savitar_diagnostics::{Diagnostic, Severity, SourceFile, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Demand {
    MustBeTrue,
    MustNotBeTrue,
}

pub struct ConfigRequirement {
    pub option: &'static str,
    pub demand: Demand,
    pub severity: Severity,
    pub reason: &'static str,
}

pub static CONFIG_REQUIREMENTS: &[ConfigRequirement] = &[
    ConfigRequirement {
        option: "strict",
        demand: Demand::MustBeTrue,
        severity: Severity::Error,
        reason: "without it the checker's answers are not even nominally safe",
    },
    ConfigRequirement {
        option: "noUncheckedIndexedAccess",
        demand: Demand::MustBeTrue,
        severity: Severity::Error,
        reason: "without it `arr[i]` is typed `T` when it can be `undefined`",
    },
    ConfigRequirement {
        option: "exactOptionalPropertyTypes",
        demand: Demand::MustBeTrue,
        severity: Severity::Error,
        reason: "without it an optional field silently accepts an explicit `undefined`",
    },
    ConfigRequirement {
        option: "useUnknownInCatchVariables",
        demand: Demand::MustBeTrue,
        severity: Severity::Error,
        reason: "a caught value is `unknown`, and typing it `any` erases every proof",
    },
    ConfigRequirement {
        option: "noPropertyAccessFromIndexSignature",
        demand: Demand::MustBeTrue,
        severity: Severity::Error,
        reason: "without it `o.missing` reads through an index signature as if declared",
    },
    ConfigRequirement {
        option: "noImplicitOverride",
        demand: Demand::MustBeTrue,
        severity: Severity::Error,
        reason: "an accidental override changes dispatch the layout layer proved",
    },
    ConfigRequirement {
        option: "suppressImplicitAnyIndexErrors",
        demand: Demand::MustNotBeTrue,
        severity: Severity::Error,
        reason: "it hides exactly the reads that have no declared type",
    },
    ConfigRequirement {
        option: "suppressExcessPropertyErrors",
        demand: Demand::MustNotBeTrue,
        severity: Severity::Error,
        reason: "it lets an object carry fields its declared shape does not have",
    },
    ConfigRequirement {
        option: "skipLibCheck",
        demand: Demand::MustNotBeTrue,
        severity: Severity::Warning,
        reason: "declaration files then go unverified, and they are a trusted boundary",
    },
];

fn span_of(range: jsonc_parser::common::Range) -> Span {
    Span::new(
        u32::try_from(range.start).unwrap_or(0),
        u32::try_from(range.end).unwrap_or(0),
    )
}

fn property<'a, 'b>(object: &'a Object<'b>, name: &str) -> Option<&'a ObjectProp<'b>> {
    object
        .properties
        .iter()
        .find(|property| property.name.as_str() == name)
}

fn boolean(value: &Value<'_>) -> Option<bool> {
    match value {
        Value::BooleanLit(literal) => Some(literal.value),
        _ => None,
    }
}

#[must_use]
pub fn check_tsconfig(file: &SourceFile) -> Vec<Diagnostic> {
    let parsed = match parse_to_ast(
        file.text(),
        &CollectOptions::default(),
        &ParseOptions::default(),
    ) {
        Ok(parsed) => parsed,
        Err(error) => {
            return vec![Diagnostic::error(
                "parse",
                Span::new(0, 0),
                error.to_string(),
            )];
        }
    };

    let Some(Value::Object(root)) = parsed.value else {
        return vec![
            Diagnostic::error(
                "tsconfig-shape",
                Span::new(0, 0),
                "tsconfig.json must be an object",
            )
            .with_help("write `{ \"compilerOptions\": { ... } }`"),
        ];
    };

    if let Some(extends) = property(&root, "extends") {
        return vec![
            Diagnostic::unsupported(
                "tsconfig-extends",
                span_of(extends.range),
                "`extends` is not resolved yet, so the effective options are unknown",
            )
            .with_help("inline the options, or wait for config resolution"),
        ];
    }

    let Some(options) = property(&root, "compilerOptions") else {
        return vec![
            Diagnostic::error(
                "tsconfig-options",
                span_of(root.range),
                "no `compilerOptions`, so every soundness flag is off",
            )
            .with_help("add `compilerOptions` with `strict` and the flags `savitar config` lists"),
        ];
    };
    let Value::Object(options) = &options.value else {
        return vec![Diagnostic::error(
            "tsconfig-shape",
            span_of(options.range),
            "`compilerOptions` must be an object",
        )];
    };

    let strict_on = property(options, "strict")
        .and_then(|property| boolean(&property.value))
        .unwrap_or(false);

    let mut diagnostics = Vec::new();
    for requirement in CONFIG_REQUIREMENTS {
        let found = property(options, requirement.option);
        let value = found.and_then(|property| boolean(&property.value));
        let span = found.map_or_else(
            || span_of(options.range),
            |property| span_of(property.range),
        );

        let satisfied = match requirement.demand {
            Demand::MustBeTrue => match value {
                Some(true) => true,
                Some(false) => false,
                None => strict_on && implied_by_strict(requirement.option),
            },
            Demand::MustNotBeTrue => value != Some(true),
        };
        if satisfied {
            continue;
        }

        let message = match requirement.demand {
            Demand::MustBeTrue => format!(
                "`{}` must be true: {}",
                requirement.option, requirement.reason
            ),
            Demand::MustNotBeTrue => format!(
                "`{}` must not be true: {}",
                requirement.option, requirement.reason
            ),
        };
        let mut diagnostic = Diagnostic::error("tsconfig-option", span, message);
        diagnostic.severity = requirement.severity;
        diagnostics.push(diagnostic.with_help(format!(
            "set \"{}\": {} in compilerOptions",
            requirement.option,
            matches!(requirement.demand, Demand::MustBeTrue)
        )));
    }

    diagnostics.sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.rule));
    diagnostics
}

fn implied_by_strict(option: &str) -> bool {
    matches!(option, "useUnknownInCatchVariables")
}
