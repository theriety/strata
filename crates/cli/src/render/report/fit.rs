//! Width fitting that splits overflowing report lines losslessly.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Splits overflowing tokens losslessly; continuation indentation is presentation only.
pub(super) fn fit_lines(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().flat_map(|line| fit_line(&line)).collect()
}

fn fit_line(line: &str) -> Vec<String> {
    const MAX_WIDTH: usize = 100;

    if line.width() <= MAX_WIDTH {
        return vec![line.to_owned()];
    }

    let continuation = continuation_prefix(line);
    let mut remaining = line;
    let mut fitted = Vec::new();
    while !remaining.is_empty() {
        let prefix = if fitted.is_empty() {
            String::new()
        } else {
            compact_continuation(&continuation, remaining, MAX_WIDTH)
        };
        let available = MAX_WIDTH.saturating_sub(prefix.width());
        if remaining.width() <= available {
            fitted.push(format!("{prefix}{remaining}"));
            break;
        }

        let end = preferred_break(remaining, available);
        fitted.push(format!("{prefix}{}", &remaining[..end]));
        remaining = &remaining[end..];
    }
    fitted
}

fn compact_continuation(continuation: &str, remaining: &str, max_width: usize) -> String {
    const MIN_PAYLOAD_WIDTH: usize = 20;

    let next_width = remaining
        .chars()
        .next()
        .and_then(UnicodeWidthChar::width)
        .unwrap_or(0)
        .max(1);
    let reserved_width = MIN_PAYLOAD_WIDTH.max(next_width).min(max_width);
    let available = max_width.saturating_sub(reserved_width);
    if continuation.width() <= available {
        return continuation.to_owned();
    }

    let elision = "… ";
    let suffix_width = available.saturating_sub(elision.width()) / 4 * 4;
    let mut retained_width = 0;
    let mut suffix_start = continuation.len();
    for (offset, character) in continuation.char_indices().rev() {
        let character_width = character.width().unwrap_or(0);
        if retained_width + character_width > suffix_width {
            break;
        }
        retained_width += character_width;
        suffix_start = offset;
    }
    format!("{elision}{}", &continuation[suffix_start..])
}

fn preferred_break(text: &str, available: usize) -> usize {
    let mut width = 0;
    let hard_end = text
        .char_indices()
        .find_map(|(offset, character)| {
            width += character.width().unwrap_or(0);
            (width > available).then_some(offset)
        })
        .unwrap_or(text.len());

    text[..hard_end]
        .char_indices()
        .rev()
        .find_map(|(offset, character)| {
            (character.is_whitespace() || character == '/').then_some(offset + character.len_utf8())
        })
        .filter(|offset| *offset > 0)
        .unwrap_or_else(|| {
            if hard_end == 0 {
                text.chars().next().map_or(0, char::len_utf8)
            } else {
                hard_end
            }
        })
}

fn continuation_prefix(line: &str) -> String {
    let connector = [("├── ", "│   "), ("└── ", "    ")]
        .into_iter()
        .filter_map(|(connector, continuation)| {
            line.find(connector)
                .map(|offset| (offset, connector, continuation))
        })
        .min_by_key(|(offset, _, _)| *offset);

    if let Some((offset, _, continuation)) = connector {
        let prefix = &line[..offset];
        if prefix
            .chars()
            .all(|character| character == ' ' || character == '│')
        {
            return format!("{prefix}{continuation}");
        }
    }

    line.chars()
        .take_while(|character| character.is_whitespace())
        .collect()
}
