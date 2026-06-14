//! Test projection: specs follow their subjects, helpers cluster like code, and
//! production never depends on test code.
//!
//! Test code is projected, not optimized. The optimizer already ran over
//! production plus test-support; this pass places the remaining `TestCase` files
//! and validates polarity. Each spec is placed beside the production file that
//! receives the largest share of its outgoing edge weight — its *subject* — under
//! the package's detected convention: a co-located sibling next to the subject,
//! or the mirrored path inside a parallel test folder. The convention is decided
//! by majority vote over the package's existing spec placements and echoed in the
//! report, never imposed.
//!
//! Polarity is the inviolable contract of AD-3: production code must never depend
//! on test code. After placement, every dependency edge is checked — a
//! production → {test-case, test-support} edge, or a test-support → test-case
//! edge, is a hard [`Violation`]. These are vetoes, not penalties: they surface
//! as violations rather than being traded against cohesion.

use strata_ir::{NodeId, Polarity};

/// Where a package places its spec files relative to their subjects.
///
/// `CoLocatedSibling` keeps each spec next to the production file it tests;
/// `MirroredFolder` puts it at the same relative path under a parallel test
/// directory. The convention is detected per package, never assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecConvention {
    /// The spec lives beside its subject (`foo.rs` + `foo.test.rs`).
    CoLocatedSibling,
    /// The spec lives at the mirrored path in a test folder (`tests/foo.rs`).
    MirroredFolder,
}

/// A package as test projection reads it: the spec placements already present in
/// source, used only to detect the package's convention.
///
/// Each [`SpecPlacement`] records how one existing spec sits relative to its
/// subject. `detect_convention` takes the majority over these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageView {
    /// The existing spec placements observed in the package's source.
    pub placements: Vec<SpecPlacement>,
}

/// How a single existing spec sits relative to its subject in source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpecPlacement {
    /// The spec file's node.
    pub spec: NodeId,
    /// The convention this spec follows in source.
    pub convention: SpecConvention,
}

/// A test-case file awaiting projection, with the production subjects it depends
/// on weighted by edge weight.
///
/// `subjects` lists each production file this spec depends on and the summed
/// weight of the spec's outgoing edges into it; the heaviest is the subject the
/// spec is placed beside.
#[derive(Debug, Clone, PartialEq)]
pub struct TestCaseFile {
    /// The test-case file's node.
    pub file: NodeId,
    /// Production subjects and the spec's outgoing edge weight into each.
    pub subjects: Vec<SubjectWeight>,
    /// The production file this spec is placed beside once projected.
    pub placed_beside: Option<NodeId>,
}

/// A production subject of a spec and the spec's outgoing edge weight into it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubjectWeight {
    /// The production subject file.
    pub subject: NodeId,
    /// Summed weight of the spec's edges into `subject`.
    pub weight: f64,
}

/// A directed dependency between two placed nodes, used for polarity validation.
///
/// `source` depends on `target`; the projection rejects any such edge that
/// violates the allowed polarity matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionEdge {
    /// The depending node.
    pub source: NodeId,
    /// The depended-upon node.
    pub target: NodeId,
}

/// A candidate structure undergoing test projection: the test-case files to
/// place and the edges to validate.
///
/// `test_cases` are mutated in place — each gains its `placed_beside` subject.
/// `edges` are the dependencies validated against the polarity matrix after
/// placement.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The test-case files to project.
    pub test_cases: Vec<TestCaseFile>,
    /// The dependency edges to validate.
    pub edges: Vec<ProjectionEdge>,
}

/// A hard polarity violation discovered during projection.
///
/// `kind` names which forbidden transition occurred; the node pair identifies the
/// offending edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Violation {
    /// The depending node.
    pub source: NodeId,
    /// The depended-upon node.
    pub target: NodeId,
    /// Which forbidden polarity transition this edge represents.
    pub kind: ViolationKind,
}

/// The forbidden polarity transitions test projection rejects (AD-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    /// Production code depends on a test case.
    ProductionToTestCase,
    /// Production code depends on test support.
    ProductionToTestSupport,
    /// Test support depends on a test case.
    TestSupportToTestCase,
}

