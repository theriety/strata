//! Assertions over container shape: size bands, synthetic buckets, and name
//! alignment.

use strata_engine::result::{ContainerNode, Level};

use crate::harness::Verdict;
use crate::harness::inputs::EvalInputs;
use crate::metrics;
use crate::target::{BandScope, BucketName, FaceMode, NameAlignment, SizeBand};

/// `size_band`: every selected container's first-level member count lies
/// within the inclusive band. Only a folder's own children are its members —
/// nested sub-places contribute nothing — so splitting an over-cap folder into
/// halves that nest under it reads as two within-band places.
pub(super) fn evaluate_size_band(
    band: &SizeBand,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
) -> Verdict {
    let selector = match (&band.scope, &band.container) {
        (Some(BandScope::AnyContainer), _) => "any_container".to_owned(),
        (_, Some(name)) => format!("container({name})"),
        (None, None) => "unscoped".to_owned(),
    };
    let label = format!("size_band({selector},max={})#{face:?}", band.max_files);
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let mut worst: Option<(String, usize)> = None;
    {
        let mut visit = |node: &ContainerNode, _chain: &str| {
            let selected = match (&band.scope, &band.container) {
                (Some(BandScope::AnyContainer), _) => metrics::is_scoped_container(node),
                (_, Some(name)) => node.name == *name && node.level != Level::File,
                (None, None) => false,
            };
            if !selected {
                return;
            }
            let count = metrics::direct_members(node).len();
            let over_max = usize::try_from(band.max_files).is_ok_and(|max| count > max);
            let under_min = band
                .min_files
                .and_then(|min| usize::try_from(min).ok())
                .is_some_and(|min| count < min);
            if over_max || under_min {
                let worse = worst
                    .as_ref()
                    .is_none_or(|(_, worst_count)| count > *worst_count);
                if worse {
                    worst = Some((node.name.clone(), count));
                }
            }
        };
        metrics::walk_containers(tree, &mut visit);
    }

    match worst {
        None => Verdict {
            label,
            passed: true,
            detail: "every selected container sits within its band".to_owned(),
        },
        Some((name, count)) => {
            let bound = band.min_files.map_or_else(
                || format!("max {}", band.max_files),
                |min| format!("min {min}/max {}", band.max_files),
            );
            Verdict {
                label,
                passed: false,
                detail: format!(
                    "over-capacity: container {name:?} holds {count} members against {bound} (first-level members only)"
                ),
            }
        }
    }
}

/// `no_synthetic_bucket`: no non-file node may carry the forbidden last segment.
pub(super) fn evaluate_no_synthetic_bucket(
    bucket: &BucketName,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
) -> Verdict {
    let label = format!("no_synthetic_bucket({})#{face:?}", bucket.name);
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let mut offenders: Vec<String> = Vec::new();
    {
        let mut visit = |node: &ContainerNode, _chain: &str| {
            if node.level != Level::File && metrics::last_segment(&node.name) == bucket.name {
                offenders.push(format!("{:?} ({:?})", node.name, node.level));
            }
        };
        metrics::walk_containers(tree, &mut visit);
    }
    Verdict {
        label,
        passed: offenders.is_empty(),
        detail: if offenders.is_empty() {
            format!("no node carries the synthetic bucket {:?}", bucket.name)
        } else {
            format!(
                "workspace collapse: synthetic bucket {:?} appears at {}",
                bucket.name,
                offenders.join(", ")
            )
        },
    }
}

/// `name_alignment`: every folder/domain container with at least `min_members`
/// first-level members keeps at least `min_ratio` of them sharing a naming
/// token. A folder is judged by its own children; descendants belong to their
/// own places and never dilute an ancestor's alignment.
pub(super) fn evaluate_name_alignment(
    alignment: &NameAlignment,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
) -> Verdict {
    let min_ratio = alignment.min_ratio;
    let min_members_requested = alignment.min_members;
    let label = format!(
        "name_alignment(min_ratio={min_ratio},min_members={min_members_requested})#{face:?}"
    );
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let min_members = usize::try_from(alignment.min_members).unwrap_or(usize::MAX);
    let mut worst: Option<(String, usize, usize)> = None;
    {
        let mut visit = |node: &ContainerNode, _chain: &str| {
            if !metrics::is_scoped_container(node) {
                return;
            }
            let members = metrics::direct_members(node);
            if members.len() < min_members || members.is_empty() {
                return;
            }
            let (aligned, total) = metrics::alignment_counts(&node.name, &members);
            let below_floor =
                metrics::alignment_sharing(&node.name, &members) < min_ratio - f64::EPSILON;
            let worse = worst
                .as_ref()
                .is_none_or(|(_, _, worst_aligned)| aligned < *worst_aligned);
            if below_floor && worse {
                worst = Some((node.name.clone(), aligned, total));
            }
        };
        metrics::walk_containers(tree, &mut visit);
    }

    match worst {
        None => Verdict {
            label,
            passed: true,
            detail: "every qualifying container aligns with its members".to_owned(),
        },
        Some((name, aligned, total)) => Verdict {
            label,
            passed: false,
            detail: format!(
                "naming incoherence: container {name:?} aligns {aligned}/{total} members below min_ratio {min_ratio}"
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use strata_engine::result::Level;

    use crate::harness::assertions::fixtures::{file, inputs_with, laminar_candidate, node};
    use crate::target::{BucketName, FaceMode, SizeBand};

    use super::{evaluate_no_synthetic_bucket, evaluate_size_band};

    #[test]
    fn no_synthetic_bucket_reads_last_segments_at_any_level() {
        let bucketed = node(
            Level::PackageGroup,
            "root",
            vec![node(
                Level::Package,
                "app",
                vec![node(Level::Domain, "workspace", vec![file("loose.py")])],
            )],
        );
        let bucketed_inputs = inputs_with(&bucketed);
        let bucket = BucketName {
            name: "workspace".to_owned(),
            mode: None,
            because: "real directories never collapse into workspace".to_owned(),
        };
        let verdict = evaluate_no_synthetic_bucket(&bucket, FaceMode::Anchored, &bucketed_inputs);
        assert!(!verdict.passed);
        assert!(verdict.detail.contains("workspace"));

        let laminar = laminar_candidate();
        let laminar_inputs = inputs_with(&laminar);
        let laminar_verdict =
            evaluate_no_synthetic_bucket(&bucket, FaceMode::Anchored, &laminar_inputs);
        assert!(laminar_verdict.passed);
    }

    #[test]
    fn size_band_container_selector_matches_full_prefix_names_any_level() {
        let wide = node(
            Level::PackageGroup,
            "root",
            vec![node(
                Level::Package,
                "app",
                vec![node(
                    Level::Folder,
                    "hub",
                    vec![
                        file("hub/a.py"),
                        file("hub/b.py"),
                        file("hub/c.py"),
                        file("hub/d.py"),
                    ],
                )],
            )],
        );
        let inputs = inputs_with(&wide);
        let band = SizeBand {
            max_files: 2,
            min_files: None,
            scope: None,
            container: Some("hub".to_owned()),
            mode: None,
            because: "hub splits".to_owned(),
        };
        let verdict = evaluate_size_band(&band, FaceMode::Anchored, &inputs);
        assert!(!verdict.passed);
        assert!(verdict.detail.contains("holds 4 members"));
    }
}
