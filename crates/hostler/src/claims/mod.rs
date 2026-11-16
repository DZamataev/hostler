//! Claims on exclusive resources: catalogs, foreign sources, sharing (plan P3a, Task 4).

use std::collections::BTreeMap;

use crate::contract::claims::{
    ForeignClaim, ForeignView, Freshness, ResourceId, ResourceInfo, ResourceKind,
};

pub mod engine;
pub mod rules;
pub mod workspace_match;

/// The resources hostler arbitrates (P3b adds devices).
///
/// How often it is called (T5-4): once per claims tick (every 2 s), and once by each call that
/// decides on resources (`claims.acquire`, `claims.seize_begin` and `claims.seize_commit`,
/// `claims.reserve`); every other call reads the last collection. It runs under the claims
/// lock, so it must answer quickly and never block on I/O without a bound.
pub trait ResourceCatalog: Send + Sync {
    fn resources(&self) -> Vec<ResourceInfo>;
}

/// Claims hostler did not make (P3b: agent-device sessions). Called as often as
/// `ResourceCatalog::resources`, with the same rules.
pub trait ForeignClaimSource: Send + Sync {
    fn foreign(&self) -> Vec<ForeignClaim>;

    /// The claims and their freshness from one read (P3-2). A source that does not track claim
    /// ends answers `NotTracked`, so nothing waits for it.
    fn collect(&self) -> ForeignView {
        ForeignView {
            claims: self.foreign(),
            freshness: Freshness::NotTracked,
            unreadable: None,
        }
    }

    /// A device claim on `r` ended (P3-3): the source counts it and answers the new count, which
    /// a later `Freshness::Since` must reach. Untracked sources answer 0.
    fn claim_ended(&self, _r: &ResourceId) -> u64 {
        0
    }
}

/// The one desktop session of this machine.
pub struct ScreenCatalog;

impl ResourceCatalog for ScreenCatalog {
    fn resources(&self) -> Vec<ResourceInfo> {
        vec![ResourceInfo {
            id: ResourceId::screen(),
            kind: ResourceKind::Screen,
            display: "screen".into(),
            attributes: BTreeMap::new(),
            available: true,
        }]
    }
}

/// Accounts are not catalogued: any well-formed account id describes itself.
pub fn account_info(id: &ResourceId) -> Option<ResourceInfo> {
    (id.kind() == Some(ResourceKind::Account)).then(|| ResourceInfo {
        id: id.clone(),
        kind: ResourceKind::Account,
        display: format!("account {}", id.token()),
        attributes: BTreeMap::new(),
        available: true,
    })
}

/// L13: further runs and holds of a workspace join its device or account claim; the screen is
/// shared only through holds and re-entrant nesting.
pub fn shared_within_workspace(kind: ResourceKind) -> bool {
    match kind {
        ResourceKind::Device | ResourceKind::Account => true,
        ResourceKind::Screen => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_catalog_lists_the_screen() {
        let screen = ScreenCatalog.resources();
        assert_eq!(screen.len(), 1);
        assert_eq!(screen[0].id, ResourceId::screen());
        assert_eq!(screen[0].kind, ResourceKind::Screen);
        assert_eq!(screen[0].display, "screen");
        assert!(screen[0].available);
    }

    #[test]
    fn account_info_is_synthesized_for_account_ids_only() {
        let id = ResourceId::account("p-1", "qa-user");
        let info = account_info(&id).unwrap();
        assert_eq!(info.id, id);
        assert_eq!(info.kind, ResourceKind::Account);
        assert_eq!(info.display, "account qa-user");
        assert!(info.available);
        assert_eq!(account_info(&ResourceId::screen()), None);
        assert_eq!(account_info(&ResourceId("device:test:0".into())), None);
        assert_eq!(account_info(&ResourceId("account:p/BAD".into())), None);
    }

    struct Plain(Vec<ForeignClaim>);

    impl ForeignClaimSource for Plain {
        fn foreign(&self) -> Vec<ForeignClaim> {
            self.0.clone()
        }
    }

    /// A source that counts ends, as agent-device's will (Task 9b).
    struct Counting(std::sync::atomic::AtomicU64);

    impl ForeignClaimSource for Counting {
        fn foreign(&self) -> Vec<ForeignClaim> {
            Vec::new()
        }

        fn collect(&self) -> ForeignView {
            ForeignView {
                claims: Vec::new(),
                freshness: Freshness::Since(self.0.load(std::sync::atomic::Ordering::SeqCst)),
                unreadable: None,
            }
        }

        fn claim_ended(&self, _r: &ResourceId) -> u64 {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
        }
    }

    #[test]
    fn freshness_is_a_counter_and_collect_defaults_to_not_tracked() {
        let claim = ForeignClaim {
            resource: ResourceId("device:test:0".into()),
            source: "plain".into(),
            description: "d".into(),
            workspace_path: Some("/work/app".into()),
            since_ms: Some(1),
            stop: None,
            workspace_id: None,
        };
        let plain = Plain(vec![claim.clone()]);
        assert_eq!(
            plain.collect(),
            ForeignView {
                claims: vec![claim],
                freshness: Freshness::NotTracked,
                unreadable: None,
            }
        );
        let r = ResourceId("device:test:0".into());
        assert_eq!(plain.claim_ended(&r), 0);
        assert_eq!(plain.claim_ended(&r), 0);

        // `Since` carries a count of ends, never a time.
        let counting = Counting(std::sync::atomic::AtomicU64::new(0));
        assert_eq!(counting.collect().freshness, Freshness::Since(0));
        assert_eq!(counting.claim_ended(&r), 1);
        assert_eq!(counting.claim_ended(&r), 2);
        assert_eq!(counting.collect().freshness, Freshness::Since(2));
    }

    #[test]
    fn sharing_is_per_kind() {
        assert!(shared_within_workspace(ResourceKind::Device));
        assert!(shared_within_workspace(ResourceKind::Account));
        assert!(!shared_within_workspace(ResourceKind::Screen));
    }
}
