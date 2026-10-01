//! Elects a cluster's directory name through a never-mixed, never-numeric ladder.

use std::collections::BTreeMap;

use smol_str::SmolStr;

/// Per-cluster directory election: each key holds its accumulated
/// (production SLOC, file count) vote.
pub(in crate::analyze) type NameTally = BTreeMap<u32, BTreeMap<SmolStr, (u64, u32)>>;

/// Adds one file's vote for `key` — production SLOC weighs first, file count
/// second, so test-only files cannot outvote the production home directory.
pub(in crate::analyze) fn vote(
    tally: &mut NameTally,
    cluster: u32,
    key: SmolStr,
    production_sloc: u32,
) {
    let (sloc, count) = tally.entry(cluster).or_default().entry(key).or_default();
    *sloc = sloc.saturating_add(u64::from(production_sloc));
    *count = count.saturating_add(1);
}

/// Returns the heaviest-weighted key in `tally`, ties broken by the
/// lexicographically smallest key so container naming is deterministic across
/// runs.
pub(in crate::analyze) fn plurality<V: Ord>(tally: &BTreeMap<SmolStr, V>) -> SmolStr {
    tally
        .iter()
        .max_by(|left, right| left.1.cmp(right.1).then(right.0.cmp(left.0)))
        .map_or_else(|| SmolStr::new("workspace"), |(key, _)| key.clone())
}

/// True when a name is fit to serve as an elected identity: non-empty, not
/// all-digit at every `/`-segment (`2024`, `2024/2025`), and not tailed by a
/// `-<digits>` marker (`report-2`) — the shapes reserved for real directory
/// names and the arena's collision backstop, which a *suggested* container
/// name must never imitate.
fn fit_for_election(name: &str) -> bool {
    let numeric = name
        .split('/')
        .all(|segment| !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()));
    let suffixed = name
        .rsplit_once('-')
        .is_some_and(|(_, tail)| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()));
    !name.is_empty() && !numeric && !suffixed
}

/// Returns the plurality home when it holds a *strict* majority of the
/// cluster's weight and is fit to elect. Weight is production SLOC (as
/// `plurality` ranks), degrading to file count only for an all-test,
/// zero-SLOC cluster — a production home keeps its name despite companion
/// specs. Cross-multiplication keeps the test integer-exact, and demanding
/// `2 × win > total` sends a tied pair to the ladder's lower rungs instead
/// of crowning one side.
fn strict_majority(tally: &BTreeMap<SmolStr, (u64, u32)>) -> Option<SmolStr> {
    let winner = plurality(tally);
    let (win_sloc, win_count) = tally.get(&winner).copied()?;
    let total_sloc = tally
        .values()
        .fold(0_u64, |total, &(sloc, _)| total.saturating_add(sloc));
    let majority = if total_sloc > 0 {
        win_sloc.saturating_mul(2) > total_sloc
    } else {
        let total_count = tally.values().fold(0_u64, |total, &(_, count)| {
            total.saturating_add(u64::from(count))
        });
        u64::from(win_count).saturating_mul(2) > total_count
    };
    (majority && fit_for_election(&winner)).then_some(winner)
}

/// Returns the longest `/`-segment prefix shared by every key in `tally` —
/// empty when the keys already diverge at their first segment.
fn shared_prefix(tally: &BTreeMap<SmolStr, (u64, u32)>) -> SmolStr {
    let mut keys = tally.keys();
    let Some(first) = keys.next() else {
        return SmolStr::new("");
    };
    let mut prefix: Vec<&str> = first.split('/').collect();
    for key in keys {
        let shared = prefix
            .iter()
            .zip(key.split('/'))
            .take_while(|(held, segment)| **held == *segment)
            .count();
        prefix.truncate(shared);
    }
    SmolStr::new(prefix.join("/"))
}

