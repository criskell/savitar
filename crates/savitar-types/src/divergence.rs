#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divergence {
    pub id: &'static str,
    pub what_tsc_answers: &'static str,
    pub what_savitar_answers: &'static str,
    pub why_savitar_refuses_to_follow: &'static str,
}

pub const INTENTIONAL_DIVERGENCES: &[Divergence] = &[
    Divergence {
        id: "unknown-where-tsc-answers-any",
        what_tsc_answers: "`any`, which is both top and bottom, so every proof passes through it",
        what_savitar_answers: "`unknown`, which has to be narrowed before it is used",
        why_savitar_refuses_to_follow: "a type that satisfies every relation is a hole in the relation, not a type",
    },
    Divergence {
        id: "readonly-counts-in-assignability",
        what_tsc_answers: "a mutable slot accepts a `readonly` source, because `readonly` is not part of the relation",
        what_savitar_answers: "a mutable slot demands a mutable source",
        why_savitar_refuses_to_follow: "writing through the mutable view of a `readonly` slot changes a value its declaration promised never changes",
    },
    Divergence {
        id: "mutable-position-is-invariant",
        what_tsc_answers: "a mutable position is covariant, so `Dog[]` fits where `Animal[]` is wanted and a narrower field redeclaration fits",
        what_savitar_answers: "a mutable position is invariant, and only a `readonly` position is covariant",
        why_savitar_refuses_to_follow: "a covariant mutable position lets a write through the wider view store a value the narrower view promised could not be there",
    },
];

#[must_use]
pub fn is_registered(id: &str) -> bool {
    INTENTIONAL_DIVERGENCES
        .iter()
        .any(|divergence| divergence.id == id)
}
