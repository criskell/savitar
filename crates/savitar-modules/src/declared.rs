use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionExpression, Class, ClassElement, Declaration, ExportDefaultDeclarationKind,
    Expression, FormalParameter, Function, Statement as AstStatement,
};
use oxc_parser::Parser;
use oxc_span::SourceType;
use savitar_diagnostics::{Diagnostic, Span};

use crate::{Finding, ModuleGraph, holds_declarations_only};

const HOW_DEEP_THE_PROJECT_IS_WALKED: usize = 16;

#[derive(Debug, Default)]
pub struct TypesDeclaredBehindNoEdge {
    by_name: BTreeMap<String, PathBuf>,
}

impl TypesDeclaredBehindNoEdge {
    #[must_use]
    pub fn declaring(&self, name: &str) -> Option<&Path> {
        self.by_name.get(name).map(PathBuf::as_path)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.by_name.len()
    }

    pub fn named(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.by_name
            .iter()
            .map(|(name, path)| (name.as_str(), path.as_path()))
    }

    fn take(&mut self, name: &str, declaring: &Path) {
        self.by_name
            .entry(name.to_string())
            .or_insert_with(|| declaring.to_path_buf());
    }
}

#[must_use]
pub fn types_declared_behind_no_edge(root: &Path) -> TypesDeclaredBehindNoEdge {
    let mut found = TypesDeclaredBehindNoEdge::default();
    let mut files = Vec::new();
    collect_declaration_files(root, 0, &mut files);
    files.sort();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        take_global_types_of(&mut found, path, &text);
    }
    found
}

fn collect_declaration_files(at: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth > HOW_DEEP_THE_PROJECT_IS_WALKED {
        return;
    }
    let Ok(listing) = std::fs::read_dir(at) else {
        return;
    };
    for entry in listing.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            collect_declaration_files(&path, depth + 1, found);
        } else if holds_declarations_only(&path) {
            found.push(path);
        }
    }
}

fn take_global_types_of(found: &mut TypesDeclaredBehindNoEdge, path: &Path, text: &str) {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap_or_else(|_| SourceType::ts());
    let parsed = Parser::new(&allocator, text, source_type).parse();
    if parsed.fatal_error {
        return;
    }

    let reached_with_no_import = !parsed.program.body.iter().any(is_a_module_boundary);
    for statement in &parsed.program.body {
        if let AstStatement::TSGlobalDeclaration(augmentation) = statement {
            for inner in &augmentation.body.body {
                take_type_name_of(found, path, inner);
            }
            continue;
        }
        if reached_with_no_import {
            take_type_name_of(found, path, statement);
        }
    }
}

fn is_a_module_boundary(statement: &AstStatement<'_>) -> bool {
    matches!(
        statement,
        AstStatement::ImportDeclaration(_)
            | AstStatement::ExportDeclaration(_)
            | AstStatement::ExportNamedDeclaration(_)
            | AstStatement::ExportDefaultDeclaration(_)
            | AstStatement::ExportAllDeclaration(_)
            | AstStatement::ExportFromDeclaration(_)
    )
}

fn take_type_name_of(
    found: &mut TypesDeclaredBehindNoEdge,
    path: &Path,
    statement: &AstStatement<'_>,
) {
    match statement {
        AstStatement::TSInterfaceDeclaration(declaration) => {
            found.take(declaration.id.name.as_str(), path);
        }
        AstStatement::TSTypeAliasDeclaration(declaration) => {
            found.take(declaration.id.name.as_str(), path);
        }
        AstStatement::TSEnumDeclaration(declaration) => {
            found.take(declaration.id.name.as_str(), path);
        }
        AstStatement::TSNamespaceDeclaration(declaration) => {
            found.take(declaration.id.name.as_str(), path);
        }
        AstStatement::ClassDeclaration(class) => {
            if let Some(id) = class.id.as_ref() {
                found.take(id.name.as_str(), path);
            }
        }
        _ => {}
    }
}