/// Joins the cluster's two heaviest homes with `/` — ranked by production
/// SLOC then file count, the same vote order every other rung uses, ties by
/// key order — when at least two homes exist and the composite is fit to
/// elect. A balanced grab-bag with no shared prefix is honestly named after
/// both of its real origins, and a test-only spec dump outnumbering the
/// production homes in files can never lead the joined name.
///
/// Package-qualified homes share their leading segments; the second home is
/// relativized against the first before joining, so `ai/adapters` +
/// `ai/model` composes `ai/adapters/model`. A joined name is synthetic by
/// design — a proposed container not existing yet is the point — but it must
/// stay coherent: `ai/adapters/ai/model` re-embeds the shared root mid-path,
/// a nesting no human would ever write, and bakes in the very segment
/// repetition the display fold exists to prevent. This rung fires only when a
/// divergent third home has already emptied the all-member [`shared_prefix`]
/// consensus, so the pair's common ancestor alone would misclaim the cluster;
/// naming both heavy homes stays the more honest cover. When one home is the
/// other's ancestor, though, that ancestor covers both and elects alone.
fn join_top_two(tally: &BTreeMap<SmolStr, (u64, u32)>) -> Option<SmolStr> {
    let mut ranked: Vec<(&SmolStr, (u64, u32))> =
        tally.iter().map(|(key, &weight)| (key, weight)).collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    let [(first, _), (second, _), ..] = ranked.as_slice() else {
        return None;
    };
    let shared = first
        .split('/')
        .zip(second.split('/'))
        .take_while(|(left, right)| left == right)
        .count();
    let remainder = second.split('/').skip(shared).collect::<Vec<_>>().join("/");
    let joined = if remainder.is_empty() {
        // `second` is an ancestor of `first`: the ancestor covers both homes.
        (*second).clone()
    } else if shared == first.split('/').count() {
        // `first` is an ancestor of `second`: same cover, other direction.
        (*first).clone()
    } else {
        SmolStr::new(format!("{first}/{remainder}"))
    };
    fit_for_election(&joined).then_some(joined)
}

/// Returns the heaviest non-numeric path token across the cluster's home
/// keys — weight accumulated as (production SLOC, file count), ties by the
/// lexicographically smaller token — skipping tokens unfit to elect. Rescues
/// a name when whole keys are numeric (`2024/2025`) but a real word survives
/// inside them.
fn dominant_token(tally: &BTreeMap<SmolStr, (u64, u32)>) -> Option<SmolStr> {
    let mut tokens: BTreeMap<&str, (u64, u32)> = BTreeMap::new();
    for (key, &(sloc, count)) in tally {
        for segment in key.split('/') {
            if segment.is_empty() || segment.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let (token_sloc, token_count) = tokens.entry(segment).or_default();
            *token_sloc = token_sloc.saturating_add(sloc);
            *token_count = token_count.saturating_add(count);
        }
    }
    let mut ranked: Vec<(&str, (u64, u32))> = tokens.into_iter().collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    ranked
        .into_iter()
        .map(|(token, _)| token)
        .find(|token| fit_for_election(token))
        .map(SmolStr::new)
}

/// Elects a cluster's directory name through a deterministic ladder that is
/// structurally incapable of yielding a synthetic label or a bare number —
/// the first fit rung wins:
///
/// 1. the strict-majority home ([`strict_majority`]);
/// 2. the longest home prefix every member shares ([`shared_prefix`]);
/// 3. the two heaviest homes joined ([`join_top_two`]);
/// 4. the dominant non-numeric path token ([`dominant_token`]);
/// 5. the first home key — wrapped with the cluster's dot-encoded `anchor`
///    folder when the key alone is unfit (`2024 (2024.x)`), so even an
///    all-numeric grab-bag renders as a real, non-numeric identity.
pub(in crate::analyze) fn elect(
    tally: &BTreeMap<SmolStr, (u64, u32)>,
    anchor: &SmolStr,
) -> SmolStr {
    if let Some(winner) = strict_majority(tally) {
        return winner;
    }
    let prefix = shared_prefix(tally);
    if !prefix.is_empty() && fit_for_election(&prefix) {
        return prefix;
    }
    if let Some(joined) = join_top_two(tally) {
        return joined;
    }
    if let Some(token) = dominant_token(tally) {
        return token;
    }
    let first = tally
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| SmolStr::new("workspace"));
    if fit_for_election(&first) {
        return first;
    }
    SmolStr::new(format!("{first} ({})", anchor.replace('/', ".")))
}
