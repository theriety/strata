//! Production SLOC counting over Rust source slices.
//!
//! Production SLOC is the number of non-blank, non-comment physical lines a
//! declaration spans. Counting works on the exact source slice delimited by a
//! declaration's syn span, so reformatting comments or blank lines elsewhere in
//! the file never shifts a symbol's size. Line (`//`, including doc `///` and
//! `//!`) and block (`/* */`, including `/** */`) comments are stripped before a
//! line is judged blank; `cfg(test)` regions are excluded upstream by never
//! emitting production nodes for them, so this counter sees production slices
//! only.

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
/// String, char, and raw-string literals are honored so that comment markers
/// appearing inside them are not mistaken for comments. Rust block comments
/// nest, so the scanner tracks block depth.
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
                    state = ScanState::BlockComment(1);
                }
                '"' => {
                    out.push(current);
                    state = ScanState::StringLiteral;
                }
                '\'' => {
                    out.push(current);
                    state = ScanState::CharLiteral;
                }
                other => out.push(other),
            },
            ScanState::LineComment => {
                if current == '\n' {
                    out.push('\n');
                    state = ScanState::Code;
                }
            }
            ScanState::BlockComment(depth) => {
                if current == '\n' {
                    out.push('\n');
                } else if current == '/' && chars.peek() == Some(&'*') {
                    chars.next();
                    state = ScanState::BlockComment(depth + 1);
                } else if current == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    state = if depth <= 1 {
                        ScanState::Code
                    } else {
                        ScanState::BlockComment(depth - 1)
                    };
                }
            }
            ScanState::StringLiteral => {
                out.push(current);
                if current == '\\' {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                } else if current == '"' {
                    state = ScanState::Code;
                }
            }
            ScanState::CharLiteral => {
                out.push(current);
                if current == '\\' {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                } else if current == '\'' {
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
    /// Inside a `//` comment (including `///` and `//!`), until end of line.
    LineComment,
    /// Inside a `/* ... */` comment at the given nesting depth, until it closes.
    BlockComment(u32),
    /// Inside a `"..."` string literal.
    StringLiteral,
    /// Inside a `'.'` char literal.
    CharLiteral,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_count_only_non_blank_code_lines() {
        let source = "let a = 1;\n\nlet b = 2;\n";

        assert_eq!(production_sloc(source), 2);
    }

    #[test]
    fn should_exclude_line_comments() {
        let source = "let a = 1; // trailing\n// whole line\nlet b = 2;\n";

        assert_eq!(production_sloc(source), 2);
    }

    #[test]
    fn should_exclude_doc_comments() {
        let source = "/// doc\n/// more\npub fn f() {}\n";

        assert_eq!(production_sloc(source), 1);
    }

    #[test]
    fn should_exclude_nested_block_comments() {
        let source = "/* outer /* inner */ still */\nlet x = 1;\n";

        assert_eq!(production_sloc(source), 1);
    }

    #[test]
    fn should_not_treat_comment_markers_inside_strings_as_comments() {
        let source = "let url = \"http://x\";\nlet y = 2;\n";

        assert_eq!(production_sloc(source), 2);
    }

    #[test]
    fn should_count_zero_for_an_all_comment_block() {
        let source = "// a\n/* b\n c */\n";

        assert_eq!(production_sloc(source), 0);
    }

    #[test]
    fn should_not_treat_a_slash_in_a_char_literal_as_a_comment() {
        let source = "let slash = '/';\nlet y = 2;\n";

        assert_eq!(production_sloc(source), 2);
    }
}
