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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwoModulesForOneSpecifier {
    pub specifier: String,
    pub first: String,
    pub second: String,
}

#[derive(Debug)]
struct Noted {
    module: String,
    standing: Standing,
}

#[derive(Debug, Default)]
pub struct ModuleStanding {
    by_specifier: BTreeMap<String, Noted>,
}

impl ModuleStanding {
    #[must_use]
    pub fn alone() -> Self {
        Self::default()
    }

    pub fn note(
        &mut self,
        specifier: &str,
        module: &str,
        standing: Standing,
    ) -> Result<(), TwoModulesForOneSpecifier> {
        if let Some(already) = self.by_specifier.get_mut(specifier) {
            if already.module == module {
                already.standing = standing;
                return Ok(());
            }
            let clash = TwoModulesForOneSpecifier {
                specifier: specifier.to_string(),
                first: already.module.clone(),
                second: module.to_string(),
            };
            already.standing = Standing::NotChecked;
            return Err(clash);
        }
        self.by_specifier.insert(
            specifier.to_string(),
            Noted {
                module: module.to_string(),
                standing,
            },
        );
        Ok(())
    }

    #[must_use]
    pub fn of(&self, specifier: &str, exported: &str) -> Ran {
        match self
            .by_specifier
            .get(specifier)
            .map(|noted| &noted.standing)
        {
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

#[cfg(test)]
mod tests {
    use super::{ModuleStanding, Ran, Standing, TwoModulesForOneSpecifier};
    use std::collections::BTreeSet;

    fn bound(name: &str) -> Standing {
        Standing::RanBeforeBinding(BTreeSet::from([name.to_string()]))
    }

    #[test]
    fn a_specifier_nobody_noted_is_unchecked() {
        let standing = ModuleStanding::alone();

        assert_eq!(standing.of("./settings.ts", "port"), Ran::Unchecked);
    }

    #[test]
    fn a_module_that_ran_before_answers_for_the_name_it_bound_and_not_for_another() {
        let mut standing = ModuleStanding::alone();
        standing
            .note("./settings.ts", "/project/settings.ts", bound("port"))
            .expect("the first note of a specifier is never a clash");

        assert_eq!(
            standing.of("./settings.ts", "port"),
            Ran::BeforeAndBoundTheName
        );
        assert_eq!(
            standing.of("./settings.ts", "host"),
            Ran::WithoutBindingTheName
        );
    }

    #[test]
    fn noting_the_same_module_twice_for_one_specifier_keeps_the_last_standing() {
        let mut standing = ModuleStanding::alone();
        standing
            .note(
                "./settings.ts",
                "/project/settings.ts",
                Standing::DidNotRunBefore,
            )
            .expect("the first note of a specifier is never a clash");
        standing
            .note("./settings.ts", "/project/settings.ts", bound("port"))
            .expect("the same module twice is not a clash");

        assert_eq!(
            standing.of("./settings.ts", "port"),
            Ran::BeforeAndBoundTheName
        );
    }

    #[test]
    fn noting_two_modules_for_one_specifier_is_refused_and_grants_nothing() {
        let mut standing = ModuleStanding::alone();
        standing
            .note("./settings.ts", "/project/a/settings.ts", bound("port"))
            .expect("the first note of a specifier is never a clash");

        let clash = standing
            .note("./settings.ts", "/project/b/settings.ts", bound("port"))
            .expect_err("one specifier resolving to two modules has to be refused");

        assert_eq!(
            clash,
            TwoModulesForOneSpecifier {
                specifier: "./settings.ts".to_string(),
                first: "/project/a/settings.ts".to_string(),
                second: "/project/b/settings.ts".to_string(),
            }
        );
        assert_eq!(standing.of("./settings.ts", "port"), Ran::Unchecked);
    }
}
