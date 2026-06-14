//! Production SLOC counting over swc spans.
//!
//! Production SLOC is the number of non-blank, non-comment physical lines a
//! declaration spans. Counting works on the exact source slice delimited by the
//! declaration's swc span, so reformatting comments or blank lines elsewhere in
//! the file never shifts a symbol's size. Line (`//`) and block (`/* */`)
//! comments — including JSDoc `/** */` blocks — are stripped before a line is
//! judged blank.

/// Counts production SLOC in `source`: non-blank lines that carry code once all
/// comments have been removed.
///
/// `source` is expected to be the exact text a declaration's span covers.
#[must_use]
pub fn production_sloc(source: &str) -> u32 {
    let stripped = strip_comments(source);
    let lines = stripped
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    u32::try_from(lines).unwrap_or(u32::MAX)
}

/// Removes line and block comments from `source`, preserving newlines so that
/// physical line boundaries (and thus per-line blankness) are unchanged.
///
/// String and template literals are honored so that comment markers appearing
/// inside them are not mistaken for comments.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut state = ScanState::Code;

    while let Some(current) = chars.next() {
        match state {
            ScanState::Code => match current {
                '/' if chars.peek() == Some(&'/') => {
                    chars.next();
                    state = ScanState::LineComment;
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    state = ScanState::BlockComment;
                }
                '"' | '\'' | '`' => {
                    out.push(current);
                    state = ScanState::StringLiteral(current);
                }
                other => out.push(other),
            },
            ScanState::LineComment => {
                if current == '\n' {
                    out.push('\n');
                    state = ScanState::Code;
                }
            }
            ScanState::BlockComment => {
                if current == '\n' {
                    out.push('\n');
                } else if current == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    state = ScanState::Code;
                }
            }
            ScanState::StringLiteral(quote) => {
                out.push(current);
                if current == '\\' {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                } else if current == quote {
                    state = ScanState::Code;
                }
            }
        }
    }

    out
}

/// The lexer state machine used by [`strip_comments`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    /// Ordinary code.
    Code,
    /// Inside a `//` comment, until end of line.
    LineComment,
    /// Inside a `/* ... */` comment, until `*/`.
    BlockComment,
    /// Inside a string or template literal opened by the given quote.
    StringLiteral(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_count_only_non_blank_code_lines() {
        let source = "const a = 1;\n\nconst b = 2;\n";

        assert_eq!(production_sloc(source), 2);
    }

    #[test]
    fn should_exclude_line_comments() {
        let source = "const a = 1; // trailing\n// whole line\nconst b = 2;\n";

        assert_eq!(production_sloc(source), 2);
    }

    #[test]
    fn should_exclude_jsdoc_block_comments() {
        let source = "/**\n * doc\n * @param x\n */\nfunction f() {}\n";

        assert_eq!(production_sloc(source), 1);
    }

    #[test]
    fn should_not_treat_comment_markers_inside_strings_as_comments() {
        let source = "const url = \"http://x\";\nconst y = 2;\n";

        assert_eq!(production_sloc(source), 2);
    }

    #[test]
    fn should_count_zero_for_an_all_comment_block() {
        let source = "// a\n/* b\n c */\n";

        assert_eq!(production_sloc(source), 0);
    }
}
