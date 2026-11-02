//! Partition helpers keying clusters by folded-file pins and home names.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_core::cluster::{ClusterId, Partition};

use crate::analyze::layout::NameTally;

/// Carries each folder's fold pin (ADR-21) up to the domain holding it;
/// domains holding no folded file keep the empty key.
pub(super) fn lift_pins(
    folder_pins: &[SmolStr],
    domain_parts: &Partition,
    domain_count: usize,
) -> Vec<SmolStr> {
    let mut domain_pins = vec![SmolStr::default(); domain_count];
    for (folder, pin) in folder_pins.iter().enumerate() {
        if let Some(slot) = domain_parts
            .cluster_of(u32::try_from(folder).unwrap_or(u32::MAX))
            .and_then(|domain| domain_pins.get_mut(domain.0 as usize))
            && !pin.is_empty()
        {
            slot.clone_from(pin);
        }
    }
    domain_pins
}

/// Names each package holding a pinned domain by its pin alone: a folded file
/// keeps its pass-start path (ADR-21), so the package drawn around it must be
/// the one it already lives in, whoever else joined it.
pub(super) fn name_pinned_packages(
    package_tally: &mut NameTally,
    domain_pins: &[SmolStr],
    package_parts: &Partition,
) {
    for (domain, pin) in domain_pins.iter().enumerate() {
        let package = u32::try_from(domain)
            .ok()
            .and_then(|domain| package_parts.cluster_of(domain));
        if let (Some(package), false) = (package, pin.is_empty())
            && let Some(tally) = package_tally.get_mut(&package.0)
        {
            let total = tally
                .values()
                .fold((0, 0), |(sloc, count), &(s, c)| (sloc + s, count + c));
            *tally = BTreeMap::from([(pin.clone(), total)]);
        }
    }
}

/// Refines `parts` so no cluster mixes vertices of different `keys`: each
/// (cluster, key) pair becomes its own cluster, numbered densely in vertex order.
pub(super) fn split_by_key(parts: &Partition, keys: &[SmolStr]) -> Partition {
    let mut ids: BTreeMap<(u32, &SmolStr), u32> = BTreeMap::new();
    let assignment = parts
        .assignment()
        .iter()
        .zip(keys)
        .map(|(cluster, key)| {
            let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
            ClusterId(*ids.entry((cluster.0, key)).or_insert(next))
        })
        .collect();
    Partition::from_assignment(assignment, ids.len())
}

/// Clusters vertices by exact key equality, numbered densely in vertex order.
pub(super) fn group_by_key(keys: &[SmolStr]) -> Partition {
    let mut ids: BTreeMap<&SmolStr, u32> = BTreeMap::new();
    let assignment = keys
        .iter()
        .map(|key| {
            let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
            ClusterId(*ids.entry(key).or_insert(next))
        })
        .collect();
    Partition::from_assignment(assignment, ids.len())
}
