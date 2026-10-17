use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ran {
    BeforeAndPublished,
    NotBefore,
    WithoutPublishing,
    Unchecked,
}

#[derive(Debug, Default)]
pub struct ModuleStanding {
    by_specifier: BTreeMap<String, Ran>,
}

impl ModuleStanding {
    #[must_use]
    pub fn alone() -> Self {
        Self::default()
    }

    pub fn note(&mut self, specifier: &str, ran: Ran) {
        self.by_specifier.insert(specifier.to_string(), ran);
    }

    #[must_use]
    pub fn of(&self, specifier: &str) -> Ran {
        self.by_specifier
            .get(specifier)
            .copied()
            .unwrap_or(Ran::Unchecked)
    }
}