/// Detects a package's spec-placement convention by majority vote over its
/// existing placements.
///
/// Co-located siblings and mirrored folders are tallied; the larger tally wins.
/// A tie — including a package with no existing specs — defaults to
/// [`SpecConvention::CoLocatedSibling`], the convention that keeps specs nearest
/// their subjects. The decision is meant to be echoed in the report.
#[must_use]
pub fn detect_convention(package: &PackageView) -> SpecConvention {
    let mut co_located = 0_usize;
    let mut mirrored = 0_usize;
    for placement in &package.placements {
        match placement.convention {
            SpecConvention::CoLocatedSibling => co_located += 1,
            SpecConvention::MirroredFolder => mirrored += 1,
        }
    }

    if mirrored > co_located {
        SpecConvention::MirroredFolder
    } else {
        SpecConvention::CoLocatedSibling
    }
}

/// Places every test-case file beside its subject and validates polarity.
///
/// For each test-case file, the subject is the production file receiving the
/// largest share of the spec's outgoing edge weight (ties broken by ascending
/// node id); the spec is recorded as placed beside it. The chosen `convention`
/// governs whether that means a sibling path or a mirrored one — placement is the
/// same node either way, so the convention is carried for the report rather than
/// changing the subject.
///
/// After placement, every edge is checked against the polarity matrix: a
/// production → {test-case, test-support} edge or a test-support → test-case edge
/// is returned as a hard [`Violation`]. Violations are ordered by ascending
/// `(source, target)` for determinism.
///
/// `polarity` is indexed by raw [`NodeId`]; nodes outside its range are treated
/// as production, the conservative default.
#[must_use]
pub fn project(
    candidate: &mut Candidate,
    convention: SpecConvention,
    polarity: &[Polarity],
) -> Vec<Violation> {
    let _ = convention;
    for test_case in &mut candidate.test_cases {
        test_case.placed_beside = dominant_subject(&test_case.subjects);
    }

    validate_polarity(&candidate.edges, polarity)
}

/// Returns the production subject with the largest outgoing edge weight, breaking
/// ties toward the smaller node id; `None` when the spec depends on nothing.
fn dominant_subject(subjects: &[SubjectWeight]) -> Option<NodeId> {
    subjects
        .iter()
        .copied()
        .reduce(|best, candidate| {
            // heaviest weight wins; a tie (or unorderable NaN) falls to the
            // smaller id, which keeps the choice deterministic without comparing
            // the floats for equality directly.
            match candidate
                .weight
                .partial_cmp(&best.weight)
                .unwrap_or(std::cmp::Ordering::Equal)
            {
                std::cmp::Ordering::Greater => candidate,
                std::cmp::Ordering::Less => best,
                std::cmp::Ordering::Equal => {
                    if candidate.subject.0 < best.subject.0 {
                        candidate
                    } else {
                        best
                    }
                }
            }
        })
        .map(|winner| winner.subject)
}

/// Validates every edge against the allowed polarity matrix, collecting hard
/// violations sorted by ascending `(source, target)`.
fn validate_polarity(edges: &[ProjectionEdge], polarity: &[Polarity]) -> Vec<Violation> {
    let mut violations: Vec<Violation> = edges
        .iter()
        .filter_map(|edge| {
            let source = polarity_of(polarity, edge.source);
            let target = polarity_of(polarity, edge.target);
            violation_kind(source, target).map(|kind| Violation {
                source: edge.source,
                target: edge.target,
                kind,
            })
        })
        .collect();

    violations.sort_by(|left, right| {
        left.source
            .0
            .cmp(&right.source.0)
            .then(left.target.0.cmp(&right.target.0))
    });
    violations
}

/// Returns the polarity of `node`, defaulting to [`Polarity::Production`] when
/// the node is outside the `polarity` slice.
fn polarity_of(polarity: &[Polarity], node: NodeId) -> Polarity {
    polarity
        .get(node.0 as usize)
        .copied()
        .unwrap_or(Polarity::Production)
}

