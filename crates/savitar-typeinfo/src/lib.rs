use savitar_diagnostics::Span;
use savitar_types::{TypeId, TypeTable};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Known(TypeId),
    Unsupported(String),
    OutOfScope,
}

impl Answer {
    #[must_use]
    pub fn known(&self) -> Option<TypeId> {
        match self {
            Self::Known(id) => Some(*id),
            Self::Unsupported(_) | Self::OutOfScope => None,
        }
    }
}

pub trait TypeProvider {
    fn name(&self) -> &'static str;

    fn type_at(&self, table: &mut TypeTable, file: &str, span: Span) -> Answer;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NullProvider;

impl TypeProvider for NullProvider {
    fn name(&self) -> &'static str {
        "null"
    }

    fn type_at(&self, _table: &mut TypeTable, _file: &str, _span: Span) -> Answer {
        Answer::OutOfScope
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_provider_never_guesses() {
        let mut table = TypeTable::new();
        let answer = NullProvider.type_at(&mut table, "a.ts", Span::new(0, 1));
        assert_eq!(answer, Answer::OutOfScope);
        assert_eq!(answer.known(), None);
        assert!(table.is_empty());
    }
}
