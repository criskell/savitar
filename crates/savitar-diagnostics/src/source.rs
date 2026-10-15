use crate::Span;

#[derive(Debug, Clone)]
pub struct SourceFile {
    name: String,
    text: String,
    line_starts: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    pub line: u32,
    pub column: u32,
}

impl SourceFile {
    #[must_use]
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        let text: String = text.into();
        let mut line_starts = vec![0u32];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(u32::try_from(offset + 1).unwrap_or(u32::MAX));
            }
        }
        Self {
            name: name.into(),
            text,
            line_starts,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn snippet(&self, span: Span) -> &str {
        let start = (span.start as usize).min(self.text.len());
        let end = (span.end as usize).min(self.text.len());
        if start >= end {
            return "";
        }
        self.text.get(start..end).unwrap_or("")
    }

    #[must_use]
    pub fn line_text(&self, line: u32) -> &str {
        let index = line.saturating_sub(1) as usize;
        let Some(&start) = self.line_starts.get(index) else {
            return "";
        };
        let end = self
            .line_starts
            .get(index + 1)
            .map_or(self.text.len(), |&next| next as usize);
        self.text
            .get(start as usize..end)
            .unwrap_or("")
            .trim_end_matches(['\n', '\r'])
    }

    #[must_use]
    pub fn line_col(&self, offset: u32) -> LineCol {
        let index = self.line_starts.partition_point(|&start| start <= offset);
        let line = index.max(1);
        let line_start = self.line_starts[line - 1] as usize;
        let upto = (offset as usize).min(self.text.len());
        let column = self
            .text
            .get(line_start..upto)
            .map_or(0, |prefix| prefix.chars().count());
        LineCol {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            column: u32::try_from(column + 1).unwrap_or(u32::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_offset_is_line_one_column_one() {
        let file = SourceFile::new("a.ts", "const x = 1;\n");
        assert_eq!(file.line_col(0), LineCol { line: 1, column: 1 });
    }

    #[test]
    fn offset_after_newline_starts_next_line() {
        let file = SourceFile::new("a.ts", "a;\nbb;\n");
        assert_eq!(file.line_col(3), LineCol { line: 2, column: 1 });
        assert_eq!(file.line_col(4), LineCol { line: 2, column: 2 });
    }

    #[test]
    fn column_counts_characters_not_bytes() {
        let file = SourceFile::new("a.ts", "const acao = 1;");
        let offset = u32::try_from("const ac".len()).unwrap();
        assert_eq!(file.line_col(offset), LineCol { line: 1, column: 9 });
    }

    #[test]
    fn line_text_drops_the_terminator() {
        let file = SourceFile::new("a.ts", "one;\r\ntwo;\n");
        assert_eq!(file.line_text(1), "one;");
        assert_eq!(file.line_text(2), "two;");
        assert_eq!(file.line_text(99), "");
    }

    #[test]
    fn snippet_is_clamped_to_the_text() {
        let file = SourceFile::new("a.ts", "abc");
        assert_eq!(file.snippet(Span::new(1, 2)), "b");
        assert_eq!(file.snippet(Span::new(2, 99)), "c");
        assert_eq!(file.snippet(Span::new(9, 9)), "");
    }
}
