use crate::{ModuleGraph, ModuleId, Target};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyUncheckedCodeReachesTheExports {
    TheProgramIsEnteredHere,
    NoModuleInTheProgramImportsIt,
}

impl WhyUncheckedCodeReachesTheExports {
    #[must_use]
    pub const fn said(self) -> &'static str {
        match self {
            Self::TheProgramIsEnteredHere => {
                "Savitar was asked to check this file, so the program is entered here"
            }
            Self::NoModuleInTheProgramImportsIt => {
                "no module in the program imports this file, so whatever it exports it exports to \
                 code Savitar never sees"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnTheEntrySurface {
    pub module: ModuleId,
    pub why: WhyUncheckedCodeReachesTheExports,
}

impl ModuleGraph {
    #[must_use]
    pub fn on_the_entry_surface(&self, id: ModuleId) -> Option<WhyUncheckedCodeReachesTheExports> {
        if self.entered_at.contains(&id) {
            return Some(WhyUncheckedCodeReachesTheExports::TheProgramIsEnteredHere);
        }
        if self.imported_by_a_module_in_the_program(id) {
            return None;
        }
        Some(WhyUncheckedCodeReachesTheExports::NoModuleInTheProgramImportsIt)
    }

    #[must_use]
    pub fn entry_surface(&self) -> Vec<OnTheEntrySurface> {
        self.modules()
            .filter_map(|(id, _)| {
                self.on_the_entry_surface(id)
                    .map(|why| OnTheEntrySurface { module: id, why })
            })
            .collect()
    }

    fn imported_by_a_module_in_the_program(&self, id: ModuleId) -> bool {
        self.modules().any(|(importer, module)| {
            importer != id
                && module
                    .imports
                    .iter()
                    .any(|import| import.target == Target::Module(id))
        })
    }
}
