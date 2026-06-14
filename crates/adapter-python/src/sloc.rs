//! Production SLOC counting over Python source slices.
//!
//! Production SLOC is the number of non-blank, non-comment physical lines a
//! declaration spans. Counting works on the exact source slice the binder
//! attributes to a declaration (already trimmed of any leading docstring), so a
//! reformatted comment or blank line never shifts a symbol's size. `#` line
//! comments are stripped before a line is judged blank; string-literal `#`
//! markers are honored so they are never mistaken for comments.

/// Counts production SLOC in `source`: non-blank lines that carry code once all
/// `#` comments have been removed.
///
/// `source` is expected to be the exact text a declaration spans, with any
/// leading docstring already removed by the caller.
#[must_use]
pub fn production_sloc(source: &str) -> u32 {
    let stripped = strip_comments(source);
    let lines = stripped
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    u32::try_from(lines).unwrap_or(u32::MAX)
}

/// Removes `#` line comments from `source`, preserving newlines so that physical
/// line boundaries (and thus per-line blankness) are unchanged.
///
/// String literals (single, double, and triple-quoted) are honored so a `#`
/// inside a string is not mistaken for a comment marker.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut state = ScanState::Code;

    while let Some(current) = chars.next() {
        match state {
            ScanState::Code => match current {
                '#' => state = ScanState::LineComment,
                '"' | '\'' => {
                    let kind = open_string(current, &mut chars, &mut out);
                    state = ScanState::StringLiteral(kind);
                }
                other => out.push(other),
            },
            ScanState::LineComment => {
                if current == '\n' {
                    out.push('\n');
                    state = ScanState::Code;
                }
            }
            ScanState::StringLiteral(kind) => {
                out.push(current);
                if current == '\\' {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                } else if closes_string(current, kind, &mut chars, &mut out) {
                    state = ScanState::Code;
                }
            }
        }
    }

    out
}

/// Opens a string literal, detecting a triple-quoted form and emitting the
/// already-consumed opening quote(s). Returns the kind of string opened.
fn open_string(
    quote: char,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    out: &mut String,
) -> StringKind {
    out.push(quote);
    if chars.peek() == Some(&quote) {
        // Could be a triple quote. Consume the second quote and look ahead.
        let second = chars.next().unwrap_or(quote);
        out.push(second);
        if chars.peek() == Some(&quote) {
            let third = chars.next().unwrap_or(quote);
            out.push(third);
            return StringKind::Triple(quote);
        }
        // Two quotes only: an empty string literal already closed.
        return StringKind::Closed;
    }
    StringKind::Single(quote)
}

/// Reports whether `current` closes the open string, consuming the trailing two
/// quotes of a triple-quoted close.
fn closes_string(
    current: char,
    kind: StringKind,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    out: &mut String,
) -> bool {
    match kind {
        StringKind::Single(quote) => current == quote,
        StringKind::Triple(quote) => {
            if current != quote {
                return false;
            }
            if chars.peek() == Some(&quote) {
                out.push(chars.next().unwrap_or(quote));
                if chars.peek() == Some(&quote) {
                    out.push(chars.next().unwrap_or(quote));
                    return true;
                }
            }
            false
        }
        StringKind::Closed => true,
    }
}

/// The lexer state machine used by [`strip_comments`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    /// Ordinary code.
    Code,
    /// Inside a `#` comment, until end of line.
    LineComment,
    /// Inside a string literal of the given kind.
    StringLiteral(StringKind),
}

/// The flavor of an open string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringKind {
    /// A single- or double-quoted string closed by one matching quote.
    Single(char),
    /// A triple-quoted string closed by three matching quotes.
    Triple(char),
    /// A string already closed on open (the empty `""` / `''` case).
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_count_only_non_blank_code_lines() {
        let source = "a = 1\n\nb = 2\n";

        assert_eq!(production_sloc(source), 2);
    }

    #[test]
    fn should_exclude_hash_comments() {
        let source = "a = 1  # trailing\n# whole line\nb = 2\n";

        assert_eq!(production_sloc(source), 2);
    }

    #[test]
    fn should_not_treat_hash_inside_a_string_as_a_comment() {
        let source = "url = \"http://x#frag\"\ny = 2\n";

        assert_eq!(production_sloc(source), 2);
    }

    #[test]
    fn should_count_zero_for_an_all_comment_block() {
        let source = "# a\n# b\n";

        assert_eq!(production_sloc(source), 0);
    }

    #[test]
    fn should_count_a_triple_quoted_string_assignment_as_code() {
        let source = "text = \"\"\"line one\nline two\n\"\"\"\n";

        // Three physical, non-blank lines of a multi-line string assignment.
        assert_eq!(production_sloc(source), 3);
    }
}
