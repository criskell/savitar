mod order;
mod resolve;
mod specifiers;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use savitar_diagnostics::{Diagnostic, Span};

pub use order::Order;
use resolve::SpecifierResolver;
pub use resolve::tsconfig_beside;
use specifiers::specifiers_of;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(u32);

impl ModuleId {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carries {
    Values,
    TypesOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Module(ModuleId),
    Package,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub specifier: String,
    pub span: Span,
    pub carries: Carries,
    pub target: Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub path: PathBuf,
    pub imports: Vec<Import>,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub module: ModuleId,
    pub diagnostic: Diagnostic,
}

#[derive(Debug)]
pub struct ModuleGraph {
    modules: Vec<Module>,
    by_path: BTreeMap<PathBuf, ModuleId>,
    findings: Vec<Finding>,
    unreadable: BTreeSet<ModuleId>,
}

impl ModuleGraph {
    #[must_use]
    pub fn build(root: &Path, entries: &[PathBuf]) -> Self {
        let resolver = SpecifierResolver::rooted_at(root);
        let mut graph = Self {
            modules: Vec::new(),
            by_path: BTreeMap::new(),
            findings: Vec::new(),
            unreadable: BTreeSet::new(),
        };
        let mut queue: Vec<PathBuf> = Vec::new();
        let mut sorted: Vec<PathBuf> = entries.iter().map(|path| absolute(root, path)).collect();
        sorted.sort();
        sorted.dedup();
        for path in sorted {
            if graph.reserve(&path).is_some() {
                queue.push(path);
            }
        }

        let mut at = 0usize;
        while at < queue.len() {
            let path = queue[at].clone();
            at += 1;
            let id = graph.by_path[&path];
            let Ok(text) = std::fs::read_to_string(&path) else {
                graph.refuse_unreadable(id);
                continue;
            };
            for found in specifiers_of(&path, &text) {
                let from = path.parent().unwrap_or(root);
                let target = match resolver.resolve(from, &found.specifier) {
                    resolve::Resolved::File(resolved) => {
                        let fresh = graph.reserve(&resolved);
                        if let Some(fresh) = fresh {
                            queue.push(resolved.clone());
                            Target::Module(fresh)
                        } else {
                            Target::Module(graph.by_path[&resolved])
                        }
                    }
                    resolve::Resolved::InsidePackage => Target::Package,
                    resolve::Resolved::Nowhere => Target::Unresolved,
                };
                graph.note(id, &found, &target);
                graph.modules[id.0 as usize].imports.push(Import {
                    specifier: found.specifier,
                    span: found.span,
                    carries: found.carries,
                    target,
                });
            }
        }

        graph.findings.sort_by_key(|finding| {
            (
                finding.module,
                finding.diagnostic.span.start,
                finding.diagnostic.rule,
            )
        });
        graph
    }

    fn reserve(&mut self, path: &Path) -> Option<ModuleId> {
        if self.by_path.contains_key(path) {
            return None;
        }
        let id = ModuleId(u32::try_from(self.modules.len()).expect("module overflow"));
        self.modules.push(Module {
            path: path.to_path_buf(),
            imports: Vec::new(),
        });
        self.by_path.insert(path.to_path_buf(), id);
        Some(id)
    }

    fn refuse_unreadable(&mut self, module: ModuleId) {
        self.unreadable.insert(module);
        let path = self.module(module).path.display().to_string();
        self.findings.push(Finding {
            module,
            diagnostic: Diagnostic::error(
                "unreadable-module",
                Span::new(0, 0),
                format!("`{path}` is in the module graph and cannot be read"),
            ),
        });
    }

    fn note(&mut self, module: ModuleId, found: &specifiers::Found, target: &Target) {
        let specifier = &found.specifier;
        let diagnostic = match target {
            Target::Module(_) => return,
            Target::Package => Diagnostic::unsupported(
                "unsupported-package-import",
                found.span,
                format!(
                    "`{specifier}` resolves inside `node_modules`, and nothing a package declares is checked by Savitar"
                ),
            )
            .with_help("the package boundary is declared, not verified, so no order crosses it"),
            Target::Unresolved => Diagnostic::error(
                "unresolved-import",
                found.span,
                format!("`{specifier}` resolves to no file"),
            )
            .with_help("the module graph needs the file the specifier names"),
        };
        self.findings.push(Finding { module, diagnostic });
    }

    #[must_use]
    pub fn module(&self, id: ModuleId) -> &Module {
        &self.modules[id.0 as usize]
    }

    pub fn modules(&self) -> impl Iterator<Item = (ModuleId, &Module)> {
        self.modules
            .iter()
            .enumerate()
            .map(|(index, module)| (ModuleId(u32::try_from(index).unwrap_or(u32::MAX)), module))
    }

    #[must_use]
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    #[must_use]
    pub fn id_of(&self, path: &Path) -> Option<ModuleId> {
        self.by_path.get(path).copied()
    }

    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    #[must_use]
    pub fn readable(&self, id: ModuleId) -> bool {
        !self.unreadable.contains(&id)
    }

    #[must_use]
    pub fn evaluation_order(&self) -> Order {
        order::of(self)
    }

    fn evaluated_before(&self, id: ModuleId) -> Vec<ModuleId> {
        let mut needed: Vec<ModuleId> = self
            .module(id)
            .imports
            .iter()
            .filter(|import| import.carries == Carries::Values)
            .filter_map(|import| match import.target {
                Target::Module(other) => Some(other),
                Target::Package | Target::Unresolved => None,
            })
            .collect();
        needed.sort_unstable();
        needed.dedup();
        needed
    }

    fn blocked_by_an_unchecked_import(&self, id: ModuleId) -> Option<&Import> {
        self.module(id)
            .imports
            .iter()
            .filter(|import| import.carries == Carries::Values)
            .find(|import| matches!(import.target, Target::Package | Target::Unresolved))
    }
}

fn absolute(root: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    joined.canonicalize().unwrap_or(joined)
}
