//! Edition limits. The build picks the baseline; only a verified license
//! (`atlas_core::license`) can raise a Free build to Full, and only at runtime.
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    Free,
    Full,
}

/// Edition the binary was compiled for, before any license is applied.
pub const BUILD: Edition = if cfg!(feature = "free") {
    Edition::Free
} else {
    Edition::Full
};

static ENTITLED: AtomicBool = AtomicBool::new(false);

/// Effective edition: the build baseline, or Full once a license is verified.
pub fn current() -> Edition {
    if ENTITLED.load(Ordering::Relaxed) {
        Edition::Full
    } else {
        BUILD
    }
}

/// Only the license module calls this, after validating a Store or Polar entitlement.
pub fn set_entitled(entitled: bool) {
    ENTITLED.store(entitled, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    SavedConnections,
    Profiles,
    BatchRename,
    BatchConversion,
    /// Side-by-side panes (tab containers), not tabs.
    Panes,
    /// Total files converted today, including the operation being checked.
    ConversionsPerDay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub feature: Feature,
    pub maximum: usize,
}

impl Edition {
    /// `count` is the total after adding an item, or the entire operation size.
    /// Check before changing state; never truncate an operation to fit a limit.
    pub fn check(self, feature: Feature, count: usize) -> Result<(), Limit> {
        let maximum = match feature {
            Feature::SavedConnections | Feature::BatchConversion => 1,
            Feature::Profiles | Feature::Panes => 2,
            Feature::BatchRename => 25,
            Feature::ConversionsPerDay => 10,
        };
        if self == Self::Free && count > maximum {
            Err(Limit { feature, maximum })
        } else {
            Ok(())
        }
    }
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let feature = match self.feature {
            Feature::SavedConnections => "saved cloud connection(s)",
            Feature::Profiles => "profile(s)",
            Feature::BatchRename => "files per batch rename",
            Feature::BatchConversion => "file(s) per conversion",
            Feature::Panes => "side-by-side pane(s)",
            Feature::ConversionsPerDay => "conversion(s) per day",
        };
        write!(
            f,
            "Atlas Free allows up to {} {feature}. A paid edition is required for more.",
            self.maximum
        )
    }
}

impl std::error::Error for Limit {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_accepts_the_boundary_and_rejects_the_next_item() {
        for (feature, maximum) in [
            (Feature::SavedConnections, 1),
            (Feature::Profiles, 2),
            (Feature::BatchRename, 25),
            (Feature::BatchConversion, 1),
            (Feature::Panes, 2),
            (Feature::ConversionsPerDay, 10),
        ] {
            for count in [0, 1, maximum] {
                assert_eq!(Edition::Free.check(feature, count), Ok(()));
            }
            for count in [maximum + 1, usize::MAX] {
                assert_eq!(
                    Edition::Free.check(feature, count),
                    Err(Limit { feature, maximum })
                );
            }
            assert_eq!(Edition::Full.check(feature, usize::MAX), Ok(()));
        }
    }

    #[test]
    fn build_feature_selects_the_expected_policy() {
        assert_eq!(BUILD == Edition::Free, cfg!(feature = "free"));
        assert_eq!(
            BUILD.check(Feature::BatchConversion, 2).is_err(),
            cfg!(feature = "free")
        );
    }

    #[test]
    fn entitlement_raises_only_the_runtime_edition() {
        // Single test owns the global so parallel tests never observe it flipped.
        set_entitled(true);
        assert_eq!(current(), Edition::Full);
        assert!(current()
            .check(Feature::BatchConversion, usize::MAX)
            .is_ok());
        set_entitled(false);
        assert_eq!(current(), BUILD);
    }
}
