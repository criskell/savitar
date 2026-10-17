use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ran {
    BeforeAndBoundTheName,
    NotBefore,
    WithoutBindingTheName,
    Unchecked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    RanBeforeBinding(BTreeSet<String>),
    DidNotRunBefore,
    NotChecked,
}

#[derive(Debug, Default)]
pub struct ModuleStanding {
    by_specifier: BTreeMap<String, Standing>,
}

impl ModuleStanding {
    #[must_use]
    pub fn alone() -> Self {
        Self::default()
    }

    pub fn note(&mut self, specifier: &str, standing: Standing) {
        self.by_specifier.insert(specifier.to_string(), standing);
    }

    #[must_use]
    pub fn of(&self, specifier: &str, exported: &str) -> Ran {
        match self.by_specifier.get(specifier) {
            None | Some(Standing::NotChecked) => Ran::Unchecked,
            Some(Standing::DidNotRunBefore) => Ran::NotBefore,
            Some(Standing::RanBeforeBinding(bound)) => {
                if bound.contains(exported) {
                    Ran::BeforeAndBoundTheName
                } else {
                    Ran::WithoutBindingTheName
                }
            }
        }
    }
}