#[must_use]
pub fn refusals_on_the_entry_surface(
    graph: &ModuleGraph,
    declared: &TypesDeclaredBehindNoEdge,
) -> Vec<Finding> {
    if declared.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for standing in graph.entry_surface() {
        if !graph.readable(standing.module) {
            continue;
        }
        let path = &graph.module(standing.module).path;
        if holds_declarations_only(path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for refusal in refusals_in_one_module(&text, path, declared) {
            findings.push(Finding {
                module: standing.module,
                diagnostic: refusal.told(standing.why.said()),
            });
        }
    }
    findings.sort_by_key(|finding| (finding.module, finding.diagnostic.span.start));
    findings
}

struct AValueComesInTypedByADeclaredBoundary {
    at: Span,
    type_name: String,
    declaring: PathBuf,
}

impl AValueComesInTypedByADeclaredBoundary {
    fn told(&self, why_it_is_reachable: &str) -> Diagnostic {
        let type_name = &self.type_name;
        let declaring = self.declaring.display();
        Diagnostic::unsupported(
            "unsupported-declared-type-on-the-entry-surface",
            self.at,
            format!(
                "this parameter takes in a value typed by `{type_name}`, which `{declaring}` \
                 declares with no checked code behind it, and {why_it_is_reachable}"
            ),
        )
        .with_help(
            "a declared type is sound only while every value that reaches it comes from checked \
             code; on the entry surface the caller is not checked, so nothing proves the value \
             has that shape",
        )
    }
}

fn refusals_in_one_module(
    text: &str,
    path: &Path,
    declared: &TypesDeclaredBehindNoEdge,
) -> Vec<AValueComesInTypedByADeclaredBoundary> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap_or_else(|_| SourceType::ts());
    let parsed = Parser::new(&allocator, text, source_type).parse();
    if parsed.fatal_error {
        return Vec::new();
    }

    let mut taken_in = Vec::new();
    for statement in &parsed.program.body {
        match statement {
            AstStatement::ExportDeclaration(export) => {
                parameters_of_declaration(&export.declaration, &mut taken_in);
            }
            AstStatement::ExportDefaultDeclaration(export) => match &export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(function) => {
                    parameters_of_function(function, &mut taken_in);
                }
                ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                    parameters_of_class(class, &mut taken_in);
                }
                ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
                    parameters_of_arrow(arrow, &mut taken_in);
                }
                ExportDefaultDeclarationKind::FunctionExpression(function) => {
                    parameters_of_function(function, &mut taken_in);
                }
                _ => {}
            },
            _ => {}
        }
    }

    taken_in
        .iter()
        .filter_map(|parameter| refusal_for_one_parameter(parameter, text, declared))
        .collect()
}

fn refusal_for_one_parameter(
    parameter: &FormalParameter<'_>,
    text: &str,
    declared: &TypesDeclaredBehindNoEdge,
) -> Option<AValueComesInTypedByADeclaredBoundary> {
    let annotation = parameter.type_annotation.as_ref()?;
    let written = names_written_in(text, annotation.span.start, annotation.span.end);
    written.iter().find_map(|name| {
        declared
            .declaring(name)
            .map(|declaring| AValueComesInTypedByADeclaredBoundary {
                at: Span::new(parameter.span.start, parameter.span.end),
                type_name: name.clone(),
                declaring: declaring.to_path_buf(),
            })
    })
}

fn names_written_in(text: &str, start: u32, end: u32) -> Vec<String> {
    let from = start as usize;
    let to = (end as usize).min(text.len());
    if from >= to {
        return Vec::new();
    }
    let mut names = Vec::new();
    let mut building = String::new();
    for character in text[from..to].chars() {
        if character.is_alphanumeric() || character == '_' || character == '$' {
            building.push(character);
            continue;
        }
        if !building.is_empty() {
            names.push(std::mem::take(&mut building));
        }
    }
    if !building.is_empty() {
        names.push(building);
    }
    names
}

fn parameters_of_declaration<'a>(
    declaration: &'a Declaration<'a>,
    taken_in: &mut Vec<&'a FormalParameter<'a>>,
) {
    match declaration {
        Declaration::FunctionDeclaration(function) => parameters_of_function(function, taken_in),
        Declaration::ClassDeclaration(class) => parameters_of_class(class, taken_in),
        Declaration::VariableDeclaration(variables) => {
            for declarator in &variables.declarations {
                match declarator.init.as_ref() {
                    Some(Expression::ArrowFunctionExpression(arrow)) => {
                        parameters_of_arrow(arrow, taken_in);
                    }
                    Some(Expression::FunctionExpression(function)) => {
                        parameters_of_function(function, taken_in);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn parameters_of_function<'a>(
    function: &'a Function<'a>,
    taken_in: &mut Vec<&'a FormalParameter<'a>>,
) {
    taken_in.extend(function.params.items.iter());
}

fn parameters_of_arrow<'a>(
    arrow: &'a ArrowFunctionExpression<'a>,
    taken_in: &mut Vec<&'a FormalParameter<'a>>,
) {
    taken_in.extend(arrow.params.items.iter());
}

fn parameters_of_class<'a>(class: &'a Class<'a>, taken_in: &mut Vec<&'a FormalParameter<'a>>) {
    for element in &class.body.body {
        if let ClassElement::MethodDefinition(method) = element {
            taken_in.extend(method.value.params.items.iter());
        }
    }
}
