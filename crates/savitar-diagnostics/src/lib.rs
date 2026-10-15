mod render;
mod source;

pub use render::render;
pub use source::{LineCol, SourceFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Unsupported,
}

impl Severity {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Unsupported => "unsupported",
        }
    }

    #[must_use]
    pub const fn rejects(self) -> bool {
        matches!(self, Self::Error | Self::Unsupported)
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub rule: &'static str,
    pub severity: Severity,
    pub message: String,
    pub span: Span,
    pub help: Option<String>,
}

impl Diagnostic {
    #[must_use]
    pub fn error(rule: &'static str, span: Span, message: impl Into<String>) -> Self {
        Self {
            rule,
            severity: Severity::Error,
            message: message.into(),
            span,
            help: None,
        }
    }

    #[must_use]
    pub fn unsupported(rule: &'static str, span: Span, message: impl Into<String>) -> Self {
        Self {
            rule,
            severity: Severity::Unsupported,
            message: message.into(),
            span,
            help: None,
        }
    }

    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}
