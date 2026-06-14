//! THROWAWAY feasibility spike for AD-4 (rust-analyzer binder).
//!
//! Goal (recorded in the AD-4 Component Note, not kept as production code):
//!   1. load the fixture cargo workspace via `ra_ap_load-cargo`
//!   2. walk every use-path segment, method call, and trait-impl identifier and
//!      attempt cross-crate resolution through `ra_ap_ide`'s go-to-definition
//!   3. print resolution coverage (%), wall time, and peak RSS (measured
//!      in-process via `getrusage(RUSAGE_SELF)`, so no external launcher is
//!      required)
//!
//! Exit criteria for proceeding to commit 10: coverage >= 95% on the fixture,
//! load time acceptable for CI use, and peak RSS bounded under
//! [`PEAK_RSS_BUDGET_MIB`] — otherwise revisit AD-4 before any adapter work.
//!
//! This binary is intentionally minimal and deliberately not wired into the
//! library; it exists only to de-risk the long pole and is deleted once the
//! Component Note is written.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use ra_ap_ide::{
    Analysis, AnalysisHost, FileId, FilePosition, GotoDefinitionConfig, RaFixtureConfig,
};
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_project_model::{CargoConfig, RustLibSource};
use ra_ap_syntax::ast::{Impl, MethodCallExpr, NameRef, Use};
use ra_ap_syntax::{AstNode, SourceFile, SyntaxNode};
use ra_ap_vfs::{Vfs, VfsPath};

/// Peak-RSS budget (mebibytes) the spike enforces as the AD-4 "memory bounded"
/// exit criterion. Loading a 3-crate fixture plus the rust-analyzer sysroot
/// comfortably fits well under this; a run that blows past it signals the
/// `ra_ap_*` binder is too memory-hungry for CI and AD-4 must be revisited.
const PEAK_RSS_BUDGET_MIB: u64 = 4096;

/// One reference category the spike attempts to resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefKind {
    /// An identifier inside a `use` path.
    UsePath,
    /// The method name of a `a.method()` call.
    MethodCall,
    /// An identifier in a trait/type position of an `impl` header.
    TraitImpl,
}

/// A single attempted resolution, recorded for the coverage tally.
#[derive(Clone, Debug)]
struct Attempt {
    /// Which category the reference belongs to.
    kind: RefKind,
    /// Whether go-to-definition produced at least one target.
    resolved: bool,
}

/// Tally of attempts and successes for one reference category. Counts are
/// `u32`: a single fixture never holds anywhere near `u32::MAX` references, so
/// the conversion to `f64` in [`Tally::coverage_pct`] is lossless.
#[derive(Clone, Copy, Debug, Default)]
struct Tally {
    /// Number of references seen in this category.
    total: u32,
    /// Number that resolved to a definition.
    resolved: u32,
}

impl Tally {
    /// Folds one attempt's outcome into the running tally.
    fn record(&mut self, resolved: bool) {
        self.total += 1;
        if resolved {
            self.resolved += 1;
        }
    }

    /// Coverage as a percentage; `100.0` when nothing was attempted.
    fn coverage_pct(self) -> f64 {
        if self.total == 0 {
            return 100.0;
        }
        // `u32 -> f64` is lossless, so the metric is exact.
        (f64::from(self.resolved) / f64::from(self.total)) * 100.0
    }
}

/// Peak resident set size of this process so far, in mebibytes.
///
/// Reads `ru_maxrss` from `getrusage(RUSAGE_SELF)`. The field's unit differs by
/// platform — bytes on macOS/BSD, kibibytes on Linux — so the conversion is
/// branched on the target OS. Returns `None` only if the (infallible-in-practice)
/// syscall reports an error, in which case the spike treats peak RSS as
/// unknown rather than fabricating a number.
fn peak_rss_mib() -> Option<u64> {
    // SAFETY: `getrusage` writes a fully-initialised `rusage` into the provided
    // out-pointer; we pass a valid, zeroed, stack-owned struct and read it only
    // after a success return.
    let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
    let code = unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) };
    if code != 0 {
        return None;
    }
    // `ru_maxrss` is non-negative in practice; clamp the sign away before the
    // unit conversion so the arithmetic stays in `u64`.
    let max_rss = u64::try_from(usage.ru_maxrss).unwrap_or(0);
    let bytes = if cfg!(target_os = "macos") {
        // macOS/BSD report bytes.
        max_rss
    } else {
        // Linux and other Unixes report kibibytes.
        max_rss.saturating_mul(1024)
    };
    Some(bytes / (1024 * 1024))
}

