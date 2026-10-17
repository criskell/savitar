use std::collections::BTreeMap;

use oxc_ast::ast::{
    BindingPattern, Class, Declaration, ExportDefaultDeclarationKind, Function, ImportDeclaration,
    ImportDeclarationSpecifier, ImportOrExportKind, ModuleExportName, Statement as AstStatement,
    VariableDeclaration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleName {
    BoundByTheInitializer,
    AHoistedFunction,
    AClassBoundWhereDeclared,
    DeclaredWithNoCodeBehindIt,
    Imported { specifier: String, exported: String },
    AWholeModule { specifier: String },
}

#[derive(Debug, Default)]
pub struct ModuleNames {
    held: BTreeMap<String, ModuleName>,
}

impl ModuleNames {
    #[must_use]
    pub fn of(body: &[AstStatement<'_>]) -> Self {
        let mut names = Self::default();
        for statement in body {
            match statement {
                AstStatement::ImportDeclaration(declaration) => names.take_import(declaration),
                AstStatement::VariableDeclaration(declaration) => {
                    names.take_variables(declaration);
                }
                AstStatement::FunctionDeclaration(declaration) => {
                    names.take_function(declaration);
                }
                AstStatement::ClassDeclaration(class) => names.take_class(class),
                AstStatement::ExportDeclaration(export) => {
                    names.take_declaration(&export.declaration);
                }
                AstStatement::ExportDefaultDeclaration(export) => match &export.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(declaration) => {
                        names.take_function(declaration);
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                        names.take_class(class)
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        names
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ModuleName> {
        self.held.get(name)
    }

    fn put(&mut self, name: &str, held: ModuleName) {
        self.held.entry(name.to_string()).or_insert(held);
    }

    fn take_function(&mut self, declaration: &Function<'_>) {
        let Some(id) = declaration.id.as_ref() else {
            return;
        };
        if declaration.body.is_some() {
            self.held
                .insert(id.name.to_string(), ModuleName::AHoistedFunction);
        } else {
            self.put(id.name.as_str(), ModuleName::DeclaredWithNoCodeBehindIt);
        }
    }

    fn take_class(&mut self, class: &Class<'_>) {
        let Some(id) = class.id.as_ref() else {
            return;
        };
        if class.declare {
            self.put(id.name.as_str(), ModuleName::DeclaredWithNoCodeBehindIt);
        } else {
            self.put(id.name.as_str(), ModuleName::AClassBoundWhereDeclared);
        }
    }

    fn take_declaration(&mut self, declaration: &Declaration<'_>) {
        match declaration {
            Declaration::VariableDeclaration(variables) => self.take_variables(variables),
            Declaration::FunctionDeclaration(function) => self.take_function(function),
            Declaration::ClassDeclaration(class) => self.take_class(class),
            _ => {}
        }
    }

    fn take_variables(&mut self, declaration: &VariableDeclaration<'_>) {
        for declarator in &declaration.declarations {
            for name in names_bound_by(&declarator.id) {
                self.put(&name, ModuleName::BoundByTheInitializer);
            }
        }
    }

    fn take_import(&mut self, declaration: &ImportDeclaration<'_>) {
        if declaration.import_kind == ImportOrExportKind::Type {
            return;
        }
        let specifier = declaration.source.value.to_string();
        let Some(specifiers) = declaration.specifiers.as_ref() else {
            return;
        };
        for named in specifiers {
            match named {
                ImportDeclarationSpecifier::ImportSpecifier(one) => {
                    if one.import_kind == ImportOrExportKind::Type {
                        continue;
                    }
                    let exported = match &one.imported {
                        ModuleExportName::IdentifierName(name) => name.name.to_string(),
                        ModuleExportName::IdentifierReference(name) => name.name.to_string(),
                        ModuleExportName::StringLiteral(name) => name.value.to_string(),
                    };
                    self.put(
                        one.local.name.as_str(),
                        ModuleName::Imported {
                            specifier: specifier.clone(),
                            exported,
                        },
                    );
                }
                ImportDeclarationSpecifier::ImportDefaultSpecifier(one) => self.put(
                    one.local.name.as_str(),
                    ModuleName::Imported {
                        specifier: specifier.clone(),
                        exported: "default".to_string(),
                    },
                ),
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(one) => self.put(
                    one.local.name.as_str(),
                    ModuleName::AWholeModule {
                        specifier: specifier.clone(),
                    },
                ),
            }
        }
    }
}

#[must_use]
pub fn names_bound_by(pattern: &BindingPattern<'_>) -> Vec<String> {
    let mut found = Vec::new();
    gather(pattern, &mut found);
    found
}

fn gather(pattern: &BindingPattern<'_>, found: &mut Vec<String>) {
    match pattern {
        BindingPattern::BindingIdentifier(id) => found.push(id.name.to_string()),
        BindingPattern::AssignmentPattern(assignment) => gather(&assignment.left, found),
        BindingPattern::ArrayPattern(array) => {
            for element in array.elements.iter().flatten() {
                gather(element, found);
            }
            if let Some(rest) = array.rest.as_ref() {
                gather(&rest.argument, found);
            }
        }
        BindingPattern::ObjectPattern(object) => {
            for property in &object.properties {
                gather(&property.value, found);
            }
            if let Some(rest) = object.rest.as_ref() {
                gather(&rest.argument, found);
            }
        }
    }
}

#[must_use]
pub fn is_a_package_specifier(specifier: &str) -> bool {
    !specifier.starts_with('.') && !specifier.starts_with('/')
}
