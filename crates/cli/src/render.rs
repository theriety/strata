//! TTY-aware rendering of an [`AnalyzeResult`] into human-readable text.
//!
//! Rendering is a pure function of the result and an output sink: the same result
//! always renders the same bytes, and `--format json` bypasses this module
//! entirely (the JSON path serializes the library value verbatim, the AD-5
//! parity guarantee). The summary face is the printed report the design rounds
//! approved: a deterministic, monospace-safe plain-text report — no color, no
//! links, no terminal escapes, every line inside the 100-column grid — so
//! piping a rendered view into a file is stable and diffable. Its vocabulary is
//! normative: suggested actions only, a grain label on every suggestion,
//! three-column change tables (`leaf | before place | after place`) with no
//! marker column, backticks around every file, folder, and symbol name,
//! explicit truncation markers, and candidate-count honesty.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, Write};

use strata_engine::{
    AnalyzeResult, Candidate, ContainerNode, CurrentStanding, FileMove, Level, ModeResult, Move,
    MoveKind, MoveReason, ScoreBreakdown, Severity, Violation, ViolationKind,
};

/// The output format the `analyze` command renders in.
///
/// `Summary` is the human-readable printed-report face; `Json` is the serialized
/// library result, the AD-5 parity face. The `violations` command carries its
/// own table face so the two surfaces never share a variant they do not both
/// use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The default human-readable printed report.
    Summary,
    /// The serialized `AnalyzeResult` JSON.
    Json,
}

impl Format {
    /// Resolves the effective format from an optional explicit choice and whether
    /// the sink is a terminal.
    ///
    /// With no explicit choice the format is TTY-aware: a terminal gets the
    /// human-readable `default_human` face, a pipe gets [`Format::Json`]. An
    /// explicit choice always wins.
    #[must_use]
    pub fn resolve(explicit: Option<Format>, is_terminal: bool, default_human: Format) -> Format {
        match explicit {
            Some(format) => format,
            None if is_terminal => default_human,
            None => Format::Json,
        }
    }
}

/// Renders the analysis `result` in `format` to `out`.
///
/// The JSON face is the parity-pinned serialization of the library result; the
/// summary face is the approved printed report — header and reading rules, the
/// run's candidates, the itemized suggestions of the recommended candidate, the
/// blast radius, the findings, the score delta, and the recommendation.
/// `project` names the analyzed repository and banners the report; the caller
/// derives it from the analyzed root.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] if writing to `out` fails.
pub fn render(
    result: &AnalyzeResult,
    format: Format,
    project: &str,
    out: &mut impl Write,
) -> io::Result<()> {
    match format {
        Format::Json => write_json(result, out),
        Format::Summary => write_report(result, project, out),
    }
}

/// Serializes `result` as pretty JSON to `out`.
///
/// # Errors
///
/// Returns an [`io::Error`] if serialization or writing fails.
pub fn write_json(result: &AnalyzeResult, out: &mut impl Write) -> io::Result<()> {
    let json = serde_json::to_vec(result).map_err(io::Error::other)?;
    out.write_all(&json)?;
    out.write_all(b"\n")
}

