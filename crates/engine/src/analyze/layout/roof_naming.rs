//! Names the places a roof rebuild proposes after the files they hold.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::condense::Condensation;

use crate::analyze::layout::{basename_stem, union_root};
use crate::analyze::relocation::FileInfo;
use crate::narrate::tokenize;

/// Fraction of `member_files` whose basename shares a token with the last `/`
/// segment of `container_name` — the engine-side twin of the eval harness's
/// alignment metric, so the synthesis trigger and the verdict measure the same
/// coherence and can never disagree about what "misnamed" means.
pub(super) fn roof_coherence(
    container_name: &str,
    residual: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
) -> f64 {
    let last = container_name.rsplit('/').next().unwrap_or(container_name);
    let container_tokens = tokenize(last);
    let member_files: Vec<String> = residual
        .iter()
        .flat_map(|&scc| scc_file_names(condensation, files, scc))
        .collect();
    let total = member_files.len();
    if total == 0 {
        return 1.0;
    }
    let aligned = member_files
        .iter()
        .filter(|file| {
            let base = file.rsplit('/').next().unwrap_or(file.as_str());
            let stem = base.split('.').next().unwrap_or(base);
            tokenize(stem)
                .iter()
                .any(|token| container_tokens.contains(token))
        })
        .count();
    // reason: member counts are corpus-sized; the f64 mantissa loses nothing
    #[allow(clippy::cast_precision_loss)]
    let sharing = aligned as f64 / total as f64;
    sharing
}

/// Collects the file paths inside one SCC, ascending.
fn scc_file_names(condensation: &Condensation, files: &[FileInfo], scc: u32) -> Vec<String> {
    condensation
        .members
        .get(scc as usize)
        .into_iter()
        .flatten()
        .filter_map(|member| files.get(member.0 as usize))
        .map(|file| file.name.to_string())
        .collect()
}

/// Basename tokens of one file path, per the CONTRACT tokenization scope: the
/// basename minus extension only — the directory prefix never counts toward a
/// container's claim on a file.
fn basename_tokens(name: &str) -> BTreeSet<String> {
    let basename = name.rsplit('/').next().unwrap_or(name);
    let stem = basename.split('.').next().unwrap_or(basename);
    tokenize(stem)
}

/// Groups `sccs` into token-connected components: two SCCs join when any pair
/// of their files shares a basename token. This is naming evidence only — no
/// edge is priced or fabricated (D-46 holds: absence of priced edges is read
/// as separation evidence, and shared words group the separated). Deterministic:
/// ascending SCC pairs, union roots the smaller id, components emitted by
/// smallest member ascending, each sorted ascending.
pub(in crate::analyze) fn token_groups(
    sccs: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
) -> Vec<Vec<u32>> {
    let tokens_of: BTreeMap<u32, BTreeSet<String>> = sccs
        .iter()
        .map(|&scc| {
            let tokens: BTreeSet<String> = scc_file_names(condensation, files, scc)
                .iter()
                .flat_map(|name| basename_tokens(name))
                .collect();
            (scc, tokens)
        })
        .collect();
    let mut parent: BTreeMap<u32, u32> = sccs.iter().map(|&scc| (scc, scc)).collect();
    for (index, &left) in sccs.iter().enumerate() {
        for &right in sccs.iter().skip(index + 1) {
            let joined = tokens_of.get(&left).is_some_and(|left_tokens| {
                tokens_of
                    .get(&right)
                    .is_some_and(|right_tokens| !left_tokens.is_disjoint(right_tokens))
            });
            if joined {
                let (a, b) = (union_root(&parent, left), union_root(&parent, right));
                if a != b {
                    let (keep, move_) = if a <= b { (a, b) } else { (b, a) };
                    parent.insert(move_, keep);
                }
            }
        }
    }
    let mut components: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &scc in sccs {
        components
            .entry(union_root(&parent, scc))
            .or_default()
            .push(scc);
    }
    let mut groups: Vec<Vec<u32>> = components.into_values().collect();
    for group in &mut groups {
        group.sort_unstable();
    }
    groups
}

/// Names one proposed place after its members: a token shared by EVERY file in
/// the group wins (`helpers/utils`); otherwise the two heaviest distinct
/// basename stems join (`helpers/charge-refund`) — the [`join_top_two`]
/// honesty, so a name covering two stems survives later absorption of a third
/// differently-named file without falling under half-aligned. The separator
/// between base and suffix is a slash — the label path-extends its base folder
/// at render time — while a joined stem pair stays dash-joined inside the last
/// segment. Collisions fall back to the numeric form, mirroring
/// [`relieve_over_capacity`]. Returns the label inserted into `used`.
pub(in crate::analyze) fn rebuild_label(
    base_name: &str,
    group: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
    used: &mut BTreeSet<SmolStr>,
    ordinal: u64,
) -> SmolStr {
    // per-file token sets, for the everyone-shares-it intersection.
    let per_file: Vec<BTreeSet<String>> = group
        .iter()
        .flat_map(|&scc| scc_file_names(condensation, files, scc))
        .map(|name| basename_tokens(&name))
        .collect();
    let common: Option<String> = per_file
        .first()
        .map(|first| {
            per_file.iter().skip(1).fold(first.clone(), |held, set| {
                held.intersection(set).cloned().collect()
            })
        })
        // an all-digit token would mint a label indistinguishable from the
        // numeric fallback (`helpers/2024` vs `helpers/3`) and would never
        // align under the contract tokenizer, which drops digit tokens — skip
        // it and let the stem path or the fallback name the place.
        .and_then(|tokens| {
            tokens
                .into_iter()
                .find(|token| token.chars().any(char::is_alphabetic))
        });
    let candidate = if let Some(token) = common {
        format!("{base_name}/{token}")
    } else {
        // heaviest distinct stems by production SLOC then stem order.
        #[derive(Default)]
        struct Tally {
            sloc: u64,
        }
        let mut tally: BTreeMap<String, Tally> = BTreeMap::new();
        for &scc in group {
            let Some(members) = condensation.members.get(scc as usize) else {
                continue;
            };
            for member in members {
                let Some(file) = files.get(member.0 as usize) else {
                    continue;
                };
                let Some(stem) = basename_stem(&file.name) else {
                    continue;
                };
                tally.entry(stem).or_default().sloc += u64::from(file.production_sloc);
            }
        }
        let mut ranked: Vec<(String, u64)> =
            tally.into_iter().map(|(stem, t)| (stem, t.sloc)).collect();
        ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let stems: Vec<String> = ranked.into_iter().map(|(stem, _)| stem).take(2).collect();
        if stems.is_empty() {
            // no alphabetic stem anywhere in the group: nothing honest to
            // name it after — the numeric form is the only fit left.
            format!("{base_name}/{ordinal}")
        } else {
            format!("{base_name}/{}", stems.join("-"))
        }
    };
    let label = if used.contains(candidate.as_str()) {
        format!("{base_name}/{ordinal}")
    } else {
        candidate
    };
    used.insert(SmolStr::from(label.clone()));
    SmolStr::from(label)
}
