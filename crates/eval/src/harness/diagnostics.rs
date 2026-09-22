//! Diagnostic serialization and conditional dump writing.

use std::path::Path;

use strata_engine::result::AnalyzeResult;

use crate::error::EvalError;

use super::{CaseReport, ModeObservation, PairF1Report, Verdict};

/// A serializable diagnostics dump written when `STRATA_BLESS_EVAL` is set.
///
/// This regenerates evidence only — expectations live in the targets, never in
/// a golden file, so there is no bless path that could rewrite them.
#[derive(serde::Serialize)]
struct DumpVerdict {
    label: String,
    passed: bool,
    detail: String,
}

/// The dump document: the case's verdicts plus the full engine result.
#[derive(serde::Serialize)]
struct DumpDoc<'a> {
    fixture: &'a str,
    preconditions: Vec<DumpVerdict>,
    verdicts: Vec<DumpVerdict>,
    observations: Vec<ModeObservation>,
    pair_f1: PairF1Report,
    result: &'a AnalyzeResult,
}

/// Writes the diagnostic dump when the environment asks for one; a dump
/// failure surfaces as a case error, never silently.
pub(super) fn dump_diagnostics(result: &AnalyzeResult, report: &mut CaseReport) {
    if std::env::var_os("STRATA_BLESS_EVAL").is_none() {
        return;
    }
    let fixture = report.fixture.clone();
    let doc = DumpDoc {
        fixture: &fixture,
        preconditions: dump_verdicts(&report.preconditions),
        verdicts: dump_verdicts(&report.verdicts),
        observations: report.observations.clone(),
        pair_f1: report.pair_f1.clone(),
        result,
    };
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or(Path::new(env!("CARGO_MANIFEST_DIR")));
    let directory = workspace.join("target").join("strata-eval-diagnostics");
    let outcome = std::fs::create_dir_all(&directory).and_then(|()| {
        let path = directory.join(format!("{fixture}.json"));
        std::fs::write(path, serde_json::to_string_pretty(&doc).unwrap_or_default())
    });
    if let Err(error) = outcome {
        report.errors.push(EvalError::DumpFailed {
            fixture,
            message: error.to_string(),
        });
    }
}

/// Converts verdicts to their serializable shape.
fn dump_verdicts(verdicts: &[Verdict]) -> Vec<DumpVerdict> {
    verdicts
        .iter()
        .map(|verdict| DumpVerdict {
            label: verdict.label.clone(),
            passed: verdict.passed,
            detail: verdict.detail.clone(),
        })
        .collect()
}
