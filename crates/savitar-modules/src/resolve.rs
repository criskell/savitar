use std::path::{Component, Path, PathBuf};

use oxc_resolver::{
    ResolveOptions, Resolver, TsconfigDiscovery, TsconfigOptions, TsconfigReferences,
};

pub enum Resolved {
    File(PathBuf),
    BehindADeclaredBoundary(DeclaredBoundary),
    Nowhere,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredBoundary {
    APackage,
    ADeclarationFile(PathBuf),
}

impl DeclaredBoundary {
    #[must_use]
    pub fn said(&self) -> String {
        match self {
            Self::APackage => "a package in `node_modules`".to_string(),
            Self::ADeclarationFile(path) => {
                format!("the declaration file `{}`", path.display())
            }
        }
    }
}

pub struct SpecifierResolver {
    resolver: Resolver,
}

#[must_use]
pub fn tsconfig_beside(root: &Path) -> Option<PathBuf> {
    let candidate = root.join("tsconfig.json");
    candidate.is_file().then_some(candidate)
}

impl SpecifierResolver {
    #[must_use]
    pub fn rooted_at(root: &Path) -> Self {
        let options = ResolveOptions {
            extensions: vec![
                ".ts".into(),
                ".tsx".into(),
                ".d.ts".into(),
                ".js".into(),
                ".jsx".into(),
                ".json".into(),
            ],
            extension_alias: vec![
                (
                    ".js".into(),
                    vec![".ts".into(), ".tsx".into(), ".d.ts".into(), ".js".into()],
                ),
                (".mjs".into(), vec![".mts".into(), ".mjs".into()]),
                (".cjs".into(), vec![".cts".into(), ".cjs".into()]),
            ],
            condition_names: vec!["types".into(), "import".into(), "node".into()],
            main_fields: vec!["types".into(), "module".into(), "main".into()],
            tsconfig: tsconfig_beside(root).map(|config_file| {
                TsconfigDiscovery::Manual(TsconfigOptions {
                    config_file,
                    references: TsconfigReferences::Auto,
                })
            }),
            ..ResolveOptions::default()
        };
        Self {
            resolver: Resolver::new(options),
        }
    }

    pub fn resolve(&self, from: &Path, specifier: &str) -> Resolved {
        let Ok(resolution) = self.resolver.resolve(from, specifier) else {
            return Resolved::Nowhere;
        };
        let path = resolution.into_path_buf();
        if reaches_into_a_package(&path) {
            return Resolved::BehindADeclaredBoundary(DeclaredBoundary::APackage);
        }
        if holds_declarations_only(&path) {
            return Resolved::BehindADeclaredBoundary(DeclaredBoundary::ADeclarationFile(path));
        }
        Resolved::File(path.canonicalize().unwrap_or(path))
    }
}

fn reaches_into_a_package(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::Normal(name) if name == "node_modules"))
}

#[must_use]
pub fn holds_declarations_only(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    [".d.ts", ".d.tsx", ".d.mts", ".d.cts"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}