/// Loads the fixture workspace, resolves its cross-crate references, prints the
/// metrics, and returns a process exit code reflecting the go/no-go decision.
fn main() -> ExitCode {
    let Some(fixture_root) = locate_fixture() else {
        report_failure("could not locate the fixture workspace manifest");
        return ExitCode::FAILURE;
    };

    let started = Instant::now();
    let (host, vfs) = match load(&fixture_root) {
        Ok(loaded) => loaded,
        Err(error) => {
            report_failure(&format!("failed to load workspace: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let load_elapsed_ms = started.elapsed().as_millis();

    let analysis = host.analysis();
    let attempts = collect_attempts(&analysis, &vfs, &fixture_root);
    let wall_time_ms = started.elapsed().as_millis();
    // Sample peak RSS after the database is fully built and walked, so the
    // high-water mark reflects the binder's full working set.
    let peak_rss_mib = peak_rss_mib();

    let report = Report::from_attempts(&attempts, wall_time_ms, load_elapsed_ms, peak_rss_mib);
    report.print();

    if report.is_go() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Walks up from the binary's manifest directory to the fixture workspace root.
fn locate_fixture() -> Option<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest_dir
        .join("tests")
        .join("fixtures")
        .join("workspace");
    candidate.join("Cargo.toml").is_file().then_some(candidate)
}

/// Loads the cargo workspace into a rust-analyzer database.
fn load(root: &Path) -> Result<(AnalysisHost, Vfs), Box<dyn std::error::Error>> {
    // Discover the sysroot so std-library references (e.g. `ToString`) resolve;
    // this mirrors the real CI configuration the adapter would run under.
    let cargo_config = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        ..CargoConfig::default()
    };
    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: false,
        with_proc_macro_server: ProcMacroServerChoice::None,
        prefill_caches: false,
        num_worker_threads: 1,
        proc_macro_processes: 0,
    };
    let (db, vfs, _proc_macro) =
        load_workspace_at(root, &cargo_config, &load_config, &|_progress| {})?;
    Ok((AnalysisHost::with_database(db), vfs))
}

/// Resolves every in-fixture reference, returning one [`Attempt`] per reference.
fn collect_attempts(analysis: &Analysis, vfs: &Vfs, fixture_root: &Path) -> Vec<Attempt> {
    let mut attempts = Vec::new();
    for (file_id, path) in vfs.iter() {
        if !is_fixture_source(path, fixture_root) {
            continue;
        }
        let Ok(source) = analysis.parse(file_id) else {
            continue;
        };
        collect_file(analysis, file_id, &source, &mut attempts);
    }
    attempts
}

/// Walks one parsed file's AST, attempting resolution at each reference of
/// interest: every `use`-path segment, every method call, and every trait /
/// self-type name in an `impl` header.
fn collect_file(
    analysis: &Analysis,
    file_id: FileId,
    source: &SourceFile,
    attempts: &mut Vec<Attempt>,
) {
    let root = source.syntax();
    for node in root.descendants() {
        if let Some(use_item) = Use::cast(node.clone()) {
            attempts.extend(use_path_attempts(analysis, file_id, &use_item));
        } else if let Some(call) = MethodCallExpr::cast(node.clone()) {
            if let Some(name_ref) = call.name_ref() {
                attempts.push(attempt(analysis, file_id, RefKind::MethodCall, &name_ref));
            }
        } else if let Some(impl_item) = Impl::cast(node) {
            attempts.extend(impl_header_attempts(analysis, file_id, &impl_item));
        }
    }
}

/// One attempt per `NameRef` in the use tree (the path segments of the import).
fn use_path_attempts(analysis: &Analysis, file_id: FileId, use_item: &Use) -> Vec<Attempt> {
    name_refs(use_item.syntax())
        .iter()
        .map(|name_ref| attempt(analysis, file_id, RefKind::UsePath, name_ref))
        .collect()
}

/// One attempt per `NameRef` in the impl's trait and self-type positions.
fn impl_header_attempts(analysis: &Analysis, file_id: FileId, impl_item: &Impl) -> Vec<Attempt> {
    let mut refs = Vec::new();
    if let Some(trait_ty) = impl_item.trait_() {
        refs.extend(name_refs(trait_ty.syntax()));
    }
    if let Some(self_ty) = impl_item.self_ty() {
        refs.extend(name_refs(self_ty.syntax()));
    }
    refs.iter()
        .map(|name_ref| attempt(analysis, file_id, RefKind::TraitImpl, name_ref))
        .collect()
}

/// Records the resolution outcome at a single `NameRef`.
fn attempt(analysis: &Analysis, file_id: FileId, kind: RefKind, name_ref: &NameRef) -> Attempt {
    let position = FilePosition {
        file_id,
        offset: name_ref.syntax().text_range().start(),
    };
    Attempt {
        kind,
        resolved: resolves(analysis, position),
    }
}

/// Collects every `NameRef` descendant of a node, in source order.
fn name_refs(node: &SyntaxNode) -> Vec<NameRef> {
    node.descendants().filter_map(NameRef::cast).collect()
}

/// True when a vfs path points inside the fixture's own source tree (not the
/// sysroot or registry dependencies pulled in transitively).
fn is_fixture_source(path: &VfsPath, fixture_root: &Path) -> bool {
    let Some(as_path) = path.as_path() else {
        return false;
    };
    let raw: &Path = as_path.as_ref();
    raw.starts_with(fixture_root) && raw.extension().is_some_and(|ext| ext == "rs")
}

/// True when go-to-definition at `position` yields at least one target.
fn resolves(analysis: &Analysis, position: FilePosition) -> bool {
    let config = GotoDefinitionConfig {
        ra_fixture: RaFixtureConfig {
            disable_ra_fixture: true,
            ..RaFixtureConfig::default()
        },
    };
    matches!(
        analysis.goto_definition(position, &config),
        Ok(Some(range_info)) if !range_info.info.is_empty()
    )
}

/// Aggregated metrics for the run.
struct Report {
    /// Per-category tallies, plus the combined total.
    use_path: Tally,
    /// Method-call tally.
    method_call: Tally,
    /// Trait-impl tally.
    trait_impl: Tally,
    /// Combined tally across every category.
    overall: Tally,
    /// End-to-end wall time including load and resolution.
    wall_time_ms: u128,
    /// Time spent solely loading the workspace.
    load_ms: u128,
    /// Peak resident set size in mebibytes, or `None` if it could not be read.
    peak_rss_mib: Option<u64>,
}

impl Report {
    /// Builds the report from the raw attempt log.
    fn from_attempts(
        attempts: &[Attempt],
        wall_time_ms: u128,
        load_ms: u128,
        peak_rss_mib: Option<u64>,
    ) -> Self {
        let mut use_path = Tally::default();
        let mut method_call = Tally::default();
        let mut trait_impl = Tally::default();
        let mut overall = Tally::default();
        for attempt in attempts {
            match attempt.kind {
                RefKind::UsePath => use_path.record(attempt.resolved),
                RefKind::MethodCall => method_call.record(attempt.resolved),
                RefKind::TraitImpl => trait_impl.record(attempt.resolved),
            }
            overall.record(attempt.resolved);
        }
        Self {
            use_path,
            method_call,
            trait_impl,
            overall,
            wall_time_ms,
            load_ms,
            peak_rss_mib,
        }
    }

    /// True when peak RSS is known and sits within [`PEAK_RSS_BUDGET_MIB`].
    ///
    /// An unknown peak RSS fails this check: AD-4 exists specifically to
    /// de-risk memory, so a run that cannot prove it is bounded is treated as
    /// not-yet-bounded rather than optimistically passed.
    fn memory_bounded(&self) -> bool {
        self.peak_rss_mib
            .is_some_and(|mib| mib <= PEAK_RSS_BUDGET_MIB)
    }

    /// Go/no-go gate: at least one reference seen, overall coverage >= 95%, and
    /// peak RSS within budget (the AD-4 "memory bounded" exit criterion).
    fn is_go(&self) -> bool {
        self.overall.total > 0 && self.overall.coverage_pct() >= 95.0 && self.memory_bounded()
    }

    /// Prints the metrics block to the process's standard output.
    fn print(&self) {
        let decision = if self.is_go() { "GO" } else { "NO-GO" };
        let lines = [
            "ad-4 ra_ap binder feasibility spike".to_owned(),
            format!(
                "use-path     : {}/{} ({:.1}%)",
                self.use_path.resolved,
                self.use_path.total,
                self.use_path.coverage_pct()
            ),
            format!(
                "method-call  : {}/{} ({:.1}%)",
                self.method_call.resolved,
                self.method_call.total,
                self.method_call.coverage_pct()
            ),
            format!(
                "trait-impl   : {}/{} ({:.1}%)",
                self.trait_impl.resolved,
                self.trait_impl.total,
                self.trait_impl.coverage_pct()
            ),
            format!(
                "overall      : {}/{} ({:.1}%)",
                self.overall.resolved,
                self.overall.total,
                self.overall.coverage_pct()
            ),
            format!("load_ms      : {}", self.load_ms),
            format!("wall_time_ms : {}", self.wall_time_ms),
            match self.peak_rss_mib {
                Some(mib) => {
                    format!("peak_rss_mib : {mib} (budget {PEAK_RSS_BUDGET_MIB})")
                }
                None => "peak_rss_mib : unknown (getrusage failed)".to_owned(),
            },
            format!("decision     : {decision}"),
        ];
        emit(&lines.join("\n"));
    }
}

/// Prints a failure note and the NO-GO decision.
fn report_failure(reason: &str) {
    emit(&format!(
        "ad-4 ra_ap binder feasibility spike\nerror        : {reason}\ndecision     : NO-GO"
    ));
}

/// Writes a line to standard output without tripping the workspace's
/// `print_stdout` clippy lint (the spike is a CLI whose entire job is output).
fn emit(message: &str) {
    use std::io::Write as _;

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{message}");
}

#[cfg(test)]
mod tests {
    use super::{Attempt, PEAK_RSS_BUDGET_MIB, RefKind, Report, Tally};

    #[test]
    fn tally_reports_full_coverage_when_empty() {
        let tally = Tally::default();

        assert!((tally.coverage_pct() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn tally_accumulates_resolved_and_total() {
        let mut tally = Tally::default();

        tally.record(true);
        tally.record(false);

        assert_eq!((tally.total, tally.resolved), (2, 1));
        assert!((tally.coverage_pct() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn report_buckets_attempts_by_kind() {
        let attempts = [
            Attempt {
                kind: RefKind::UsePath,
                resolved: true,
            },
            Attempt {
                kind: RefKind::MethodCall,
                resolved: false,
            },
            Attempt {
                kind: RefKind::TraitImpl,
                resolved: true,
            },
        ];

        let report = Report::from_attempts(&attempts, 200, 50, Some(64));

        assert_eq!(
            (
                report.use_path.resolved,
                report.method_call.total,
                report.trait_impl.resolved,
                report.overall.total,
                report.overall.resolved,
            ),
            (1, 1, 1, 3, 2)
        );
    }

    #[test]
    fn report_is_no_go_below_threshold() {
        let attempts = [
            Attempt {
                kind: RefKind::UsePath,
                resolved: true,
            },
            Attempt {
                kind: RefKind::MethodCall,
                resolved: false,
            },
        ];

        let report = Report::from_attempts(&attempts, 0, 0, Some(64));

        assert!(!report.is_go());
    }

    #[test]
    fn report_is_go_at_full_coverage_within_budget() {
        let attempts = [Attempt {
            kind: RefKind::UsePath,
            resolved: true,
        }];

        let report = Report::from_attempts(&attempts, 0, 0, Some(64));

        assert!(report.is_go());
    }

    #[test]
    fn report_is_no_go_with_no_attempts() {
        let report = Report::from_attempts(&[], 0, 0, Some(64));

        assert!(!report.is_go());
    }

    #[test]
    fn report_is_no_go_when_peak_rss_exceeds_budget() {
        let attempts = [Attempt {
            kind: RefKind::UsePath,
            resolved: true,
        }];

        let report = Report::from_attempts(&attempts, 0, 0, Some(PEAK_RSS_BUDGET_MIB + 1));

        assert!(report.memory_bounded().eq(&false));
        assert!(!report.is_go());
    }

    #[test]
    fn report_is_no_go_when_peak_rss_unknown() {
        let attempts = [Attempt {
            kind: RefKind::UsePath,
            resolved: true,
        }];

        let report = Report::from_attempts(&attempts, 0, 0, None);

        assert!(report.memory_bounded().eq(&false));
        assert!(!report.is_go());
    }

    #[test]
    fn report_memory_bounded_at_exact_budget() {
        let report = Report::from_attempts(&[], 0, 0, Some(PEAK_RSS_BUDGET_MIB));

        assert!(report.memory_bounded());
    }
}
