//! The test-root zone permission of the identity guard (ADR-26).

use strata_core::cluster::ClusterId;

use crate::analyze::relocation::identity_guard::RelocationIdentityGuard;

impl RelocationIdentityGuard {
    /// Whether `moving` may join `target` without crossing a test-root zone:
    /// `target` is one of its own pass-start clusters, or every zone its files
    /// belong to is held by files physically in `target` at pass start. A
    /// production file therefore never enters a folder of test files alone, a
    /// test file never leaves its test root, and a test file among production
    /// files (no test root of its own) only stays where it is.
    pub(super) fn permits_zone_join(&self, moving: u32, target: ClusterId) -> bool {
        self.is_pass_start_home(moving, target)
            || match (
                self.zones_by_scc.get(moving as usize),
                self.allowed_zones.get(target.0 as usize),
            ) {
                (Some(zones), Some(allowed)) => zones.is_subset(allowed),
                _ => false,
            }
    }
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;
    use strata_core::cluster::{ClusterId, Partition};

    use crate::analyze::relocation::identity_guard::RelocationIdentityGuard;
    use crate::analyze::relocation::{FileInfo, LaminarHome};
    use crate::analyze::test_support::singleton_condensation;

    fn info(path: &str) -> FileInfo {
        FileInfo {
            container: 0,
            name: SmolStr::new(path),
            production_sloc: 1,
            namespace: SmolStr::default(),
            home: LaminarHome {
                folder: SmolStr::default(),
                domain: SmolStr::default(),
                package: SmolStr::new("p"),
                synthetic: false,
            },
        }
    }

    /// Two production files and two tests, each alone in its pass-start folder.
    fn guard() -> RelocationIdentityGuard {
        let files = [
            info("src/a.ts"),
            info("src/b.ts"),
            info("tests/x.spec.ts"),
            info("tests/sub/y.spec.ts"),
        ];
        let identity = Partition::from_assignment((0..4).map(ClusterId).collect(), 4);
        let zones: Vec<SmolStr> = ["", "", "p:tests", "p:tests"]
            .iter()
            .map(SmolStr::new)
            .collect();
        RelocationIdentityGuard::new_with_zones(
            &files,
            &singleton_condensation(4),
            &identity,
            &identity,
            None,
            &zones,
        )
    }

    #[test]
    fn should_keep_production_out_of_a_test_folder() {
        let guard = guard();
        assert!(!guard.permits_zone_join(0, ClusterId(2)));
        assert!(guard.permits_zone_join(0, ClusterId(1)));
    }

    #[test]
    fn should_keep_an_unpaired_test_inside_its_test_root() {
        let guard = guard();
        assert!(guard.permits_zone_join(2, ClusterId(3)));
        assert!(!guard.permits_zone_join(2, ClusterId(0)));
        assert!(guard.permits_zone_join(2, ClusterId(2)));
    }
}
