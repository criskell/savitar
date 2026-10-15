use oxc_ast::AstKind;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, Expression, MethodDefinitionKind, PropertyKind, TSEnumMember,
    TSType, TSTypeName,
};
use oxc_syntax::operator::UnaryOperator;
use savitar_diagnostics::{Diagnostic, Span};

pub struct Rule {
    pub id: &'static str,
    pub reason: &'static str,
    pub run: fn(AstKind<'_>) -> Option<Diagnostic>,
}

pub static RULES: &[Rule] = &[
    Rule {
        id: "no-any",
        reason: "`any` is both top and bottom, so no proof survives it",
        run: no_any,
    },
    Rule {
        id: "no-type-assertion",
        reason: "an assertion claims a type the compiler cannot verify",
        run: no_type_assertion,
    },
    Rule {
        id: "no-non-null-assertion",
        reason: "`!` claims presence without proving it",
        run: no_non_null_assertion,
    },
    Rule {
        id: "no-delete",
        reason: "removing a field breaks the shape a declared type promises",
        run: no_delete,
    },
    Rule {
        id: "no-accessor",
        reason: "a getter or setter turns a field read into an arbitrary call",
        run: no_accessor,
    },
    Rule {
        id: "no-proxy",
        reason: "a Proxy can intercept every operation on a proven shape",
        run: no_proxy,
    },
    Rule {
        id: "no-prototype-mutation",
        reason: "rewriting a prototype invalidates a shape already proven",
        run: no_prototype_mutation,
    },
    Rule {
        id: "no-eval",
        reason: "`eval` opens a world the build must see all of",
        run: no_eval,
    },
    Rule {
        id: "no-function-constructor",
        reason: "building a function from text opens the world at run time",
        run: no_function_constructor,
    },
    Rule {
        id: "closed-world-import",
        reason: "a computed specifier leaves the module graph open",
        run: closed_world_import,
    },
    Rule {
        id: "no-namespace",
        reason: "declaration merging means a shape is never final",
        run: no_namespace,
    },
    Rule {
        id: "no-arguments",
        reason: "`arguments` ignores the parameter list the signature declares",
        run: no_arguments,
    },
    Rule {
        id: "no-with",
        reason: "`with` makes every name in its body unresolvable at build time",
        run: no_with,
    },
    Rule {
        id: "no-sparse-array",
        reason: "a hole reads back as `undefined` where the type promises an element, and the engine wants a dense array",
        run: no_sparse_array,
    },
    Rule {
        id: "no-array-constructor-length",
        reason: "`new Array(n)` builds an array of holes, just like a sparse literal",
        run: no_array_constructor_length,
    },
    Rule {
        id: "no-import-equals",
        reason: "a legacy CommonJS form escapes the ESM module graph the closed world must see",
        run: no_import_equals,
    },
    Rule {
        id: "no-export-assignment",
        reason: "a legacy interoperability export is not an ESM module the closed world can see",
        run: no_export_assignment,
    },
    Rule {
        id: "no-numeric-enum",
        reason: "the reverse mapping types `E[k]` as `string` but returns `undefined` for a key that is not there",
        run: no_numeric_enum,
    },
];

fn span_of(span: oxc_span::Span) -> Span {
    Span::new(span.start, span.end)
}

fn no_any(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::TSAnyKeyword(node) = kind else {
        return None;
    };
    Some(
        Diagnostic::error("no-any", span_of(node.span), "`any` has no sound meaning")
            .with_help("use `unknown` and narrow it, or write the real type"),
    )
}

fn is_as_const(annotation: &TSType<'_>) -> bool {
    let TSType::TSTypeReference(reference) = annotation else {
        return false;
    };
    let TSTypeName::IdentifierReference(name) = &reference.type_name else {
        return false;
    };
    name.name == "const"
}

fn no_type_assertion(kind: AstKind<'_>) -> Option<Diagnostic> {
    match kind {
        AstKind::TSAsExpression(node) => {
            if is_as_const(&node.type_annotation) {
                return None;
            }
            Some(
                Diagnostic::error(
                    "no-type-assertion",
                    span_of(node.span),
                    "`as` asserts a type the compiler cannot verify",
                )
                .with_help("narrow the value with a check, or declare the type at its origin"),
            )
        }
        AstKind::TSTypeAssertion(node) => Some(
            Diagnostic::error(
                "no-type-assertion",
                span_of(node.span),
                "angle-bracket assertion asserts a type the compiler cannot verify",
            )
            .with_help("narrow the value with a check, or declare the type at its origin"),
        ),
        _ => None,
    }
}

fn no_non_null_assertion(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::TSNonNullExpression(node) = kind else {
        return None;
    };
    Some(
        Diagnostic::error(
            "no-non-null-assertion",
            span_of(node.span),
            "`!` claims a value is present without proving it",
        )
        .with_help("check for `null` and `undefined` before the use"),
    )
}

fn no_delete(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::UnaryExpression(node) = kind else {
        return None;
    };
    if node.operator != UnaryOperator::Delete {
        return None;
    }
    Some(
        Diagnostic::error(
            "no-delete",
            span_of(node.span),
            "`delete` removes a field a declared shape promises",
        )
        .with_help("model absence with `| undefined`, or use a Map"),
    )
}

fn callee_path(expression: &Expression<'_>) -> Option<String> {
    match expression {
        Expression::Identifier(identifier) => Some(identifier.name.to_string()),
        Expression::StaticMemberExpression(member) => {
            let object = callee_path(&member.object)?;
            Some(format!("{object}.{}", member.property.name))
        }
        _ => None,
    }
}

fn no_eval(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::CallExpression(node) = kind else {
        return None;
    };
    if callee_path(&node.callee).as_deref() != Some("eval") {
        return None;
    }
    Some(
        Diagnostic::error(
            "no-eval",
            span_of(node.span),
            "`eval` opens the world the compiler must see all of",
        )
        .with_help("move the behaviour into code the build can see"),
    )
}

fn closed_world_import(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::ImportExpression(node) = kind else {
        return None;
    };
    if matches!(node.source, Expression::StringLiteral(_)) {
        return None;
    }
    Some(
        Diagnostic::error(
            "closed-world-import",
            span_of(node.span),
            "a computed import specifier leaves the module graph open",
        )
        .with_help("use a literal specifier, or a map from a key to static imports"),
    )
}

const PROTOTYPE_SINKS: &[&str] = &[
    "Object.setPrototypeOf",
    "Reflect.setPrototypeOf",
    "Object.defineProperty",
    "Object.defineProperties",
    "Reflect.defineProperty",
];

fn no_prototype_mutation(kind: AstKind<'_>) -> Option<Diagnostic> {
    match kind {
        AstKind::CallExpression(node) => {
            let path = callee_path(&node.callee)?;
            if !PROTOTYPE_SINKS.contains(&path.as_str()) {
                return None;
            }
            Some(
                Diagnostic::error(
                    "no-prototype-mutation",
                    span_of(node.span),
                    format!("`{path}` can rewrite a shape the compiler proved"),
                )
                .with_help("declare the field or the method on the class instead"),
            )
        }
        AstKind::StaticMemberExpression(node) if node.property.name == "__proto__" => Some(
            Diagnostic::error(
                "no-prototype-mutation",
                span_of(node.span),
                "`__proto__` reaches the prototype of a proven shape",
            )
            .with_help("declare the field or the method on the class instead"),
        ),
        _ => None,
    }
}

fn no_with(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::WithStatement(node) = kind else {
        return None;
    };
    Some(
        Diagnostic::error(
            "no-with",
            span_of(node.span),
            "`with` makes every name in its body unresolvable at build time",
        )
        .with_help("bind what you need to a local"),
    )
}

fn no_accessor(kind: AstKind<'_>) -> Option<Diagnostic> {
    let (span, where_) = match kind {
        AstKind::ObjectProperty(node) if node.kind != PropertyKind::Init => {
            (node.span, "object literal")
        }
        AstKind::MethodDefinition(node)
            if matches!(
                node.kind,
                MethodDefinitionKind::Get | MethodDefinitionKind::Set
            ) =>
        {
            (node.span, "class")
        }
        _ => return None,
    };
    Some(
        Diagnostic::error(
            "no-accessor",
            span_of(span),
            format!("an accessor on a {where_} makes a field read run arbitrary code"),
        )
        .with_help("use a plain field, or a method with an explicit name"),
    )
}

fn new_callee_is(kind: AstKind<'_>, name: &str) -> Option<Span> {
    let AstKind::NewExpression(node) = kind else {
        return None;
    };
    let path = callee_path(&node.callee)?;
    if path != name {
        return None;
    }
    Some(span_of(node.span))
}

fn no_proxy(kind: AstKind<'_>) -> Option<Diagnostic> {
    let span = new_callee_is(kind, "Proxy")?;
    Some(
        Diagnostic::error(
            "no-proxy",
            span,
            "a Proxy can intercept every operation on a proven shape",
        )
        .with_help("wrap the value in a class with explicit methods"),
    )
}

fn no_function_constructor(kind: AstKind<'_>) -> Option<Diagnostic> {
    let span = new_callee_is(kind, "Function")?;
    Some(
        Diagnostic::error(
            "no-function-constructor",
            span,
            "`new Function` builds code the build cannot see",
        )
        .with_help("move the generation to build time, or write the function out"),
    )
}

fn no_namespace(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::TSNamespaceDeclaration(node) = kind else {
        return None;
    };
    Some(
        Diagnostic::error(
            "no-namespace",
            span_of(node.span),
            "a namespace can be merged later, so its shape is never final",
        )
        .with_help("use a module, and import what you need"),
    )
}

fn no_arguments(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::IdentifierReference(node) = kind else {
        return None;
    };
    if node.name != "arguments" {
        return None;
    }
    Some(
        Diagnostic::error(
            "no-arguments",
            span_of(node.span),
            "`arguments` ignores the parameter list the signature declares",
        )
        .with_help("declare a rest parameter"),
    )
}

fn no_sparse_array(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::ArrayExpression(node) = kind else {
        return None;
    };
    if !node
        .elements
        .iter()
        .any(|element| matches!(element, ArrayExpressionElement::Elision(_)))
    {
        return None;
    }
    Some(
        Diagnostic::error(
            "no-sparse-array",
            span_of(node.span),
            "an elided element reads back as `undefined` where the type promises an element",
        )
        .with_help("write every element out, or build the array with `map` over a dense source"),
    )
}

fn no_array_constructor_length(kind: AstKind<'_>) -> Option<Diagnostic> {
    let span = new_callee_is(kind, "Array")?;
    let AstKind::NewExpression(node) = kind else {
        return None;
    };
    let [only] = node.arguments.as_slice() else {
        return None;
    };
    if matches!(only, Argument::SpreadElement(_)) {
        return None;
    }
    Some(
        Diagnostic::error(
            "no-array-constructor-length",
            span,
            "`new Array(n)` builds an array of holes, just like a sparse literal",
        )
        .with_help("list the elements, or fill the length with `Array.from({ length: n }, ...)`"),
    )
}

fn no_import_equals(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::TSImportEqualsDeclaration(node) = kind else {
        return None;
    };
    Some(
        Diagnostic::error(
            "no-import-equals",
            span_of(node.span),
            "`import x = ...` is a legacy form outside the ESM module graph",
        )
        .with_help("use an ESM `import` declaration"),
    )
}

fn no_export_assignment(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::TSExportAssignment(node) = kind else {
        return None;
    };
    Some(
        Diagnostic::error(
            "no-export-assignment",
            span_of(node.span),
            "`export = x` is a legacy interoperability export, not an ESM module",
        )
        .with_help("use `export default x`, or named exports"),
    )
}

fn enum_member_has_string_literal_initializer(member: &TSEnumMember<'_>) -> bool {
    matches!(member.initializer, Some(Expression::StringLiteral(_)))
}

fn no_numeric_enum(kind: AstKind<'_>) -> Option<Diagnostic> {
    let AstKind::TSEnumDeclaration(node) = kind else {
        return None;
    };
    if node.r#const {
        return None;
    }
    if node
        .body
        .members
        .iter()
        .all(enum_member_has_string_literal_initializer)
    {
        return None;
    }
    Some(
        Diagnostic::error(
            "no-numeric-enum",
            span_of(node.span),
            "the reverse mapping of a numeric enum types `E[k]` as `string` but returns `undefined` for a key that is not there",
        )
        .with_help("use `const enum`, or give every member a string literal initializer"),
    )
}