/// Writes the printed report of `result` to `out`.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
fn write_report(result: &AnalyzeResult, project: &str, out: &mut impl Write) -> io::Result<()> {
    for line in report_lines(result, project) {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Builds the complete printed report as one line per element, right-trimmed.
///
/// Pure so the unit suite can pin vocabulary, widths, and arithmetic without an
/// output sink; [`write_report`] only adds newlines.
fn report_lines(result: &AnalyzeResult, project: &str) -> Vec<String> {
    let mut lines = Vec::new();
    lines.extend(header_lines(result, project));
    lines.push(String::new());
    lines.extend(section_bars("§1  The candidates this run produced"));
    lines.extend(candidates_section(result));
    lines.push(String::new());
    lines.extend(section_bars(&format!(
        "§2  What would change — {}",
        featured_title(result)
    )));
    lines.extend(changes_section(result));
    lines.push(String::new());
    lines.extend(section_bars("§3  Blast radius — changed containers only"));
    lines.extend(blast_radius_section(result));
    lines.push(String::new());
    lines.extend(section_bars("§4  Findings this run raised"));
    lines.extend(findings_section(result));
    lines.push(String::new());
    lines.extend(section_bars(&format!(
        "§5  Score delta — {}",
        delta_title(result)
    )));
    lines.extend(score_delta_section(result));
    lines.push(String::new());
    lines.extend(section_bars("§6  Recommendation"));
    lines.extend(recommendation_section(result));
    lines.push(String::new());
    lines.push(BAR.to_owned());
    lines.push(format!(" end of report · {project}"));
    lines.push(BAR.to_owned());
    lines
        .into_iter()
        .map(|line| line.trim_end().to_owned())
        .collect()
}

/// The heavy section separator.
const BAR: &str =
    "================================================================================";

/// The light section separator.
const SUB: &str =
    "--------------------------------------------------------------------------------";

/// Frames `title` between heavy bars.
fn section_bars(title: &str) -> Vec<String> {
    vec![BAR.to_owned(), format!(" {title}"), BAR.to_owned()]
}

/// Formats a score figure with four decimals.
fn f4(value: f64) -> String {
    format!("{value:.4}")
}

/// Formats a signed figure with four decimals and an explicit sign.
fn sf(value: f64) -> String {
    if value >= 0.0 {
        format!("+{value:.4}")
    } else {
        format!("-{:.4}", value.abs())
    }
}

/// Rounds to four decimals the way the delta column prints.
fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Formats a pull weight: at least one decimal, trailing zeros trimmed.
fn weight(value: f64) -> String {
    let mut text = format!("{value:.4}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.push('0');
    }
    text
}

/// Quotes a file, folder, or symbol name in backticks — every name token in
/// prose, titles, captions, and table cells carries them.
fn nq(name: &str) -> String {
    format!("`{name}`")
}

/// Wraps `text` on spaces to a 99-column budget.
///
/// `first_pad` indents the first line, `cont_pad` every continuation; both are
/// counted against the budget, mirroring the approved artifact's geometry.
fn wrap(text: &str, first_pad: usize, cont_pad: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut indent = " ".repeat(first_pad);
    for word in text.split_whitespace() {
        let grown = cur.chars().count() + usize::from(!cur.is_empty()) + word.chars().count();
        if !cur.is_empty() && indent.chars().count() + grown > 99 {
            lines.push(format!("{indent}{cur}"));
            word.clone_into(&mut cur);
            indent = " ".repeat(cont_pad);
        } else {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(format!("{indent}{cur}"));
    }
    lines
}

/// Renders a labeled prose block: `label` prefixes the first line, continuation
/// lines indent to `cont_pad`; `strip` is the wrapper padding the label replaces.
fn labeled_block(label: &str, strip: usize, cont_pad: usize, text: &str) -> Vec<String> {
    let mut lines = wrap(text, strip, cont_pad);
    if let Some(first) = lines.first_mut() {
        let body: String = first.chars().skip(strip).collect();
        *first = format!("{label}{body}");
    }
    lines
}

/// Shortens `name` to `budget` display characters with a leading ellipsis;
/// shorter names pass through untouched.
fn tail(name: &str, budget: usize) -> String {
    let count = name.chars().count();
    if count <= budget {
        return name.to_owned();
    }
    if budget <= 1 {
        return "…".repeat(budget);
    }
    let kept: String = name.chars().skip(count - (budget - 1)).collect();
    format!("…{kept}")
}

/// Derives each moved file's displayed leaf name.
///
/// The basename is the default; when basenames collide inside one table the
/// path suffix grows from the end until every colliding member is unique, and
/// if duplicates survive that, every leaf falls back to its full relative path.
fn leaf_names(files: &[FileMove]) -> Vec<String> {
    let base = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
    let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();

    // A basename shared with a peer needs suffix growth; unique ones pass through.
    let grown: Vec<String> = paths
        .iter()
        .map(|path| {
            let name = base(path);
            let shared = paths.iter().any(|other| other != path && base(other) == name);
            if !shared {
                return name;
            }
            let total_segments = path.split('/').count();
            let mut depth = 1_usize;
            loop {
                let suffix = |p: &str| {
                    let segments: Vec<&str> = p.split('/').collect();
                    segments
                        .iter()
                        .rev()
                        .take(depth)
                        .rev()
                        .copied()
                        .collect::<Vec<_>>()
                        .join("/")
                };
                let candidate = suffix(path);
                let clash = paths
                    .iter()
                    .any(|other| other != path && base(other) == name && suffix(other) == candidate);
                depth += 1;
                if total_segments < depth || !clash {
                    break candidate;
                }
            }
        })
        .collect();

    // If duplicates survive even at full-path length (identical paths), every
    // leaf falls back to its full relative path.
    let collapsed = grown
        .iter()
        .any(|name| grown.iter().filter(|other| *other == name).count() > 1);
    if collapsed {
        paths
    } else {
        grown
    }
}

/// Bounds a composed suggestion title so `prefix` plus the title stays inside
/// the grid: the widest quoted name shortens one character at a time with a
/// leading-ellipsis tail, and the flag reports that anything was cut.
fn fit_title(prefix: &str, title: &str, limit: usize) -> (String, bool) {
    let mut current = title.to_owned();
    let mut shortened = false;
    let prefix_width = prefix.chars().count();
    while prefix_width + current.chars().count() > limit {
        if let Some(shrunk) = shrink_longest_quoted(&current) {
            current = shrunk;
            shortened = true;
        } else {
            let budget = limit.saturating_sub(prefix_width).max(1);
            return (tail(&current, budget), true);
        }
    }
    (current, shortened)
}

/// Shortens the widest backtick-quoted name in `title` by one character with a
/// leading-ellipsis tail; `None` when no quoted name can shrink further.
fn shrink_longest_quoted(title: &str) -> Option<String> {
    let parts: Vec<&str> = title.split('`').collect();
    let (widest_position, widest) = parts
        .iter()
        .enumerate()
        .filter(|(position, _)| position % 2 == 1)
        .max_by_key(|(_, part)| part.chars().count())?;
    let widest = *widest;
    let shrunk = tail(widest, widest.chars().count().saturating_sub(1));
    if shrunk == widest {
        return None;
    }
    let rebuilt = parts
        .iter()
        .enumerate()
        .map(|(position, part)| {
            if position == widest_position {
                nq(&shrunk)
            } else {
                (*part).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("`");
    Some(rebuilt)
}

/// Builds a suggestion title: action verb, the named leaves (spelled out for
/// one or two, counted beyond), the preposition, and the quoted destination.
fn suggestion_title(kind: MoveKind, names: &[String], to: &str) -> String {
    let dest = nq(if to.is_empty() { "(root)" } else { to });
    match kind {
        MoveKind::Move => {
            let leaf = names.first().map(|name| nq(name)).unwrap_or_default();
            format!("Move {leaf} into {dest}")
        }
        MoveKind::Merge => counted_title("Merge", names, "into", &dest),
        MoveKind::Split => counted_title("Split", names, "out into", &dest),
    }
}

/// The merge/split title body: spelled-out leaves for one or two, a file count
/// beyond.
fn counted_title(verb: &str, names: &[String], prep: &str, dest: &str) -> String {
    if names.len() <= 2 {
        let shown = names
            .iter()
            .map(|name| nq(name))
            .collect::<Vec<_>>()
            .join(" and ");
        format!("{verb} {shown} {prep} {dest}")
    } else {
        format!("{verb} {} files {prep} {dest}", names.len())
    }
}

/// Builds a suggestion caption from the move's dominant reason.
///
/// Every caption opens with the `WHY` anchor and quotes the name tokens it
/// cites; the clustering fallback speaks of import-heavy regrouping, and the
/// naming-cohesion reason (which the approved artifact never had occasion to
/// print) borrows the term-gloss language for names fitting contents.
fn caption_for(reason: &MoveReason) -> String {
    match reason {
        MoveReason::PulledBy {
            partner,
            weight: pull,
        } => {
            format!(
                "WHY pulled toward {} — weight {}.",
                nq(partner),
                weight(*pull)
            )
        }
        MoveReason::RelievesOverCap {
            container,
            count,
            cap,
        } => format!(
            "WHY relieves {}, which holds {count} against a cap of {cap}.",
            nq(container)
        ),
        MoveReason::Follows { subject } => {
            format!(
                "WHY follows {}, which this plan places nearby.",
                nq(subject)
            )
        }
        MoveReason::NamingCohesion { .. } => {
            "WHY clusters files whose names fit their destination.".to_owned()
        }
        MoveReason::Clustering => {
            "WHY clusters files that already import each other heavily.".to_owned()
        }
    }
}

/// The dataset line, census, and reading rules — the approved header geometry.
fn header_lines(result: &AnalyzeResult, project: &str) -> Vec<String> {
    let mut lines = vec![BAR.to_owned(), format!(" dataset   : {project}")];
    let languages = result
        .summary
        .files_by_language
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let languages_suffix = if languages.is_empty() {
        String::new()
    } else {
        format!(" ({languages})")
    };
    let census = format!(
        " census    : {} symbols · {} edges · {} files{languages_suffix}",
        result.summary.symbols, result.summary.edges, result.summary.files
    );
    lines.push(census);
    lines.push(SUB.to_owned());
    lines.push(" reading rules".to_owned());
    lines.push(SUB.to_owned());
    lines.extend(reading_rules(result));
    lines
}

/// The reading-rules block: the owner-approved copy verbatim, with the run's
/// headline figures riding their fixed positions.
fn reading_rules(result: &AnalyzeResult) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some((_, candidate)) = featured_candidate(result) {
        lines.push(format!(
            " scores    : four decimals, lower is better            {} → {}",
            f4(result.current.score),
            f4(candidate.score)
        ));
        lines.push(
            " gain      : improvement over that mode's own starting point, measured in".to_owned(),
        );
        lines.push(format!(
            "             the same units as the score              {}",
            sf(candidate.improvement)
        ));
    } else {
        lines.push(" scores    : four decimals, lower is better".to_owned());
        lines.push(
            " gain      : improvement over that mode's own starting point, measured in".to_owned(),
        );
        lines.push("             the same units as the score".to_owned());
    }
    lines.extend([
        " modes     : anchored keeps today's good placements as fixed points while".to_owned(),
        "             improving the rest; greenfield ignores current paths entirely".to_owned(),
        "             and rebuilds from imports alone".to_owned(),
        " leaf      : one relocatable unit — a source file plus the symbols it".to_owned(),
        "             carries; moving it moves both. A table's leaf is whichever unit".to_owned(),
        "             that suggestion acts on: usually the whole file, sometimes one".to_owned(),
        "             symbol or one call.".to_owned(),
        " weight    : strength of the import relationship the move satisfies —".to_owned(),
        "             roughly how many import connections it honors; a bigger number".to_owned(),
        "             is more valuable to honor".to_owned(),
        " comparability: candidate scores are comparable only within one mode of one".to_owned(),
        "             project — greenfield drops the path and anchor terms and".to_owned(),
        "             re-baselines. When two candidates share one structure, decide".to_owned(),
        "             between them by intent, never by raw score.".to_owned(),
        " rounding  : columns round independently to four decimals; totals compute unrounded"
            .to_owned(),
        " truncation: always explicit (… +N more); nothing is cut silently".to_owned(),
        " grain     : every suggestion states what it relocates — whole files, or".to_owned(),
        "             symbols within one file; a suggestion that changes no placement".to_owned(),
        "             says so instead of shipping a table. Prose under each table is".to_owned(),
        "             its caption and carries the why.".to_owned(),
        " names     : file, folder and symbol names always appear in `backticks`".to_owned(),
        " future    : anything today's strata cannot yet emit keeps the literal label".to_owned(),
        "             \"future capability — not in current output\" (none apply here) —".to_owned(),
        "             the label marks capability, never whether the advice is real".to_owned(),
        " grid      : 100 columns · monospace · no color · no links · scroll only".to_owned(),
    ]);
    lines
}

/// The modes present in `result`, in their fixed report order.
fn present_modes(result: &AnalyzeResult) -> Vec<(&'static str, &ModeResult)> {
    let mut modes = Vec::new();
    if let Some(mode) = &result.modes.anchored {
        modes.push(("anchored", mode));
    }
    if let Some(mode) = &result.modes.greenfield {
        modes.push(("greenfield", mode));
    }
    modes
}

/// The recommended candidate: the best of anchored when present, else of
/// greenfield. Candidates arrive best-score-first, so the head is the pick.
fn featured_candidate(result: &AnalyzeResult) -> Option<(&'static str, &Candidate)> {
    for (name, mode) in present_modes(result) {
        if let Some(candidate) = mode.candidates.first() {
            return Some((name, candidate));
        }
    }
    None
}

/// The `§2` title tail naming the itemized candidate.
fn featured_title(result: &AnalyzeResult) -> String {
    match featured_candidate(result) {
        Some((name, candidate)) => format!("candidate {} ({})", candidate.index, name),
        None => "no candidate this run produced".to_owned(),
    }
}

/// The `§5` title tail naming the compared candidate.
fn delta_title(result: &AnalyzeResult) -> String {
    match featured_candidate(result) {
        Some((_, candidate)) => format!("current → candidate {}", candidate.index),
        None => "no candidate this run produced".to_owned(),
    }
}

/// The `§1` candidate list: the printed claim line, every candidate named and
/// scored, the comparability note beside the list, and the run's notices.
fn candidates_section(result: &AnalyzeResult) -> Vec<String> {
    let modes = present_modes(result);
    if modes.is_empty() {
        return vec![" candidate count : 0".to_owned()];
    }
    let total: usize = modes.iter().map(|(_, mode)| mode.candidates.len()).sum();

    let clauses = modes
        .iter()
        .map(|(name, mode)| format!("{name} mode returned {}", mode.candidates.len()))
        .collect::<Vec<_>>()
        .join(" · ");
    let mut lines = vec![format!(" candidate count : {total}  ({clauses})")];
    lines.push(String::new());

    let tags: Vec<(String, &Candidate, &'static str)> = modes
        .iter()
        .flat_map(|(name, mode)| {
            mode.candidates
                .iter()
                .map(move |candidate| (format!("{name}/{}", candidate.index), candidate, *name))
        })
        .collect();
    let tag_width = tags
        .iter()
        .map(|(tag, _, _)| tag.chars().count())
        .max()
        .unwrap_or(1);

    let featured = featured_candidate(result);
    for (row, (tag, candidate, mode_name)) in tags.iter().enumerate() {
        let annotation = match featured {
            Some((featured_name, picked))
                if *mode_name == featured_name && candidate.index == picked.index =>
            {
                "best score"
            }
            _ if is_head_of_mode(modes.as_slice(), mode_name, candidate.index)
                && featured.is_some_and(|(featured_name, _)| *mode_name != featured_name) =>
            {
                match featured {
                    Some((_, picked)) if candidate.tree == picked.tree => {
                        "same shape, own baseline"
                    }
                    _ => "different shape, own baseline",
                }
            }
            _ => "",
        };
        let suffix = if annotation.is_empty() {
            String::new()
        } else {
            format!("   {annotation}")
        };
        lines.push(format!(
            " candidate {}   {tag:<tag_width$}   score  {}   gain {}{suffix}",
            row + 1,
            f4(candidate.score),
            sf(candidate.improvement),
        ));
    }

    lines.push(String::new());
    if let [(anchored_name, anchored), (greenfield_name, greenfield)] = modes.as_slice() {
        let text = format!(
            "these two gains have different starting points — {anchored_name} baselines the \
             current tree at {}; {greenfield_name} re-prices paths and drops anchor credit, \
             baselining the same tree at {}. Each gain is measured against its own mode's \
             baseline — never compare across modes.",
            f4(anchored.current_score),
            f4(greenfield.current_score),
        );
        lines.extend(labeled_block(" comparability: ", 17, 17, &text));
    } else {
        lines.extend(labeled_block(
            " comparability: ",
            17,
            17,
            "candidate scores are comparable only within one mode of one project.",
        ));
    }

    if modes.iter().any(|(_, mode)| mode.solution_space_converged) {
        lines.push(String::new());
        lines.extend(labeled_block(
            " note  ",
            7,
            7,
            "fewer than the requested candidates survived; the solution space converged.",
        ));
    }
    if let Some(note) = infeasibility_note(result) {
        lines.push(String::new());
        lines.extend(labeled_block(" note  ", 7, 7, &note));
    }
    lines
}

/// Whether `index` names the head candidate of the named mode.
fn is_head_of_mode(modes: &[(&'static str, &ModeResult)], name: &str, index: u32) -> bool {
    modes
        .iter()
        .find(|(mode_name, _)| *mode_name == name)
        .and_then(|(_, mode)| mode.candidates.first())
        .is_some_and(|head| head.index == index)
}

/// Builds the infeasibility note: which modes mark today's layout infeasible,
/// how many capacity findings it breaks, and — when the best candidate reports
/// a remainder — what survives the proposal, stated directly. `None` when no
/// mode is infeasible.
fn infeasibility_note(result: &AnalyzeResult) -> Option<String> {
    let infeasible: Vec<&str> = present_modes(result)
        .into_iter()
        .filter(|(_, mode)| mode.current_standing == CurrentStanding::Infeasible)
        .map(|(name, _)| name)
        .collect();
    let first = *infeasible.first()?;
    let subject = match infeasible.len() {
        1 => format!("the {first} mode records"),
        _ => "both modes record".to_owned(),
    };
    // The engine owns the count: hard capacity breaches only (borderline
    // observations stay listed in §4 but never count as breaks).
    let findings = result.current.capacity_breaks;
    let plural = if findings == 1 { "" } else { "s" };
    let mut note = format!(
        "{subject} today's layout as infeasible — it breaks {findings} capacity \
         finding{plural} (§4), so keeping everything as-is is not a result this run offers."
    );
    if let Some((_, candidate)) = featured_candidate(result)
        && let Some(remainder) = candidate.capacity_remainder
    {
        if remainder.remaining == 0 {
            let _ = write!(note, " Candidate {} clears every one of them.", candidate.index);
        } else {
            let _ = write!(
                note,
                " Candidate {} still leaves {} of them above their caps",
                candidate.index, remainder.remaining
            );
            if remainder.file_level > 0 {
                let _ = write!(
                    note,
                    " ({} at file level, where only conditional splits can help)",
                    remainder.file_level
                );
            }
            note.push('.');
        }
    }
    Some(note)
}

/// The `§2` itemization: the preamble, one block per narrated change, and the
/// unchanged-files honesty line.
fn changes_section(result: &AnalyzeResult) -> Vec<String> {
    let Some((featured_name, featured)) = featured_candidate(result) else {
        return vec!["no candidate this run produced — nothing to change.".to_owned()];
    };
    let moved: usize = featured
        .delta_narration
        .iter()
        .map(|entry| entry.files.len())
        .sum();

    let mut lines = Vec::new();
    if moved == 0 {
        lines.push(format!(
            "no change suggested — candidate {} matches today's layout, file for file.",
            featured.index
        ));
        return lines;
    }

    let other_head = present_modes(result)
        .into_iter()
        .find(|(name, _)| *name != featured_name)
        .and_then(|(_, mode)| mode.candidates.first());
    let lead = match other_head {
        Some(other) if other.delta_narration != featured.delta_narration => format!(
            "{}'s plan ({} changes) differs from {featured_name}'s in detail — only candidate \
             {} is itemized below.",
            other_mode_name(result, featured_name),
            other.delta_narration.len(),
            featured.index
        ),
        Some(_) => format!(
            "{}'s plan matches this one change for change. Every suggestion relocates whole \
             files ({moved} in all).",
            other_mode_name(result, featured_name)
        ),
        None => String::new(),
    };
    if lead.is_empty() {
        lines.push(format!(
            "Every suggestion relocates whole files ({moved} in all):"
        ));
    } else {
        lines.extend(wrap(
            &format!("{lead} Every suggestion relocates whole files ({moved} in all)."),
            1,
            1,
        ));
    }
    lines.push(String::new());

    for (step, entry) in featured.delta_narration.iter().enumerate() {
        lines.extend(suggestion_block(step + 1, entry));
    }

    let unchanged = result
        .summary
        .files
        .saturating_sub(u32::try_from(moved).unwrap_or(u32::MAX));
    if unchanged > 0 {
        let omission =
            format!("The other {unchanged} files are absent because nothing about them changes.");
        lines.extend(wrap(&omission, 1, 1));
    }
    lines
}

/// The display name of the mode other than `featured_name`.
fn other_mode_name<'a>(result: &'a AnalyzeResult, featured_name: &str) -> &'a str {
    present_modes(result)
        .into_iter()
        .find(|(name, _)| *name != featured_name)
        .map_or("greenfield", |(name, _)| name)
}

/// Renders one numbered suggestion: title bar, grain label, the three-column
/// change table (with explicit shortening honesty), and the captioned why.
fn suggestion_block(number: usize, entry: &Move) -> Vec<String> {
    let names = leaf_names(&entry.files);
    let title_prefix = format!(" {number} · ");
    let (title, title_shortened) = fit_title(
        &title_prefix,
        &suggestion_title(entry.kind, &names, &entry.to),
        100,
    );
    let mut lines = vec![
        format!(" {SUB}"),
        format!("{title_prefix}{title}"),
        SUB.to_owned(),
        " grain : whole files".to_owned(),
        String::new(),
    ];
    let (table, shortened) = change_table(&entry.files, &names, &entry.to);
    lines.extend(table);
    if shortened || title_shortened {
        lines.push(
            "          a leading … marks a shortened place name; the run's output carries the \
             full paths."
                .to_owned(),
        );
    }
    lines.push(String::new());
    lines.extend(labeled_block(
        " caption  ",
        11,
        11,
        &caption_for(&entry.reason),
    ));
    lines.push(String::new());
    lines
}

/// Builds the `leaf | before place | after place` table for one suggestion.
///
/// Columns size to their widest cell (quoted names included) with the header
/// words as floors; when any row would cross the grid the place columns shrink
/// symmetrically and over-long names take a leading-ellipsis tail, flipping the
/// returned honesty flag.
fn change_table(files: &[FileMove], names: &[String], to: &str) -> (Vec<String>, bool) {
    let destination = if to.is_empty() { "(root)" } else { to };
    let leaf_width = names
        .iter()
        .map(|name| name.chars().count() + 2)
        .max()
        .unwrap_or(4)
        .max(4);
    let before_full = files
        .iter()
        .map(|file| file.from.chars().count() + 2)
        .max()
        .unwrap_or(12)
        .max(12);
    let after_full = (destination.chars().count() + 2).max(11);

    let build = |before_width: usize, after_width: usize| -> (Vec<String>, bool) {
        let before_budget = before_width.saturating_sub(2);
        let after_budget = after_width.saturating_sub(2);
        let before_cells: Vec<String> = files
            .iter()
            .map(|file| nq(&tail(&file.from, before_budget)))
            .collect();
        let shortened_before = files
            .iter()
            .any(|file| tail(&file.from, before_budget) != file.from);
        let shown_after = tail(destination, after_budget);
        let shortened = shortened_before || shown_after != destination;
        let after_cell = nq(&shown_after);
        let mut rows = Vec::new();
        for (name, before_cell) in names.iter().zip(before_cells.iter()) {
            rows.push(format!(
                " {:<leaf_width$} | {:<before_width$} | {:<after_width$}",
                nq(name),
                before_cell,
                after_cell,
            ));
        }
        rows.insert(
            0,
            format!(
                " {:<leaf_width$} | {:<before_width$} | {:<after_width$}",
                "leaf", "before place", "after place",
            ),
        );
        (rows, shortened)
    };

    let (rows, shortened) = build(before_full, after_full);
    if rows.iter().all(|row| row.chars().count() <= 100) {
        return (rows, shortened);
    }
    let budget = 93_usize.saturating_sub(leaf_width);
    let half = budget / 2;
    build(
        before_full.min(half + (budget % 2)).max(12),
        after_full.min(half).max(11),
    )
}

/// The `§3` changed-containers tally: per-container deltas, the twelve largest,
/// the honest remainder marker, and the balance note.
fn blast_radius_section(result: &AnalyzeResult) -> Vec<String> {
    let Some((_, featured)) = featured_candidate(result) else {
        return vec!["no candidate this run produced — nothing relocates.".to_owned()];
    };
    let moved: usize = featured
        .delta_narration
        .iter()
        .map(|entry| entry.files.len())
        .sum();
    let rows = blast_rows(&featured.delta_narration);
    if rows.is_empty() || moved == 0 {
        return vec!["nothing relocates — every container keeps its files.".to_owned()];
    }

    let shown = rows.len().min(12);
    let mut lines = wrap(
        &format!(
            "{} containers change as {moved} files relocate; the {shown} largest:",
            rows.len()
        ),
        1,
        1,
    );
    lines.push(String::new());

    let width = rows
        .iter()
        .take(shown)
        .map(|(container, _)| container.chars().count() + 2)
        .max()
        .unwrap_or(2);
    for (container, delta) in rows.iter().take(shown) {
        lines.push(format!(" {:<width$}  {delta:+}f", nq(container)));
    }
    let rest: Vec<(String, i64)> = rows.iter().skip(shown).cloned().collect();
    if !rest.is_empty() {
        let low = rest.iter().map(|(_, delta)| delta.abs()).min().unwrap_or(0);
        let high = rest.iter().map(|(_, delta)| delta.abs()).max().unwrap_or(0);
        let range = if low == high {
            format!("±{low}f")
        } else {
            format!("{low}–{high}f")
        };
        lines.push(format!(
            " … +{} more containers, each changing by {range}",
            rest.len()
        ));
    }
    lines.push(String::new());
    lines.extend(wrap(
        "the tally balances: every file that leaves a container arrives in one, so the changes \
         sum to zero.",
        1,
        1,
    ));
    lines
}

/// Computes per-container member deltas for a plan: every relocation subtracts
/// one at its source folder and adds one per relocated file at its
/// destination, so a merge group lands with its full weight. Sorted by largest
/// absolute change, ties in container order.
fn blast_rows(moves: &[Move]) -> Vec<(String, i64)> {
    let mut deltas: BTreeMap<String, i64> = BTreeMap::new();
    for entry in moves {
        for file in &entry.files {
            *deltas.entry(file.from.clone()).or_default() -= 1;
            *deltas.entry(entry.to.clone()).or_default() += 1;
        }
    }
    let mut rows: Vec<(String, i64)> = deltas
        .into_iter()
        .filter(|(_, delta)| *delta != 0)
        .collect();
    rows.sort_by(|left, right| {
        right
            .1
            .abs()
            .cmp(&left.1.abs())
            .then_with(|| left.0.cmp(&right.0))
    });
    rows
}

/// The `§4` findings: cycles with priced cuts, capacity breaches, then any
/// further finding classes the run raised — nothing is dropped silently.
fn findings_section(result: &AnalyzeResult) -> Vec<String> {
    let violations = &result.current.violations;
    let cycles: Vec<&Violation> = violations
        .iter()
        .filter(|violation| violation.kind == ViolationKind::Cycle)
        .collect();
    let capacity: Vec<&Violation> = violations
        .iter()
        .filter(|violation| violation.kind == ViolationKind::Capacity)
        .collect();

    let mut lines = vec![
        format!(
            " cycles ({}) — cuts that would break them, priced by weight:",
            cycles.len()
        ),
        String::new(),
    ];
    for (index, violation) in cycles.iter().enumerate() {
        let mut wrapped = wrap(&cycle_finding_text(violation), 3, 5);
        if let Some(first) = wrapped.first_mut() {
            let body: String = first.chars().skip(3).collect();
            *first = format!(" {}. {body}", index + 1);
        }
        lines.extend(wrapped);
    }

    if !capacity.is_empty() {
        lines.push(String::new());
    }
    lines.push(format!(
        " capacity ({}) — containers and files over their caps today:",
        capacity.len()
    ));
    lines.push(String::new());
    for violation in &capacity {
        lines.extend(wrap(&violation.detail, 1, 3));
    }

    for (kind, lead) in [
        (
            ViolationKind::Polarity,
            "production depending on test code today:",
        ),
        (
            ViolationKind::Visibility,
            "declared visibility wider than the derived scope today:",
        ),
    ] {
        let group: Vec<&Violation> = violations
            .iter()
            .filter(|violation| violation.kind == kind)
            .collect();
        if group.is_empty() {
            continue;
        }
        lines.push(String::new());
        lines.push(format!(" {} ({}) — {lead}", kind_word(kind), group.len()));
        lines.push(String::new());
        for violation in group {
            lines.extend(wrap(&violation.detail, 1, 3));
        }
    }
    lines
}

/// Rebuilds one cycle finding's text from its structured fields, quoting every
/// symbol name; the priced cut leads and further cuts collapse behind an
/// explicit `+N more`.
fn cycle_finding_text(violation: &Violation) -> String {
    let members = violation
        .location
        .iter()
        .map(|name| nq(name))
        .collect::<Vec<_>>()
        .join("/");
    let size = violation.location.len();
    let Some(breaks) = &violation.break_suggestions else {
        return format!("{members} — {size}-symbol cycle");
    };
    let Some(first) = breaks.first() else {
        return format!("{members} — {size}-symbol cycle");
    };
    let method = if first.exact { "exact" } else { "heuristic" };
    let mut text = format!(
        "{members} — {size}-symbol cycle; break {} -> {} (w={:.1}, {method})",
        nq(&first.source),
        nq(&first.target),
        first.weight
    );
    if breaks.len() > 1 {
        let _ = write!(text, ", +{} more", breaks.len() - 1);
    }
    text
}

/// The lowercase report word for a finding class outside cycles and capacity.
fn kind_word(kind: ViolationKind) -> &'static str {
    match kind {
        ViolationKind::Cycle => "cycles",
        ViolationKind::Polarity => "polarity",
        ViolationKind::Capacity => "capacity",
        ViolationKind::Visibility => "visibility",
    }
}

/// Accessor for one scored term of a breakdown.
type Term = fn(&ScoreBreakdown) -> f64;

/// The five scored terms in their fixed table order.
const TERMS: [(&str, Term); 5] = [
    ("cut", |breakdown| breakdown.cut),
    ("imbalance", |breakdown| breakdown.imbalance),
    ("naming", |breakdown| breakdown.naming),
    ("path", |breakdown| breakdown.path),
    ("anchor", |breakdown| breakdown.anchor),
];

/// The `§5` per-term decomposition of the current score versus the featured
/// candidate, with the combine line and the plain-language term gloss. A sixth
/// capacity term prints only when a run actually prices one, keeping every
/// zero-capacity page byte-shaped like the approved artifact.
fn score_delta_section(result: &AnalyzeResult) -> Vec<String> {
    let (proposal_breakdown, proposal_score, gain) = match featured_candidate(result) {
        Some((_, candidate)) => (
            candidate.score_breakdown,
            candidate.score,
            Some(candidate.improvement),
        ),
        None => (result.current.score_breakdown, result.current.score, None),
    };
    let current = &result.current.score_breakdown;

    let mut lines = vec![format!(
        " {:<15}{:>11}    {:>11}    delta",
        "term", "current", "proposal"
    )];
    let capacity_live = current.capacity != 0.0 || proposal_breakdown.capacity != 0.0;
    for (name, term) in TERMS {
        let before = term(current);
        let after = term(&proposal_breakdown);
        lines.push(format!(
            " {name:<15}{:>11}    {:>11}    {:>11}",
            f4(before),
            f4(after),
            sf(round4(after - before))
        ));
    }
    if capacity_live {
        lines.push(format!(
            " {:<15}{:>11}    {:>11}    {:>11}",
            "capacity",
            f4(current.capacity),
            f4(proposal_breakdown.capacity),
            sf(round4(proposal_breakdown.capacity - current.capacity))
        ));
    }
    let gain_note = gain.map_or_else(String::new, |gain| format!("   (gain {})", sf(gain)));
    lines.push(format!(
        " {:<15}{:>11}    {:>11}    {:>11}{gain_note}",
        "score",
        f4(result.current.score),
        f4(proposal_score),
        sf(round4(proposal_score - result.current.score)),
    ));
    lines.push(String::new());
    lines.push(if capacity_live {
        " the terms combine into the total score; lower is better.".to_owned()
    } else {
        " the five terms combine into the total score; lower is better.".to_owned()
    });
    lines.push(String::new());

    let mut gloss = String::from(
        "cut = import connections broken by moving · imbalance = lopsided folder sizes · naming \
         = folder names fit their contents · path = how far things travel · anchor = credit for \
         respecting existing well-placed code",
    );
    if capacity_live {
        gloss.push_str(" · capacity = penalty for containers still over cap");
    }
    lines.extend(labeled_block(" term gloss  ", 13, 13, &gloss));
    lines
}

/// The `§6` verdict band: adopt when there is an action, the second mode's
/// standing, whether keeping is offered, and the partial-adoption rule.
fn recommendation_section(result: &AnalyzeResult) -> Vec<String> {
    let mut lines = Vec::new();
    let modes = present_modes(result);
    let featured = featured_candidate(result);
    let total: usize = modes.iter().map(|(_, mode)| mode.candidates.len()).sum();

    if let Some((featured_name, candidate)) = featured
        && !candidate.delta_narration.is_empty()
    {
        let files: usize = candidate
            .delta_narration
            .iter()
            .map(|entry| entry.files.len())
            .sum();
        let mut adopt = format!(
            "candidate {} — {}, gain {}, {} changes relocating {} files (§2).",
            candidate.index,
            f4(candidate.score),
            sf(candidate.improvement),
            candidate.delta_narration.len(),
            files
        );
        let current = modes
            .iter()
            .find(|(name, _)| *name == featured_name)
            .map_or(&result.current.score_breakdown, |(_, mode)| {
                &mode.current_score_breakdown
            });
        adopt.push_str(&driver_sentence(current, candidate));
        lines.extend(labeled_block(&format!(" {:<11}", "adopt"), 12, 12, &adopt));

        if let Some((other_name, other_mode, other)) = modes
            .iter()
            .filter_map(|(name, mode)| mode.candidates.first().map(|head| (*name, mode, head)))
            .find(|(name, _, _)| *name != featured_name)
        {
            let shape = if other.tree == candidate.tree {
                "the same grouping"
            } else {
                "a related but different shape"
            };
            let pair = if total == 2 { "the two" } else { "them" };
            let second = format!(
                "{other_name} reaches {shape} — {}, gain {} against its own {} baseline; decide \
                 between {pair} by intent, never across baselines.",
                f4(other.score),
                sf(other.improvement),
                f4(other_mode.current_score),
            );
            lines.extend(labeled_block(
                &format!(" {:<11}", "second"),
                12,
                12,
                &second,
            ));
        }
    }

    let infeasible: Vec<&str> = modes
        .iter()
        .filter(|(_, mode)| mode.current_standing == CurrentStanding::Infeasible)
        .map(|(name, _)| *name)
        .collect();
    let keep = if let [single] = infeasible.as_slice() {
        format!("not offered — the {single} mode marks today's layout infeasible under its own caps (§4).")
    } else if infeasible.len() > 1 {
        "not offered — both modes mark today's layout infeasible under its own caps (§4)."
            .to_owned()
    } else {
        match featured {
            Some((_, candidate)) if !candidate.delta_narration.is_empty() => {
                let baseline = modes
                    .iter()
                    .find(|(name, _)| {
                        featured.is_some_and(|(featured_name, _)| featured_name == *name)
                    })
                    .map_or(0.0, |(_, mode)| mode.current_score);
                format!(
                    "keeping today's layout remains possible — it scores {} against this \
                     proposal's {}.",
                    f4(baseline),
                    f4(candidate.score)
                )
            }
            Some(_) => {
                "keep the current layout — the run's best candidate is today's tree unchanged."
                    .to_owned()
            }
            None => "keep the current layout — the run produced no candidate to weigh against it."
                .to_owned(),
        }
    };
    lines.extend(labeled_block(&format!(" {:<11}", "keep"), 12, 12, &keep));
    lines.extend(labeled_block(
        &format!(" {:<11}", "in part"),
        12,
        12,
        "partial plans are not rescored; apply §2 in its given order, one change at a time.",
    ));
    lines
}

/// The adopt paragraph's driver sentence, priced against the featured mode's
/// own baseline: the largest current term the proposal reduces, extended by the
/// flat-cut clause when imports broken stay within a five percent band. Empty
/// when no term dominates.
fn driver_sentence(current: &ScoreBreakdown, candidate: &Candidate) -> String {
    let proposal = &candidate.score_breakdown;
    let mut dominant: Option<(&str, Term)> = None;
    let mut best_magnitude = 0.0_f64;
    for (name, term) in TERMS {
        let magnitude = term(current).abs();
        if magnitude > best_magnitude {
            best_magnitude = magnitude;
            dominant = Some((name, term));
        }
    }
    let Some((name, term)) = dominant else {
        return String::new();
    };
    let (before, after) = (term(current), term(proposal));
    if before <= 0.0 || after >= before {
        return String::new();
    }
    let mut sentence = format!(
        " It goes straight at the {name} that dominates today's score ({} → {}).",
        f4(before),
        f4(after)
    );
    let cut_before = current.cut;
    let cut_after = proposal.cut;
    if cut_before > 0.0 && cut_after <= cut_before && (cut_before - cut_after) / cut_before <= 0.05
    {
        let _ = write!(
            sentence,
            " while imports broken stay nearly flat ({} → {}).",
            f4(cut_before),
            f4(cut_after)
        );
    }
    sentence
}

/// Renders one container `node` as an indented tree to `out`.
///
/// `symbols` lists each file's symbols with their derived visibility; `depth`
/// truncates the tree below the given container depth (the root is depth zero),
/// and `None` renders the full tree.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
pub fn render_tree(
    node: &ContainerNode,
    symbols: bool,
    depth: Option<u32>,
    out: &mut impl Write,
) -> io::Result<()> {
    write_tree_node(node, 0, symbols, depth, out)
}

/// Writes one tree node at `indent` and recurses into its children.
fn write_tree_node(
    node: &ContainerNode,
    indent: u32,
    symbols: bool,
    depth: Option<u32>,
    out: &mut impl Write,
) -> io::Result<()> {
    let pad = "  ".repeat(indent as usize);
    match node.production_sloc {
        Some(sloc) => writeln!(
            out,
            "{pad}{} [{}] {sloc} sloc",
            node.name,
            level_tag(node.level)
        )?,
        None => writeln!(out, "{pad}{} [{}]", node.name, level_tag(node.level))?,
    }

    if let Some(placements) = node.symbols.as_ref().filter(|_| symbols) {
        for placement in placements {
            writeln!(
                out,
                "{pad}  - {} ({})",
                placement.name,
                level_tag(placement.visibility)
            )?;
        }
    }

    if depth.is_some_and(|limit| indent + 1 > limit) {
        return Ok(());
    }
    if let Some(children) = &node.children {
        for child in children {
            write_tree_node(child, indent + 1, symbols, depth, out)?;
        }
    }
    Ok(())
}

/// Writes the narrated move list of `candidate` versus the current tree to `out`.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
pub fn render_diff(candidate: &Candidate, out: &mut impl Write) -> io::Result<()> {
    if candidate.delta_narration.is_empty() {
        return writeln!(out, "no moves");
    }
    let groups = candidate.delta_narration.len();
    let files: usize = candidate
        .delta_narration
        .iter()
        .map(|entry| entry.files.len())
        .sum();
    writeln!(
        out,
        "moves ({groups} group(s), {files} file(s); improvement {:+.4}):",
        candidate.improvement
    )?;
    for line in move_step_lines(&candidate.delta_narration) {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Renders a candidate's moves as numbered per-file steps — the shared body of
/// the `diff` and `report` faces so both speak in one voice.
///
/// Each group prints a header (`{kind} — {reason}`), then one numbered step per
/// moved file reading `{path} [{from} → {to}]`; step numbers run continuously
/// across the candidate so the whole change reads as one ordered plan. Lines
/// carry no trailing newline: `diff` prints them raw, `report` wraps them in a
/// fenced block. An empty source or destination renders as `(root)`.
pub(crate) fn move_step_lines(moves: &[Move]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut step = 1_usize;
    for entry in moves {
        lines.push(format!("{} — {}", move_tag(entry.kind), entry.reason));
        let to = if entry.to.is_empty() {
            "(root)"
        } else {
            &entry.to
        };
        for file in &entry.files {
            let from = if file.from.is_empty() {
                "(root)"
            } else {
                &file.from
            };
            lines.push(format!("  {step}. {} [{from} → {to}]", file.path));
            step = step.saturating_add(1);
        }
    }
    lines
}

/// Writes a violation listing for `violations` to `out`.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
pub fn write_violation_table(violations: &[Violation], out: &mut impl Write) -> io::Result<()> {
    if violations.is_empty() {
        return writeln!(out, "no violations");
    }
    for violation in violations {
        writeln!(
            out,
            "{} [{}] {} :: {}",
            kind_tag(violation.kind),
            severity_tag(violation.severity),
            violation.location.join(", "),
            violation.detail
        )?;
    }
    Ok(())
}

/// Returns the lowercase tag of a scope level.
fn level_tag(level: Level) -> &'static str {
    match level {
        Level::File => "file",
        Level::Folder => "folder",
        Level::Domain => "domain",
        Level::Package => "package",
        Level::PackageGroup => "packageGroup",
    }
}

/// Returns the lowercase tag of a move kind.
fn move_tag(kind: MoveKind) -> &'static str {
    match kind {
        MoveKind::Move => "move",
        MoveKind::Split => "split",
        MoveKind::Merge => "merge",
    }
}

/// Returns the lowercase tag of a violation kind.
fn kind_tag(kind: ViolationKind) -> &'static str {
    match kind {
        ViolationKind::Cycle => "cycle",
        ViolationKind::Polarity => "polarity",
        ViolationKind::Capacity => "capacity",
        ViolationKind::Visibility => "visibility",
    }
}

/// Returns the lowercase tag of a severity.
fn severity_tag(severity: Severity) -> &'static str {
    match severity {
        Severity::Violation => "violation",
        Severity::Borderline => "borderline",
    }
}

#[cfg(test)]
mod tests {
    use strata_engine::{
        CapacityRemainder, ContainerNode, CurrentTree, EdgeBreak, FileMove, Modes, MoveReason,
        ScoreBreakdown, Summary, SymbolPlacement,
    };

    use super::*;

    /// Builds an empty-but-valid result with `violations` on its current tree.
    /// `capacity_breaks` mirrors the engine's own predicate so synthetic results
    /// stay internally consistent; a test that needs the mismatch overrides it.
    fn result_with(violations: Vec<Violation>) -> AnalyzeResult {
        let hard = violations
            .iter()
            .filter(|violation| {
                violation.kind == ViolationKind::Capacity
                    && violation.severity == Severity::Violation
            })
            .count();
        let capacity_breaks = u32::try_from(hard).unwrap_or(u32::MAX);
        AnalyzeResult {
            schema_version: 1,
            snapshot_hash: "deadbeef".to_owned(),
            summary: Summary {
                symbols: 2,
                edges: 1,
                files: 4,
                files_by_language: BTreeMap::from([("ts".to_owned(), 4)]),
            },
            current: CurrentTree {
                tree: file_node("lib", 2),
                score: 1.5,
                score_breakdown: zero_breakdown(),
                capacity_breaks,
                violations,
            },
            modes: Modes::default(),
        }
    }

    /// Builds a result whose anchored mode carries one candidate whose tree has a
    /// `proposed` container, for exercising the candidate faces.
    fn result_with_candidate() -> AnalyzeResult {
        let mut result = result_with(Vec::new());
        result.modes = Modes {
            anchored: Some(ModeResult {
                candidates: vec![Candidate {
                    index: 1,
                    score: 0.25,
                    score_breakdown: zero_breakdown(),
                    improvement: 1.25,
                    tree: ContainerNode {
                        name: "proposed".to_owned(),
                        level: Level::Folder,
                        children: Some(vec![file_node("lib", 1)]),
                        symbols: None,
                        production_sloc: None,
                    },
                    conditional_splits: Vec::new(),
                    delta_narration: Vec::new(),
                    capacity_remainder: None,
                }],
                pairwise_distance: Vec::new(),
                solution_space_converged: true,
                current_score: 1.5,
                current_score_breakdown: zero_breakdown(),
                current_standing: CurrentStanding::Outscored,
            }),
            greenfield: None,
        };
        result
    }

    /// Builds a zeroed score breakdown.
    fn zero_breakdown() -> ScoreBreakdown {
        ScoreBreakdown {
            cut: 0.0,
            imbalance: 0.0,
            naming: 0.0,
            path: 0.0,
            anchor: 0.0,
            capacity: 0.0,
        }
    }

    /// Builds a file container node with `sloc` symbols.
    fn file_node(name: &str, sloc: u32) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::File,
            children: None,
            symbols: Some(vec![SymbolPlacement {
                name: "thing".to_owned(),
                visibility: Level::File,
            }]),
            production_sloc: Some(sloc),
        }
    }

    /// Builds a sample cycle violation.
    fn cycle() -> Violation {
        Violation {
            kind: ViolationKind::Cycle,
            severity: Severity::Violation,
            location: vec!["a".to_owned(), "b".to_owned()],
            detail: "dependency cycle".to_owned(),
            break_suggestions: None,
            capacity: None,
        }
    }

    /// Builds a capacity violation for `name`.
    fn capacity_violation(name: &str) -> Violation {
        Violation {
            kind: ViolationKind::Capacity,
            severity: Severity::Violation,
            location: vec![name.to_owned()],
            detail: format!("{name} over cap"),
            break_suggestions: None,
            capacity: None,
        }
    }

    /// Builds a move of one or more files sharing one origin.
    fn move_of(kind: MoveKind, paths: &[&str], from: &str, to: &str, reason: MoveReason) -> Move {
        Move {
            kind,
            files: paths
                .iter()
                .map(|path| FileMove {
                    path: (*path).to_owned(),
                    from: from.to_owned(),
                })
                .collect(),
            to: to.to_owned(),
            reason,
        }
    }

    #[test]
    fn should_resolve_to_json_when_piped_without_an_explicit_format() {
        assert_eq!(Format::resolve(None, false, Format::Summary), Format::Json);
    }

    #[test]
    fn should_resolve_to_the_human_face_on_a_terminal() {
        assert_eq!(
            Format::resolve(None, true, Format::Summary),
            Format::Summary
        );
    }

    #[test]
    fn should_honour_an_explicit_format_over_the_tty_default() {
        assert_eq!(
            Format::resolve(Some(Format::Json), true, Format::Summary),
            Format::Json
        );
    }

    #[test]
    fn should_render_json_byte_identically_to_the_serialized_result() {
        let result = result_with(Vec::new());
        let mut buffer = Vec::new();

        write_json(&result, &mut buffer).unwrap_or_default();

        let mut expected = serde_json::to_vec(&result).unwrap_or_default();
        expected.push(b'\n');
        assert_eq!(buffer, expected);
    }

    #[test]
    fn should_report_no_violations_when_the_list_is_empty() {
        let mut buffer = Vec::new();

        write_violation_table(&[], &mut buffer).unwrap_or_default();

        assert_eq!(buffer, b"no violations\n");
    }

    #[test]
    fn should_tabulate_a_violation_with_its_kind_and_severity() {
        let mut buffer = Vec::new();

        write_violation_table(&[cycle()], &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(text, "cycle [violation] a, b :: dependency cycle\n");
    }

    #[test]
    fn should_render_a_tree_with_symbols_and_visibility() {
        let mut buffer = Vec::new();

        render_tree(&file_node("lib", 1), true, None, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(text, "lib [file] 1 sloc\n  - thing (file)\n");
    }

    #[test]
    fn should_truncate_a_tree_below_the_requested_depth() {
        let root = ContainerNode {
            name: "root".to_owned(),
            level: Level::Folder,
            children: Some(vec![file_node("child", 1)]),
            symbols: None,
            production_sloc: None,
        };
        let mut buffer = Vec::new();

        render_tree(&root, false, Some(0), &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(text, "root [folder]\n");
    }

    #[test]
    fn should_quote_every_name_token_in_backticks() {
        assert_eq!(nq("logger.ts"), "`logger.ts`");
        assert_eq!(nq("io/util"), "`io/util`");
    }

    #[test]
    fn should_shorten_long_names_with_a_leading_ellipsis() {
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("abcdefghij", 5), "…ghij");
        assert_eq!(tail("abc", 3), "abc");
    }

    #[test]
    fn should_wrap_prose_within_the_grid_with_continuation_indents() {
        let long_text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi \
                         omicron pi rho sigma tau upsilon phi chi psi omega";
        let lines = wrap(long_text, 3, 7);

        assert!(lines.len() >= 2, "long prose wraps: {lines:?}");
        for line in &lines {
            assert!(line.chars().count() <= 99, "wrapped line fits: {line}");
        }
        assert!(
            lines
                .first()
                .is_some_and(|first| first.starts_with("   ")),
            "first pad applies: {lines:?}"
        );
        assert!(
            lines.get(1).is_some_and(|second| second.starts_with("       ")),
            "continuation pad applies"
        );
    }

    #[test]
    fn should_align_labeled_blocks_replacing_the_wrapper_padding() {
        let lines = labeled_block(
            " caption  ",
            11,
            11,
            "WHY pulled toward `x.ts` — weight 1.0. This file already imports its destination \
             heavily, and the move honors that relationship.",
        );

        assert!(lines.len() >= 2, "long captions wrap: {lines:?}");
        assert!(
            lines
                .first()
                .is_some_and(|first| first.starts_with(" caption  WHY pulled")),
            "the label prefixes the block: {lines:?}"
        );
        assert!(lines.iter().skip(1).all(|line| line.starts_with(' ')));
    }

    #[test]
    fn should_trim_weights_to_one_meaningful_decimal() {
        assert_eq!(weight(1.0), "1.0");
        assert_eq!(weight(7.8), "7.8");
        assert_eq!(weight(0.25), "0.25");
        assert_eq!(weight(6.0), "6.0");
    }

    #[test]
    fn should_sign_deltas_explicitly() {
        assert_eq!(sf(2.4194), "+2.4194");
        assert_eq!(sf(-0.0041), "-0.0041");
        // Halfway values follow binary-float formatting (round-half-to-even on
        // the stored value); pin the unambiguous neighbors instead.
        assert_eq!(f4(10.23876), "10.2388");
        assert_eq!(f4(10.23874), "10.2387");
    }

    #[test]
    fn should_disambiguate_colliding_basenames_by_growing_their_suffix() {
        let files = vec![
            FileMove {
                path: "src/a/util.ts".to_owned(),
                from: "x".to_owned(),
            },
            FileMove {
                path: "src/b/util.ts".to_owned(),
                from: "y".to_owned(),
            },
        ];

        let names = leaf_names(&files);

        assert_eq!(names, vec!["a/util.ts".to_owned(), "b/util.ts".to_owned()]);
    }

    #[test]
    fn should_fall_back_to_full_paths_when_suffixes_still_collide() {
        let files = vec![
            FileMove {
                path: "util.ts".to_owned(),
                from: "x".to_owned(),
            },
            FileMove {
                path: "one/util.ts".to_owned(),
                from: "y".to_owned(),
            },
            FileMove {
                path: "two/util.ts".to_owned(),
                from: "z".to_owned(),
            },
        ];

        let names = leaf_names(&files);

        assert_eq!(
            names,
            files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn should_keep_unique_basenames_untouched() {
        let files = vec![
            FileMove {
                path: "src/alpha.ts".to_owned(),
                from: "x".to_owned(),
            },
            FileMove {
                path: "src/beta.ts".to_owned(),
                from: "y".to_owned(),
            },
        ];

        assert_eq!(
            leaf_names(&files),
            vec!["alpha.ts".to_owned(), "beta.ts".to_owned()]
        );
    }

    /// An owned name list for title expectations.
    fn names_of(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn should_title_moves_with_verbs_quotes_and_counts() {
        assert_eq!(
            suggestion_title(MoveKind::Move, &names_of(&["logger.ts"]), "io"),
            "Move `logger.ts` into `io`"
        );
        assert_eq!(
            suggestion_title(MoveKind::Split, &names_of(&["a.ts", "b.ts"]), "pkg/out"),
            "Split `a.ts` and `b.ts` out into `pkg/out`"
        );
        assert_eq!(
            suggestion_title(
                MoveKind::Merge,
                &names_of(&["a.ts", "b.ts", "c.ts"]),
                "core"
            ),
            "Merge 3 files into `core`"
        );
        assert_eq!(
            suggestion_title(MoveKind::Move, &names_of(&["x.ts"]), ""),
            "Move `x.ts` into `(root)`"
        );
    }

    #[test]
    fn should_caption_each_reason_in_the_approved_voice() {
        assert_eq!(
            caption_for(&MoveReason::PulledBy {
                partner: "src/core/engine.ts".to_owned(),
                weight: 1.0,
            }),
            "WHY pulled toward `src/core/engine.ts` — weight 1.0."
        );
        assert_eq!(
            caption_for(&MoveReason::RelievesOverCap {
                container: "ai/adapters".to_owned(),
                count: 33,
                cap: 20,
            }),
            "WHY relieves `ai/adapters`, which holds 33 against a cap of 20."
        );
        assert_eq!(
            caption_for(&MoveReason::Follows {
                subject: "spec/log.ts".to_owned(),
            }),
            "WHY follows `spec/log.ts`, which this plan places nearby."
        );
        assert_eq!(
            caption_for(&MoveReason::Clustering),
            "WHY clusters files that already import each other heavily."
        );
    }

    #[test]
    fn should_rank_blast_rows_by_absolute_change_then_name() {
        let moves = vec![
            move_of(
                MoveKind::Merge,
                &["a.ts", "b.ts"],
                "pkg/one",
                "pkg/two",
                MoveReason::Clustering,
            ),
            move_of(
                MoveKind::Move,
                &["c.ts"],
                "pkg/three",
                "pkg/two",
                MoveReason::Clustering,
            ),
        ];

        let rows = blast_rows(&moves);

        assert_eq!(
            rows,
            vec![
                ("pkg/two".to_owned(), 3),
                ("pkg/one".to_owned(), -2),
                ("pkg/three".to_owned(), -1),
            ]
        );
    }

    #[test]
    fn should_rebuild_cycle_findings_with_quoted_symbols_and_priced_cuts() {
        let violation = Violation {
            kind: ViolationKind::Cycle,
            severity: Severity::Violation,
            location: vec!["alpha".to_owned(), "beta".to_owned()],
            detail: "2-symbol cycle".to_owned(),
            break_suggestions: Some(vec![
                EdgeBreak {
                    source: "beta".to_owned(),
                    target: "alpha".to_owned(),
                    weight: 1.0,
                    exact: true,
                },
                EdgeBreak {
                    source: "alpha".to_owned(),
                    target: "beta".to_owned(),
                    weight: 2.0,
                    exact: false,
                },
            ]),
            capacity: None,
        };

        assert_eq!(
            cycle_finding_text(&violation),
            "`alpha`/`beta` — 2-symbol cycle; break `beta` -> `alpha` (w=1.0, exact), +1 more"
        );
    }

    #[test]
    fn should_build_change_tables_with_quoted_headers_cells_and_elision_honesty() {
        let files = vec![
            FileMove {
                path: "src/a/logger.ts".to_owned(),
                from: "util/log".to_owned(),
            },
            FileMove {
                path: "src/b/cache.ts".to_owned(),
                from: "util/log".to_owned(),
            },
        ];
        let names = leaf_names(&files);

        let (rows, shortened) = change_table(&files, &names, "infra/logging");

        let pipe_positions = |line: &str| {
            line.char_indices()
                .filter(|(_, ch)| *ch == '|')
                .map(|(position, _)| position)
                .collect::<Vec<_>>()
        };
        let header = rows.first().map(String::as_str).unwrap_or_default();
        let first_row = rows.get(1).map(String::as_str).unwrap_or_default();
        assert_eq!(
            pipe_positions(header),
            pipe_positions(first_row),
            "columns align between header and data: {rows:?}"
        );
        for word in ["leaf", "before place", "after place"] {
            assert!(header.contains(word), "header names {word}: {header}");
        }
        assert!(first_row.contains("`logger.ts`"), "{first_row}");
        assert!(!shortened, "short names pass through untailored");

        let long_from = "a/really/quite/unreasonably/long/adapters/path/segment";
        let long_files = vec![FileMove {
            path: "src/x.ts".to_owned(),
            from: long_from.to_owned(),
        }];
        let (_, shortened_long) = change_table(
            &long_files,
            &names_of(&["x.ts"]),
            "another/equally/long/destination",
        );

        assert!(shortened_long, "an overflowing table declares its tails");
    }

    #[test]
    fn should_print_the_claimed_candidate_count_and_list_every_candidate() {
        let result = result_with_candidate();
        let lines = report_lines(&result, "fixture");

        let claim = lines
            .iter()
            .find(|line| line.starts_with(" candidate count"))
            .map(String::as_str);
        assert!(
            claim.is_some_and(|line| line.contains("candidate count : 1")),
            "{lines:?}"
        );
        let listed = lines
            .iter()
            .filter(|line| line.starts_with(" candidate ") && line.contains("score"))
            .count();
        assert_eq!(listed, 1, "every claimed candidate is listed");
    }

    #[test]
    fn should_annotate_the_featured_row_and_other_mode_heads() {
        let mut result = result_with_candidate();
        result.modes.greenfield = Some(ModeResult {
            candidates: vec![Candidate {
                index: 1,
                score: 0.5,
                score_breakdown: zero_breakdown(),
                improvement: 1.0,
                tree: ContainerNode {
                    name: "rebuilt".to_owned(),
                    level: Level::Folder,
                    children: Some(vec![file_node("lib", 1)]),
                    symbols: None,
                    production_sloc: None,
                },
                conditional_splits: Vec::new(),
                delta_narration: Vec::new(),
                capacity_remainder: None,
            }],
            pairwise_distance: Vec::new(),
            solution_space_converged: false,
            current_score: 1.75,
            current_score_breakdown: zero_breakdown(),
            current_standing: CurrentStanding::Outscored,
        });

        let lines = report_lines(&result, "fixture");

        assert!(
            lines.iter().any(|line| line.starts_with(" candidate 1 ")
                && line.contains("anchored/1")
                && line.contains("best score")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.starts_with(" candidate 2 ")
                && line.contains("greenfield/1")
                && line.contains("different shape, own baseline")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("these two gains have different starting points")),
            "{lines:?}"
        );
    }

    #[test]
    fn should_label_grain_on_every_suggestion_block() {
        let mut result = result_with_candidate();
        if let Some(candidate) = result
            .modes
            .anchored
            .as_mut()
            .and_then(|mode| mode.candidates.first_mut())
        {
            candidate.delta_narration = vec![move_of(
                MoveKind::Move,
                &["logger.ts"],
                "src/util",
                "src/io",
                MoveReason::Clustering,
            )];
        }

        let lines = report_lines(&result, "fixture");

        let grains = lines
            .iter()
            .filter(|line| line.starts_with(" grain :"))
            .count();
        assert_eq!(grains, 1, "each suggestion states its grain: {lines:?}");
        assert!(lines.contains(&" grain : whole files".to_owned()));
    }

    #[test]
    fn should_keep_every_report_line_inside_the_hundred_column_grid() {
        let mut result = result_with_candidate();
        result.summary.files = 900;
        if let Some(candidate) = result
            .modes
            .anchored
            .as_mut()
            .and_then(|mode| mode.candidates.first_mut())
        {
            candidate.delta_narration = vec![move_of(
                MoveKind::Merge,
                &["a-very-long-filename-indeed.spec.integration.ts", "b.ts"],
                "src/deeply/nested/adapters/anthropic/dispatch/openrouter",
                "src/equally/deeply/nested/generators/synthographers/adapters/openrouter",
                MoveReason::PulledBy {
                    partner: "src/deeply/nested/generators/synthographers/helpers.ts".to_owned(),
                    weight: 7.8,
                },
            )];
        }

        let lines = report_lines(&result, "fixture");

        for line in &lines {
            assert!(
                line.chars().count() <= 100,
                "line crosses the grid ({} cols): {line}",
                line.chars().count()
            );
        }
        let shortened_notes = lines
            .iter()
            .filter(|line| line.contains("a leading … marks a shortened place name"))
            .count();
        assert_eq!(
            shortened_notes, 1,
            "shortened names are declared explicitly: {lines:?}"
        );
    }

    #[test]
    fn should_state_no_change_when_the_best_candidate_matches_today() {
        let result = result_with_candidate();
        let text = report_lines(&result, "fixture").join("\n");

        assert!(
            text.contains("no change suggested — candidate 1 matches today's layout"),
            "{text}"
        );
        assert!(text.contains("nothing relocates — every container keeps its files."));
        assert!(text.contains("keep the current layout"));
        assert!(text.contains("What would change — candidate 1 (anchored)"));
    }

    #[test]
    fn should_render_the_recommendation_band_with_driver_and_flat_cut_clauses() {
        let mut result = result_with_candidate();
        if let Some(mode) = result.modes.anchored.as_mut() {
            mode.current_score_breakdown.imbalance = 1.0;
            mode.current_score_breakdown.cut = 0.1000;
        }
        if let Some(candidate) = result
            .modes
            .anchored
            .as_mut()
            .and_then(|mode| mode.candidates.first_mut())
        {
            candidate.delta_narration = vec![move_of(
                MoveKind::Move,
                &["logger.ts"],
                "src/util",
                "src/io",
                MoveReason::Clustering,
            )];
            candidate.score_breakdown.imbalance = 0.5;
            candidate.score_breakdown.cut = 0.0999;
        }
        result.modes.greenfield.clone_from(&result.modes.anchored);

        let text = report_lines(&result, "fixture").join("\n");

        for clause in ["adopt", "second", "keep", "in part"] {
            let label = format!(" {clause:<11}");
            assert!(text.contains(&label), "the {clause} clause prints: {text}");
        }
        assert!(
            text.contains("goes straight") && text.contains("dominates today's score"),
            "the driver sentence names the dominant reduced term: {text}"
        );
        assert!(
            text.contains("imports broken") && text.contains("stay nearly flat"),
            "the flat-cut clause rides a small cut change: {text}"
        );
    }

    #[test]
    fn should_declare_keeping_not_offered_when_a_mode_is_infeasible() {
        let mut result = result_with_candidate();
        result.current.violations = vec![
            capacity_violation("big_folder"),
            capacity_violation("huge_file"),
        ];
        // the helper seeded the field before these violations existed
        result.current.capacity_breaks = 2;
        if let Some(mode) = result.modes.anchored.as_mut() {
            mode.current_standing = CurrentStanding::Infeasible;
        }
        if let Some(candidate) = result
            .modes
            .anchored
            .as_mut()
            .and_then(|mode| mode.candidates.first_mut())
        {
            candidate.capacity_remainder = Some(CapacityRemainder {
                remaining: 1,
                file_level: 0,
            });
        }

        let text = report_lines(&result, "fixture").join("\n");

        assert!(
            text.contains(
                "the anchored mode records today's layout as infeasible — it breaks 2 capacity findings"
            ),
            "{text}"
        );
        assert!(
            text.contains("still leaves") && text.contains("above their caps"),
            "the remainder is stated directly: {text}"
        );
        assert!(text.contains("not offered — the anchored mode marks"));
    }

    #[test]
    fn should_render_a_shared_infeasibility_note_for_both_modes() {
        let mut result = result_with_candidate();
        result.modes.greenfield = result.modes.anchored.clone();
        result.current.violations = vec![capacity_violation("big_folder")];
        // the helper seeded the field before this violation existed
        result.current.capacity_breaks = 1;
        if let Some(mode) = result.modes.anchored.as_mut() {
            mode.current_standing = CurrentStanding::Infeasible;
        }
        if let Some(mode) = result.modes.greenfield.as_mut() {
            mode.current_standing = CurrentStanding::Infeasible;
        }

        let text = report_lines(&result, "fixture").join("\n");

        assert!(
            text.contains(
                "both modes record today's layout as infeasible — it breaks 1 capacity finding "
            ),
            "{text}"
        );
        assert_eq!(
            text.matches("today's layout as infeasible").count(),
            1,
            "the shared note prints once: {text}"
        );
    }

    #[test]
    fn should_count_only_hard_capacity_findings_as_breaks() {
        let mut result = result_with_candidate();
        // Two hard breaches plus a borderline observation: the note counts the
        // breaks from the engine's `capacity_breaks`, while §4 still lists all
        // three findings with their severity tags.
        result.current.violations = vec![
            capacity_violation("big_folder"),
            capacity_violation("huge_file"),
            {
                let mut borderline = capacity_violation("warm_folder");
                borderline.severity = Severity::Borderline;
                borderline
            },
        ];
        // what the engine reports for exactly this shape (borderline excluded)
        result.current.capacity_breaks = 2;
        if let Some(mode) = result.modes.anchored.as_mut() {
            mode.current_standing = CurrentStanding::Infeasible;
        }

        let text = report_lines(&result, "fixture").join("\n");

        assert!(
            text.contains("it breaks 2 capacity findings"),
            "borderline observations never count as breaks: {text}"
        );
    }

    #[test]
    fn should_extend_the_term_gloss_only_when_the_capacity_term_is_live() {
        let quiet_text = report_lines(&result_with_candidate(), "fixture").join("\n");

        assert!(quiet_text.contains("the five terms combine into the total score"));
        assert!(!quiet_text.contains("penalty"), "{quiet_text}");

        let mut live = result_with_candidate();
        live.current.score_breakdown.capacity = 0.5;
        let live_text = report_lines(&live, "fixture").join("\n");

        assert!(live_text.contains("the terms combine into the total score"));
        assert!(
            live_text.contains("penalty"),
            "the live capacity term adds its gloss: {live_text}"
        );
    }

    #[test]
    fn should_report_the_converged_solution_space_exactly_when_a_mode_converges() {
        let converged = report_lines(&result_with_candidate(), "fixture").join("\n");
        assert!(converged.contains("the solution space converged"));

        let mut diverged = result_with_candidate();
        if let Some(mode) = diverged.modes.anchored.as_mut() {
            mode.solution_space_converged = false;
        }
        let diverged_text = report_lines(&diverged, "fixture").join("\n");
        assert!(!diverged_text.contains("solution space converged"));
    }

    #[test]
    fn should_carry_no_control_characters_and_no_off_page_tokens() {
        let mut result = result_with_candidate();
        if let Some(candidate) = result
            .modes
            .anchored
            .as_mut()
            .and_then(|mode| mode.candidates.first_mut())
        {
            candidate.delta_narration = vec![move_of(
                MoveKind::Move,
                &["logger.ts"],
                "src/util",
                "src/io",
                MoveReason::Clustering,
            )];
        }
        let lines = report_lines(&result, "fixture");

        for line in &lines {
            let bad = line.chars().find(|ch| ch.is_control());
            assert!(
                bad.is_none(),
                "control character {bad:?} reached the page: {line}"
            );
        }
        let text = lines.join("\n");
        for banned in ["snapshot", "payload", "mockup", "DSG05"] {
            assert!(
                !text.contains(banned),
                "provenance token {banned} stays off the page"
            );
        }
        for interactive in ["menu", "select", "press ", "click", "navigate", "option"] {
            assert!(
                !text.to_lowercase().contains(interactive),
                "interactive token {interactive} stays off the page"
            );
        }
    }

    #[test]
    fn should_banner_the_project_and_close_with_an_end_marker() {
        let text = report_lines(&result_with_candidate(), "acme").join("\n");

        assert!(text.contains(" dataset   : acme"));
        assert!(text.contains(" census    : 2 symbols · 1 edges · 4 files (ts)"));
        assert!(text.contains(" end of report · acme"));
        assert!(text.contains(" reading rules"));
        assert!(text.contains("`backticks`"));
        assert!(
            text.contains("\"future capability — not in current output\""),
            "the future-ledger label appears in the reading rules"
        );
    }

    #[test]
    fn should_report_no_moves_for_an_empty_narration() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            improvement: 0.0,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: Vec::new(),
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        assert_eq!(buffer, b"no moves\n");
    }

    #[test]
    fn should_narrate_a_move_with_its_from_and_to_paths() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            improvement: 1.25,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![move_of(
                MoveKind::Move,
                &["alpha"],
                "old",
                "new",
                MoveReason::Clustering,
            )],
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(
            text,
            "moves (1 group(s), 1 file(s); improvement +1.2500):\nmove — regrouped by clustering\n  1. alpha [old → new]\n"
        );
    }

    #[test]
    fn should_number_every_file_of_a_large_move_group() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            improvement: 0.5,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![move_of(
                MoveKind::Merge,
                &["a.ts", "b.ts", "c.ts", "d.ts"],
                "old",
                "new",
                MoveReason::Clustering,
            )],
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(
            text.contains("merge — regrouped by clustering\n"),
            "the group header names its reason: {text}"
        );
        for (step, file) in ["a.ts", "b.ts", "c.ts", "d.ts"].into_iter().enumerate() {
            assert!(
                text.contains(&format!("  {}. {file} [old → new]\n", step + 1)),
                "{file}: {text}"
            );
        }
    }

    #[test]
    fn should_render_each_files_own_source_in_a_merge() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            improvement: 0.5,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![Move {
                kind: MoveKind::Merge,
                files: vec![
                    FileMove {
                        path: "a.ts".to_owned(),
                        from: "one".to_owned(),
                    },
                    FileMove {
                        path: "b.ts".to_owned(),
                        from: "two".to_owned(),
                    },
                ],
                to: "new".to_owned(),
                reason: MoveReason::Clustering,
            }],
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(text.contains("  1. a.ts [one → new]\n"), "{text}");
        assert!(text.contains("  2. b.ts [two → new]\n"), "{text}");
    }
}
