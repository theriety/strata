//! The shared display limit for every list in the human text faces.

use super::RenderOptions;

/// The number of items each list shows unless `--full` is given.
const DISPLAY_LIMIT: usize = 5;

/// Selects the items one list shows, plus how many it hides.
///
/// With `--full`, or when no more than [`DISPLAY_LIMIT`] items exist, every item
/// is returned in its existing order, so a short list reads the same either way.
/// Otherwise the items are stably ordered by `rank` (lower is worse, so shown
/// first), the first [`DISPLAY_LIMIT`] are kept, and the remainder is counted as
/// hidden.
pub(super) fn take_worst<T, K: Ord>(
    items: &[T],
    options: RenderOptions,
    rank: impl Fn(&T) -> K,
) -> (Vec<&T>, usize) {
    let mut selected: Vec<&T> = items.iter().collect();
    if options.full || selected.len() <= DISPLAY_LIMIT {
        return (selected, 0);
    }
    selected.sort_by_key(|item| rank(item));
    let hidden = selected.len().saturating_sub(DISPLAY_LIMIT);
    selected.truncate(DISPLAY_LIMIT);
    (selected, hidden)
}

/// Returns the single line announcing `hidden` omitted items, indented like the
/// list items, or nothing when no item is hidden.
pub(super) fn more_line(hidden: usize, indent: usize) -> Option<String> {
    (hidden > 0).then(|| {
        format!(
            "{}… {hidden} more — rerun with --full to see all",
            " ".repeat(indent)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPPED: RenderOptions = RenderOptions {
        verbose: false,
        full: false,
    };
    const FULL: RenderOptions = RenderOptions {
        verbose: false,
        full: true,
    };

    #[test]
    fn should_keep_the_first_five_and_count_the_rest() {
        let items: Vec<u32> = (0..8).collect();

        let (shown, hidden) = take_worst(&items, CAPPED, |_| ());

        assert_eq!(shown, vec![&0, &1, &2, &3, &4]);
        assert_eq!(hidden, 3);
    }

    #[test]
    fn should_hide_nothing_at_exactly_five_items() {
        let items: Vec<u32> = (0..5).collect();

        let (shown, hidden) = take_worst(&items, CAPPED, |_| ());

        assert_eq!(shown.len(), 5);
        assert_eq!(hidden, 0);
        assert_eq!(more_line(hidden, 2), None);
    }

    #[test]
    fn should_show_the_worst_ranked_items_first_when_capped() {
        let items = [9, 1, 8, 2, 7, 3, 6];

        let (shown, hidden) = take_worst(&items, CAPPED, |item| *item);

        assert_eq!(shown, vec![&1, &2, &3, &6, &7]);
        assert_eq!(hidden, 2);
    }

    #[test]
    fn should_keep_the_original_order_when_nothing_is_hidden() {
        let items = [3, 1, 2];

        let (shown, hidden) = take_worst(&items, CAPPED, |item| *item);

        assert_eq!(shown, vec![&3, &1, &2]);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn should_return_every_item_in_existing_order_with_full() {
        let items = [9, 1, 8, 2, 7, 3, 6];

        let (shown, hidden) = take_worst(&items, FULL, |item| *item);

        assert_eq!(shown, items.iter().collect::<Vec<_>>());
        assert_eq!(hidden, 0);
    }

    #[test]
    fn should_indent_the_more_line_like_the_list_items() {
        assert_eq!(
            more_line(3, 2).as_deref(),
            Some("  … 3 more — rerun with --full to see all")
        );
    }
}
