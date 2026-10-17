use std::collections::BTreeSet;

use savitar_diagnostics::{Diagnostic, Span};

use crate::{Carries, Finding, ModuleGraph, ModuleId, Target};

#[derive(Debug)]
pub struct Order {
    pub evaluated: Vec<ModuleId>,
    pub findings: Vec<Finding>,
}

impl Order {
    #[must_use]
    pub fn holds(&self, id: ModuleId) -> bool {
        self.evaluated.contains(&id)
    }

    #[must_use]
    pub fn position_of(&self, id: ModuleId) -> Option<usize> {
        self.evaluated.iter().position(|&placed| placed == id)
    }
}

pub fn of(graph: &ModuleGraph) -> Order {
    let every: Vec<ModuleId> = graph.modules().map(|(id, _)| id).collect();
    let mut evaluated: Vec<ModuleId> = Vec::new();
    let mut placed: BTreeSet<ModuleId> = BTreeSet::new();

    loop {
        let next = every.iter().copied().find(|&id| {
            !placed.contains(&id)
                && graph.readable(id)
                && graph.blocked_by_an_unchecked_import(id).is_none()
                && graph
                    .evaluated_before(id)
                    .iter()
                    .all(|needed| placed.contains(needed))
        });
        let Some(id) = next else { break };
        evaluated.push(id);
        placed.insert(id);
    }

    let left_out: BTreeSet<ModuleId> = every
        .iter()
        .copied()
        .filter(|id| !placed.contains(id))
        .collect();
    let findings = every
        .iter()
        .copied()
        .filter(|id| left_out.contains(id))
        .filter_map(|id| refusal_for(graph, id, &left_out))
        .collect();

    Order {
        evaluated,
        findings,
    }
}

fn refusal_for(
    graph: &ModuleGraph,
    id: ModuleId,
    left_out: &BTreeSet<ModuleId>,
) -> Option<Finding> {
    if !graph.readable(id) {
        return None;
    }
    if let Some(blocking) = graph.blocked_by_an_unchecked_import(id) {
        let specifier = &blocking.specifier;
        return Some(Finding {
            module: id,
            diagnostic: Diagnostic::unsupported(
                "unsupported-module-order",
                blocking.span,
                format!(
                    "this module imports values from `{specifier}`, which Savitar does not place in the evaluation order"
                ),
            )
            .with_help(
                "a module initializer runs only where every module it needs already ran",
            ),
        });
    }
    if let Some(span) = closes_a_cycle_at(graph, id) {
        return Some(Finding {
            module: id,
            diagnostic: Diagnostic::unsupported(
                "unsupported-import-cycle",
                span,
                "this import closes a cycle of value imports, so no module in the cycle has a first moment to run"
                    .to_string(),
            )
            .with_help(
                "break the cycle, or make the imports that close it `import type`, which carries no code",
            ),
        });
    }
    let reaching = graph
        .module(id)
        .imports
        .iter()
        .filter(|import| import.carries == Carries::Values)
        .find(|import| match import.target {
            Target::Module(other) => left_out.contains(&other),
            Target::DeclaredBoundary(_) | Target::Unresolved => false,
        })?;
    let specifier = &reaching.specifier;
    Some(Finding {
        module: id,
        diagnostic: Diagnostic::unsupported(
            "unsupported-module-order",
            reaching.span,
            format!(
                "`{specifier}` has no place in the evaluation order, so this module has none either"
            ),
        )
        .with_help("a module initializer runs only where every module it needs already ran"),
    })
}

fn closes_a_cycle_at(graph: &ModuleGraph, id: ModuleId) -> Option<Span> {
    graph
        .module(id)
        .imports
        .iter()
        .filter(|import| import.carries == Carries::Values)
        .find(|import| match import.target {
            Target::Module(other) => reaches(graph, other, id),
            Target::DeclaredBoundary(_) | Target::Unresolved => false,
        })
        .map(|import| import.span)
}

fn reaches(graph: &ModuleGraph, from: ModuleId, goal: ModuleId) -> bool {
    let mut seen: BTreeSet<ModuleId> = BTreeSet::new();
    let mut frontier = vec![from];
    while let Some(at) = frontier.pop() {
        if at == goal {
            return true;
        }
        if !seen.insert(at) {
            continue;
        }
        frontier.extend(graph.evaluated_before(at));
    }
    false
}
