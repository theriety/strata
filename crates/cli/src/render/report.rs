//! Shared ordered report content for the terminal and Markdown faces.

use std::io::{self, Write};

use strata_engine::AnalyzeResult;

use super::advice::advice_with_options;
use super::findings::findings_lines;
use super::{RenderOptions, effective_parameter_lines, present_modes, wrap};

mod candidate;
mod fit;
mod profile;

use fit::fit_lines;
use profile::profile_lines;

/// A heading and its already-wrapped, format-independent body.
struct Section {
    title: String,
    lines: Vec<String>,
}

/// Presentation content is selected once before applying either output syntax.
pub(super) struct Report {
    summary: Vec<String>,
    sections: Vec<Section>,
}

impl Report {
    pub(super) fn build(result: &AnalyzeResult, project: &str, options: RenderOptions) -> Self {
        let summary = vec![
            format!("Strata report — {project}"),
            format!(
                "{} files · {} symbols · {} edges",
                result.summary.files, result.summary.symbols, result.summary.edges
            ),
            format!("Snapshot `{}`", result.snapshot_hash),
        ];
        let mut findings = findings_lines("Shared findings", &result.current.shared_findings);
        for (name, profile) in present_modes(result) {
            findings.extend(findings_lines(
                &format!("{name} profile-specific findings"),
                &profile.current.unique_findings,
            ));
        }
        let mut candidates = wrap(
            "Lower scores are better. Compare scores only within one parameter profile of one project. A candidate is a proposed layout; inclusion does not make a move Recommended. Partial plans are not rescored.",
            0,
            0,
        );
        if present_modes(result).is_empty() {
            candidates.push("No candidates were produced.".to_owned());
        }
        for (name, profile) in present_modes(result) {
            candidates.extend(profile_lines(result, name, profile, options));
        }
        let mut advice = wrap(
            "Supporting profiles selected the destination; qualified profiles also passed the evidence thresholds; absent profiles did not select the move; conflicts name alternative destinations. Advice consolidates the best candidate from each executed profile.",
            0,
            0,
        );
        advice.extend(advice_with_options(result, options));
        let mut sections = vec![
            Section {
                title: "Structural findings".to_owned(),
                lines: findings,
            },
            Section {
                title: "Candidate layouts".to_owned(),
                lines: candidates,
            },
            Section {
                title: "Advice".to_owned(),
                lines: advice,
            },
        ];
        if options.verbose {
            let mut lines = Vec::new();
            for (name, profile) in present_modes(result) {
                lines.extend(effective_parameter_lines(name, &profile.parameters));
                // retain structured rule values as well as the readable numeric summary.
                if let Ok(parameters) = serde_json::to_string_pretty(&profile.parameters) {
                    lines.push("Complete saved profile parameters:".to_owned());
                    lines.extend(parameters.lines().map(str::to_owned));
                }
            }
            sections.push(Section {
                title: "Effective configuration".to_owned(),
                lines,
            });
        }
        Self { summary, sections }
    }

    pub(super) fn lines(&self) -> Vec<String> {
        let mut lines = self.summary.clone();
        for section in &self.sections {
            lines.push(String::new());
            lines.push(section.title.clone());
            lines.extend(section.lines.clone());
        }
        fit_lines(lines)
    }

    pub(super) fn write_text(&self, out: &mut impl Write) -> io::Result<()> {
        for line in self.lines() {
            writeln!(out, "{line}")?;
        }
        Ok(())
    }

    pub(super) fn markdown(&self) -> String {
        // Fenced bodies preserve action-list wrapping and Unicode tree geometry.
        let mut blocks = vec![format!(
            "# {}",
            self.summary.first().map_or("Strata report", String::as_str)
        )];
        blocks = fit_lines(blocks);
        blocks.push(fit_lines(self.summary.iter().skip(1).cloned().collect()).join("\n"));
        for section in &self.sections {
            blocks.push(format!("## {}", section.title));
            let body = fit_lines(section.lines.clone()).join("\n");
            let fence = "`".repeat(longest_backtick_run(&body).max(2) + 1);
            blocks.push(format!("{fence}text\n{body}\n{fence}"));
        }
        format!("{}\n", blocks.join("\n\n"))
    }
}

/// Avoids terminating a Markdown fence when a source name itself contains backticks.
fn longest_backtick_run(text: &str) -> usize {
    text.split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
}