/// Classifies a `source -> target` polarity pair as a violation, or `None` when
/// the dependency is allowed.
///
/// The allowed matrix is: production depends on production only; test support on
/// production and test support; a test case on anything.
fn violation_kind(source: Polarity, target: Polarity) -> Option<ViolationKind> {
    match (source, target) {
        (Polarity::Production, Polarity::TestCase) => Some(ViolationKind::ProductionToTestCase),
        (Polarity::Production, Polarity::TestSupport) => {
            Some(ViolationKind::ProductionToTestSupport)
        }
        (Polarity::TestSupport, Polarity::TestCase) => Some(ViolationKind::TestSupportToTestCase),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a subject-weight pair.
    fn subject(node: u32, weight: f64) -> SubjectWeight {
        SubjectWeight {
            subject: NodeId(node),
            weight,
        }
    }

    /// Builds a projection edge.
    fn edge(source: u32, target: u32) -> ProjectionEdge {
        ProjectionEdge {
            source: NodeId(source),
            target: NodeId(target),
        }
    }

    /// Builds an existing spec placement.
    fn placement(spec: u32, convention: SpecConvention) -> SpecPlacement {
        SpecPlacement {
            spec: NodeId(spec),
            convention,
        }
    }

    #[test]
    fn should_detect_the_majority_co_located_convention() {
        let package = PackageView {
            placements: vec![
                placement(0, SpecConvention::CoLocatedSibling),
                placement(1, SpecConvention::CoLocatedSibling),
                placement(2, SpecConvention::MirroredFolder),
            ],
        };

        assert_eq!(
            detect_convention(&package),
            SpecConvention::CoLocatedSibling
        );
    }

    #[test]
    fn should_detect_the_majority_mirrored_convention() {
        let package = PackageView {
            placements: vec![
                placement(0, SpecConvention::MirroredFolder),
                placement(1, SpecConvention::MirroredFolder),
                placement(2, SpecConvention::CoLocatedSibling),
            ],
        };

        assert_eq!(detect_convention(&package), SpecConvention::MirroredFolder);
    }

    #[test]
    fn should_default_to_co_located_on_a_tie() {
        let package = PackageView {
            placements: vec![
                placement(0, SpecConvention::MirroredFolder),
                placement(1, SpecConvention::CoLocatedSibling),
            ],
        };

        assert_eq!(
            detect_convention(&package),
            SpecConvention::CoLocatedSibling
        );
    }

    #[test]
    fn should_default_to_co_located_for_an_empty_package() {
        let package = PackageView {
            placements: Vec::new(),
        };

        assert_eq!(
            detect_convention(&package),
            SpecConvention::CoLocatedSibling
        );
    }

    #[test]
    fn should_place_a_spec_beside_its_heaviest_subject() {
        let mut candidate = Candidate {
            test_cases: vec![TestCaseFile {
                file: NodeId(10),
                subjects: vec![subject(1, 0.5), subject(2, 3.0), subject(3, 1.0)],
                placed_beside: None,
            }],
            edges: Vec::new(),
        };

        let violations = project(&mut candidate, SpecConvention::CoLocatedSibling, &[]);

        assert!(violations.is_empty());
        assert_eq!(
            candidate.test_cases.first().and_then(|tc| tc.placed_beside),
            Some(NodeId(2))
        );
    }

    #[test]
    fn should_break_subject_weight_ties_toward_the_smaller_id() {
        let mut candidate = Candidate {
            test_cases: vec![TestCaseFile {
                file: NodeId(10),
                subjects: vec![subject(5, 2.0), subject(3, 2.0)],
                placed_beside: None,
            }],
            edges: Vec::new(),
        };

        let violations = project(&mut candidate, SpecConvention::MirroredFolder, &[]);

        assert!(violations.is_empty());
        assert_eq!(
            candidate.test_cases.first().and_then(|tc| tc.placed_beside),
            Some(NodeId(3))
        );
    }

    #[test]
    fn should_leave_a_subjectless_spec_unplaced() {
        let mut candidate = Candidate {
            test_cases: vec![TestCaseFile {
                file: NodeId(10),
                subjects: Vec::new(),
                placed_beside: None,
            }],
            edges: Vec::new(),
        };

        let violations = project(&mut candidate, SpecConvention::CoLocatedSibling, &[]);

        assert!(violations.is_empty());
        assert_eq!(
            candidate.test_cases.first().and_then(|tc| tc.placed_beside),
            None
        );
    }

    #[test]
    fn should_flag_a_production_to_test_case_edge() {
        // node 0 production depends on node 1 test case.
        let polarity = vec![Polarity::Production, Polarity::TestCase];
        let mut candidate = Candidate {
            test_cases: Vec::new(),
            edges: vec![edge(0, 1)],
        };

        let violations = project(&mut candidate, SpecConvention::CoLocatedSibling, &polarity);

        assert_eq!(
            violations,
            vec![Violation {
                source: NodeId(0),
                target: NodeId(1),
                kind: ViolationKind::ProductionToTestCase,
            }]
        );
    }

    #[test]
    fn should_flag_a_production_to_test_support_edge() {
        let polarity = vec![Polarity::Production, Polarity::TestSupport];
        let mut candidate = Candidate {
            test_cases: Vec::new(),
            edges: vec![edge(0, 1)],
        };

        let violations = project(&mut candidate, SpecConvention::CoLocatedSibling, &polarity);

        assert_eq!(
            violations,
            vec![Violation {
                source: NodeId(0),
                target: NodeId(1),
                kind: ViolationKind::ProductionToTestSupport,
            }]
        );
    }

    #[test]
    fn should_flag_a_test_support_to_test_case_edge() {
        let polarity = vec![Polarity::TestSupport, Polarity::TestCase];
        let mut candidate = Candidate {
            test_cases: Vec::new(),
            edges: vec![edge(0, 1)],
        };

        let violations = project(&mut candidate, SpecConvention::CoLocatedSibling, &polarity);

        assert_eq!(
            violations.first().map(|violation| violation.kind),
            Some(ViolationKind::TestSupportToTestCase)
        );
    }

    #[test]
    fn should_allow_test_support_to_production_and_test_case_to_anything() {
        // support -> production (ok), test case -> production (ok),
        // test case -> support (ok), production -> production (ok).
        let polarity = vec![
            Polarity::Production,
            Polarity::TestSupport,
            Polarity::TestCase,
        ];
        let mut candidate = Candidate {
            test_cases: Vec::new(),
            edges: vec![edge(1, 0), edge(2, 0), edge(2, 1), edge(0, 0)],
        };

        let violations = project(&mut candidate, SpecConvention::CoLocatedSibling, &polarity);

        assert!(violations.is_empty());
    }

    #[test]
    fn should_sort_violations_by_source_then_target() {
        let polarity = vec![
            Polarity::Production,
            Polarity::TestCase,
            Polarity::Production,
            Polarity::TestSupport,
        ];
        let mut candidate = Candidate {
            test_cases: Vec::new(),
            // emit out of order: (2 -> 3) then (0 -> 1).
            edges: vec![edge(2, 3), edge(0, 1)],
        };

        let violations = project(&mut candidate, SpecConvention::CoLocatedSibling, &polarity);

        let pairs: Vec<(u32, u32)> = violations
            .iter()
            .map(|violation| (violation.source.0, violation.target.0))
            .collect();
        assert_eq!(pairs, vec![(0, 1), (2, 3)]);
    }

    #[test]
    fn should_treat_out_of_range_nodes_as_production() {
        // node 5 is outside the polarity slice -> production; depending on a test
        // case (node 1) is a violation.
        let polarity = vec![Polarity::Production, Polarity::TestCase];
        let mut candidate = Candidate {
            test_cases: Vec::new(),
            edges: vec![edge(5, 1)],
        };

        let violations = project(&mut candidate, SpecConvention::CoLocatedSibling, &polarity);

        assert_eq!(
            violations.first().map(|violation| violation.kind),
            Some(ViolationKind::ProductionToTestCase)
        );
    }
}
