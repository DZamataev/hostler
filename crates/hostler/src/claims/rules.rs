//! Pure claim rules: queues, sharing, reaping and deadlock detection (plan P3a, Task 4; P3b
//! Task 9a: own agent-device sessions, the freshness gate, uncatalogued devices).
//!
//! Every function reads a [`State`] the engine assembles; none mutates it. `State.claims` holds
//! the live (not ended) claims only, so "a live linked claim" is any claim in it whose `parent`
//! names the claim at hand.

use std::collections::{HashMap, HashSet};

use super::{account_info, shared_within_workspace};
use crate::contract::HostlerError;
use crate::contract::claims::{
    Blocker, Claim, ClaimKind, ClaimSummary, EndReason, ForeignClaim, Freshness, HoldBinding,
    Purpose, ResourceId, ResourceInfo, ResourceKind, Selector,
};
use crate::contract::secrets::errors::bad_request;
use crate::procs::{ProcessIdentity, RunGroup};

/// A waiting claims request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub id: String,
    pub ticket: u64,
    pub selector: Selector,
    pub workspace_id: String,
    pub purpose: Purpose,
    pub requester: ProcessIdentity,
    /// The requester's pid first, then its ancestors.
    pub ancestry: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeavyRunning {
    pub id: String,
    pub holder: ProcessIdentity,
    pub child: Option<ProcessIdentity>,
    pub group: Option<RunGroup>,
    pub holder_ancestry: Vec<u32>,
}

/// What the deadlock check needs of the heavy queue. `capacity` counts the slots hostler's own
/// entries may use; foreign heavy work progresses on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeavyView {
    pub capacity: u8,
    pub running: Vec<HeavyRunning>,
    /// `(id, holder ancestry)` of every waiting entry.
    pub waiting: Vec<(String, Vec<u32>)>,
}

/// A resource frozen by a live takeover plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seizing {
    pub resource: ResourceId,
    /// The taker's workspace path, for `Blocker::Seizing`.
    pub by_workspace: String,
}

/// The last end of a claim on a device (P3b, L52, P3-3): the value the tracking source answered
/// to `claim_ended`, and the workspace whose claim ended. A device with no entry has `seq` 1 and
/// no workspace: the service just started, so nothing is known about its sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceEnd {
    pub resource: ResourceId,
    pub seq: u64,
    pub workspace_id: Option<String>,
    /// L54: a person released the device from this end's wait (`devices forget-sessions`).
    pub forgotten: bool,
}

impl DeviceEnd {
    /// The entry every device has at service start (P3-3).
    pub fn at_start(resource: ResourceId) -> DeviceEnd {
        DeviceEnd {
            resource,
            seq: 1,
            workspace_id: None,
            forgotten: false,
        }
    }
}

// Every ancestry here (Pending.ancestry, HeavyView) is computed by the engine with ONE
// `procs::parent_lookup()` per acquire or tick, through `procs::ancestors_with` (R2-7).
#[derive(Clone, Copy, Debug)]
pub struct State<'a> {
    pub claims: &'a [Claim],
    pub pending: &'a [Pending],
    pub foreign: &'a [ForeignClaim],
    pub catalog: &'a [ResourceInfo],
    pub seizing: &'a [Seizing],
    /// Taker requests named in live plans (R2-8).
    pub planned_requests: &'a [String],
    pub heavy: &'a HeavyView,
    /// The last end per device (P3b); a device missing here is as at start.
    pub device_ends: &'a [DeviceEnd],
    /// How fresh `foreign` is, from the same collection (P3-2).
    pub freshness: Freshness,
    /// Why agent-device's status cannot be read now (L54): the `Checking` text names the fix
    /// and `forget-sessions`.
    pub sessions_unreadable: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    Join {
        parent: String,
        resource: ResourceId,
    },
    Queue,
    /// `--distinct` with an `Exact` selector on a resource the workspace claims (R2-12).
    Refuse(HostlerError),
}

/// `Exact` → `[it]`; `Any` → the catalog's resources of that kind carrying every attribute, in
/// catalog order.
pub fn candidates(sel: &Selector, catalog: &[ResourceInfo]) -> Vec<ResourceId> {
    match sel {
        Selector::Exact { resource } => vec![resource.clone()],
        Selector::Any { kind, attributes } => catalog
            .iter()
            .filter(|info| {
                info.kind == *kind
                    && attributes
                        .iter()
                        .all(|(key, value)| info.attributes.get(key) == Some(value))
            })
            .map(|info| info.id.clone())
            .collect(),
    }
}

/// The catalog's entry, or a synthesized one for an account.
pub fn resource_info(id: &ResourceId, catalog: &[ResourceInfo]) -> Option<ResourceInfo> {
    catalog
        .iter()
        .find(|info| info.id == *id)
        .cloned()
        .or_else(|| account_info(id))
}

fn display(id: &ResourceId, catalog: &[ResourceInfo]) -> String {
    resource_info(id, catalog).map_or_else(|| id.0.clone(), |info| info.display)
}

fn on<'a>(r: &ResourceId, s: &State<'a>) -> impl Iterator<Item = &'a Claim> + use<'a> {
    let r = r.clone();
    s.claims.iter().filter(move |c| c.resource.id == r)
}

fn seizing<'a>(r: &ResourceId, s: &State<'a>) -> Option<&'a Seizing> {
    s.seizing.iter().find(|z| z.resource == *r)
}

/// A lease or hold of `w` on `r`. Reservations are not use: they only give precedence.
fn used_by(r: &ResourceId, w: &str, s: &State) -> bool {
    on(r, s).any(|c| c.workspace.id == w && c.kind != ClaimKind::Reservation)
}

fn reserved_by(r: &ResourceId, w: &str, s: &State) -> bool {
    on(r, s).any(|c| c.workspace.id == w && c.kind == ClaimKind::Reservation)
}

fn is_distinct(purpose: &Purpose) -> bool {
    matches!(purpose, Purpose::Hold { distinct: true, .. })
}

/// L50: a foreign claim (an agent-device session) blocks every workspace but its own. One whose
/// workspace is unknown blocks everyone.
fn foreign_blocks(f: &ForeignClaim, workspace_id: &str) -> bool {
    f.workspace_id.as_deref() != Some(workspace_id)
}

/// P2-5: a live foreign claim of `workspace_id` itself on `r` (its own agent-device session).
fn own_foreign<'a>(
    r: &ResourceId,
    workspace_id: &str,
    s: &State<'a>,
) -> impl Iterator<Item = &'a ForeignClaim> + use<'a> {
    let (r, w) = (r.clone(), workspace_id.to_owned());
    s.foreign
        .iter()
        .filter(move |f| f.resource == r && !foreign_blocks(f, &w))
}

/// A `Checking` blocker's text; while agent-device's status cannot be read it names the fix
/// and the person's escape (L54), from the first failed read (P3bT9b-2). The CLI's waiting line
/// adds the same words only when they are missing.
pub fn checking_text(display: &str, unreadable: Option<&str>) -> String {
    match unreadable {
        None => format!("checking for agent-device sessions on {display}"),
        Some(why) => format!(
            "checking for agent-device sessions on {display} (agent-device status unreadable: \
             {why}; {}; a person may release this device: hostler devices forget-sessions \
             {display})",
            crate::devices::agent_device::STATUS_FIX
        ),
    }
}

/// L52, L54: a device is not free for `workspace_id` until a good sessions read that started
/// after its last claim end, unless that end was this workspace's own or a person forgot it.
/// Until the first good read after a start (`NotYet`) it is free for nobody it was not forgotten
/// for. Only devices: no source tracks sessions on anything else.
fn closed_by_freshness(r: &ResourceId, workspace_id: &str, s: &State) -> bool {
    closed_for(r, Some(workspace_id), s)
}

/// Whether the freshness gate keeps `r` from another workspace now, whoever ended it: what
/// `forget-sessions` may release (L54).
pub fn closed_to_others(r: &ResourceId, s: &State) -> bool {
    closed_for(r, None, s)
}

/// `None`: for a workspace other than the ending one.
fn closed_for(r: &ResourceId, workspace_id: Option<&str>, s: &State) -> bool {
    if r.kind() != Some(ResourceKind::Device) {
        return false;
    }
    let start;
    let end = match s.device_ends.iter().find(|e| e.resource == *r) {
        Some(end) => end,
        None => {
            start = DeviceEnd::at_start(r.clone());
            &start
        }
    };
    let ender = workspace_id.is_some() && end.workspace_id.as_deref() == workspace_id;
    match s.freshness {
        Freshness::NotTracked => false,
        Freshness::NotYet => !end.forgotten,
        Freshness::Since(seq) => end.seq > seq && !ender && !end.forgotten,
    }
}

/// False when another workspace claims `r` (any kind), a foreign claim of another workspace is
/// on it (L50), it is being seized, the catalog lists it as unavailable, a device is missing
/// from the catalog (P-14), or the freshness gate closes it (L52). Claims of `w` itself are
/// `admit`'s business.
pub fn is_free_for(r: &ResourceId, workspace_id: &str, s: &State) -> bool {
    let listed = match s.catalog.iter().find(|info| info.id == *r) {
        Some(info) => info.available,
        // Accounts are not catalogued; a device that is not listed is gone or unregistered.
        None => r.kind() != Some(ResourceKind::Device),
    };
    !on(r, s).any(|c| c.workspace.id != workspace_id)
        && !s
            .foreign
            .iter()
            .any(|f| f.resource == *r && foreign_blocks(f, workspace_id))
        && seizing(r, s).is_none()
        && listed
        && !closed_by_freshness(r, workspace_id, s)
}

/// The claim of `req`'s workspace on `r` that `req` would join: never for a distinct hold or on
/// a seizing resource (L17); a hold of any kind before a lease of a shared kind; the oldest
/// first (a root predates the claims linked to it).
fn joinable<'a>(req: &Pending, r: &ResourceId, s: &State<'a>) -> Option<&'a Claim> {
    if is_distinct(&req.purpose) || seizing(r, s).is_some() {
        return None;
    }
    on(r, s)
        .filter(|c| c.workspace.id == req.workspace_id)
        .filter(|c| match c.kind {
            ClaimKind::Hold => true,
            ClaimKind::Lease => shared_within_workspace(c.resource.kind),
            ClaimKind::Reservation => false,
        })
        .min_by_key(|c| (c.kind != ClaimKind::Hold, c.granted_at_ms, c.id.as_str()))
}

/// Precedence step 2 (step 1, re-entrancy, is the engine's). The engine calls it at acquire and,
/// for every pending request, on each tick before `grants`: a request that queued before its
/// workspace got the resource joins once the workspace has it.
pub fn admit(req: &Pending, s: &State) -> Admission {
    if is_distinct(&req.purpose) {
        if let Selector::Exact { resource } = &req.selector
            && used_by(resource, &req.workspace_id, s)
        {
            return Admission::Refuse(bad_request(format!(
                "{} is already claimed by this workspace; --distinct needs a resource it does \
                 not hold",
                display(resource, s.catalog)
            )));
        }
        return Admission::Queue;
    }
    let cands = candidates(&req.selector, s.catalog);
    let joins: Vec<&Claim> = cands.iter().filter_map(|r| joinable(req, r, s)).collect();
    // Holds before leases across the whole set, each in candidate order.
    joins
        .iter()
        .find(|c| c.kind == ClaimKind::Hold)
        .or_else(|| joins.first())
        .map_or(Admission::Queue, |c| Admission::Join {
            parent: c.id.clone(),
            resource: c.resource.id.clone(),
        })
}

/// One pass of FIFO grants over every resource. The earliest ticket picks first among the
/// candidates it may use (its workspace's reservations first, then catalog order); a resource is
/// given once per pass; requests named in a live plan are skipped (R2-8).
pub fn grants(s: &State) -> Vec<(String, ResourceId)> {
    let mut order: Vec<&Pending> = s
        .pending
        .iter()
        .filter(|p| !s.planned_requests.contains(&p.id))
        .collect();
    order.sort_by(|a, b| (a.ticket, &a.id).cmp(&(b.ticket, &b.id)));
    let mut taken: HashSet<ResourceId> = HashSet::new();
    let mut out = Vec::new();
    for req in order {
        let w = req.workspace_id.as_str();
        let distinct = is_distinct(&req.purpose);
        let mut cands = candidates(&req.selector, s.catalog);
        // Its reservations first; then (P2-5) a plain `auto` prefers the devices carrying its
        // own session: that is the device its agent works on. Otherwise catalog order.
        cands.sort_by_key(|r| {
            (
                !reserved_by(r, w, s),
                distinct || own_foreign(r, w, s).next().is_none(),
            )
        });
        // A resource `w` uses is either joined (admit) or, for the screen and distinct holds,
        // waited for. A distinct hold also counts a device with its own session as used (P2-5):
        // agent-device lets one session hold a device, so another slot's would be refused.
        let pick = cands.into_iter().find(|r| {
            !taken.contains(r)
                && is_free_for(r, w, s)
                && !used_by(r, w, s)
                && !(distinct && own_foreign(r, w, s).next().is_some())
        });
        if let Some(r) = pick {
            taken.insert(r.clone());
            out.push((req.id.clone(), r));
        }
    }
    out
}

/// The 1-based place of `req_id` among the pending requests whose candidate sets intersect its
/// own, and how many those are (itself included).
pub fn position(req_id: &str, s: &State) -> Option<(u32, u32)> {
    let req = s.pending.iter().find(|p| p.id == req_id)?;
    let mine: HashSet<ResourceId> = candidates(&req.selector, s.catalog).into_iter().collect();
    let rivals: Vec<&Pending> = s
        .pending
        .iter()
        .filter(|p| p.id != req.id)
        .filter(|p| {
            candidates(&p.selector, s.catalog)
                .iter()
                .any(|r| mine.contains(r))
        })
        .collect();
    let ahead = rivals
        .iter()
        .filter(|p| (p.ticket, &p.id) < (req.ticket, &req.id))
        .count();
    Some((ahead as u32 + 1, rivals.len() as u32 + 1))
}

/// The claims that keep `req` off `r`: every claim of another workspace, and its own workspace's
/// leases and holds unless `req` may join. Empty when `r` is free for it or joinable.
fn blocking<'a>(req: &Pending, r: &ResourceId, s: &State<'a>) -> Vec<&'a Claim> {
    if joinable(req, r, s).is_some() {
        return Vec::new();
    }
    on(r, s)
        .filter(|c| c.workspace.id != req.workspace_id || c.kind != ClaimKind::Reservation)
        .collect()
}

/// The text of a distinct request's own session (P3-12): it waits for that session to close.
/// The `stop` data is the agent-device source's (`session`, `platform`, `id`).
fn own_session_text(f: &ForeignClaim) -> String {
    let field = |key: &str| {
        f.stop
            .as_ref()
            .and_then(|stop| stop.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    let session = field("session").unwrap_or_else(|| "?".into());
    let close = match (field("platform"), field("id")) {
        (Some(platform), Some(id)) => {
            let flag = if platform == "ios" { "udid" } else { "serial" };
            format!("agent-device close --session {session} --platform {platform} --{flag} {id}")
        }
        _ => format!("agent-device close --session {session}"),
    };
    format!("your own agent-device session {session}; close it: {close}")
}

/// What `req` waits for, per candidate: the seizure, foreign claims of other workspaces (and, for
/// a distinct request, its own sessions, P3-12), the root claims among its blockers (linked ones
/// are counted in `ClaimSummary.linked`), and the freshness gate (L52).
pub fn blockers(req_id: &str, s: &State) -> Vec<Blocker> {
    let Some(req) = s.pending.iter().find(|p| p.id == req_id) else {
        return Vec::new();
    };
    let w = req.workspace_id.as_str();
    let distinct = is_distinct(&req.purpose);
    let mut out = Vec::new();
    for r in candidates(&req.selector, s.catalog) {
        if let Some(z) = seizing(&r, s) {
            out.push(Blocker::Seizing {
                by_workspace: z.by_workspace.clone(),
            });
        }
        for f in s.foreign.iter().filter(|f| f.resource == r) {
            if foreign_blocks(f, w) {
                out.push(Blocker::Foreign { claim: f.clone() });
            } else if distinct {
                out.push(Blocker::Foreign {
                    claim: ForeignClaim {
                        description: own_session_text(f),
                        ..f.clone()
                    },
                });
            }
        }
        let claims = blocking(req, &r, s);
        out.extend(
            claims
                .iter()
                .filter(|c| {
                    !c.parent
                        .as_ref()
                        .is_some_and(|p| claims.iter().any(|b| b.id == *p))
                })
                .map(|c| Blocker::Claim {
                    summary: summary(c, s.claims),
                }),
        );
        if closed_by_freshness(&r, w, s) {
            let display = display(&r, s.catalog);
            out.push(Blocker::Checking {
                text: checking_text(&display, s.sessions_unreadable),
                resource: display,
            });
        }
    }
    out
}

pub fn summary(c: &Claim, claims: &[Claim]) -> ClaimSummary {
    let (pid, expires_at_ms) = match &c.binding {
        Some(HoldBinding::Process { holder }) => (Some(holder.pid), None),
        // The holder of a TTL hold is the `hostler hold` command, gone after the grant.
        Some(HoldBinding::Ttl { expires_at_ms, .. }) => (None, Some(*expires_at_ms)),
        None => (c.holder.as_ref().map(|h| h.pid), None),
    };
    ClaimSummary {
        id: c.id.clone(),
        resource: c.resource.display.clone(),
        kind: c.kind,
        workspace_path: c.workspace.path.clone(),
        pid,
        command: c.command.clone(),
        since_ms: c.granted_at_ms,
        expires_at_ms,
        linked: claims
            .iter()
            .filter(|l| l.parent.as_deref() == Some(c.id.as_str()))
            .count() as u32,
    }
}

fn has_linked(c: &Claim, claims: &[Claim], ended: &HashSet<&str>) -> bool {
    claims
        .iter()
        .any(|l| l.parent.as_deref() == Some(c.id.as_str()) && !ended.contains(l.id.as_str()))
}

/// TTL holds at or past `expires_at_ms` with no live linked claim.
pub fn expired(claims: &[Claim], now_ms: i64) -> Vec<String> {
    claims
        .iter()
        .filter(|c| c.kind == ClaimKind::Hold)
        .filter(|c| {
            matches!(c.binding, Some(HoldBinding::Ttl { expires_at_ms, .. }) if now_ms >= expires_at_ms)
        })
        .filter(|c| !has_linked(c, claims, &HashSet::new()))
        .map(|c| c.id.clone())
        .collect()
}

/// P1's lease liveness: its holder, its child or its group lives.
pub fn lease_alive(
    c: &Claim,
    live: &dyn Fn(&ProcessIdentity) -> bool,
    group_live: &dyn Fn(&RunGroup) -> bool,
) -> bool {
    c.holder.as_ref().is_some_and(live)
        || c.child.as_ref().is_some_and(live)
        || c.group.as_ref().is_some_and(group_live)
}

/// Claim and request ids to end, in this order:
/// 1. pending requests whose requester (or, for a process-bound hold, the bound process) is dead
///    → `Reaped`;
/// 2. leases where `lease_live` is false → `Reaped`;
/// 3. process-bound holds whose process is dead → `Reaped`;
/// 4. TTL holds and reservations whose workspace path is gone and that have no live linked
///    claim (claims ended above no longer count) → `Stranded`. A claim with a live process is
///    never stranded by its path (L41): a lease, or a process-bound hold whose process lives
///    (a dead one was reaped in 3). The engine reports a path gone only after 2 minutes.
pub fn reapable(
    claims: &[Claim],
    pending: &[Pending],
    live: &dyn Fn(&ProcessIdentity) -> bool,
    lease_live: &dyn Fn(&Claim) -> bool,
    path_exists: &dyn Fn(&str) -> bool,
) -> Vec<(String, EndReason)> {
    let mut out: Vec<(String, EndReason)> = pending
        .iter()
        .filter(|p| {
            !live(&p.requester)
                || matches!(&p.purpose, Purpose::Hold { bound_to: Some(b), .. } if !live(b))
        })
        .map(|p| (p.id.clone(), EndReason::Reaped))
        .collect();
    let mut ended: HashSet<&str> = HashSet::new();
    for c in claims {
        let dead = match (&c.kind, &c.binding) {
            (ClaimKind::Lease, _) => !lease_live(c),
            (ClaimKind::Hold, Some(HoldBinding::Process { holder })) => !live(holder),
            _ => false,
        };
        if dead {
            ended.insert(&c.id);
            out.push((c.id.clone(), EndReason::Reaped));
        }
    }
    // A stranded linked hold frees its parent in the same pass.
    loop {
        let stranded: Vec<&Claim> = claims
            .iter()
            .filter(|c| matches!(c.kind, ClaimKind::Hold | ClaimKind::Reservation))
            .filter(|c| !matches!(c.binding, Some(HoldBinding::Process { .. })))
            .filter(|c| !ended.contains(c.id.as_str()) && !path_exists(&c.workspace.path))
            .filter(|c| !has_linked(c, claims, &ended))
            .collect();
        if stranded.is_empty() {
            return out;
        }
        for c in stranded {
            ended.insert(&c.id);
            out.push((c.id.clone(), EndReason::Stranded));
        }
    }
}

/// The processes whose waiting keeps `c` alive: a lease's holder and child, a process-bound
/// hold's process. Empty for a TTL hold or reservation: those end on their own once no linked
/// claim lives. L20 needs no walk here: a linked claim sits on its parent's resource, so it is
/// itself one of the blockers `deadlocked` requires to end.
fn procs(c: &Claim) -> Vec<u32> {
    match (&c.kind, &c.binding) {
        (ClaimKind::Lease, _) => c.holder.iter().chain(&c.child).map(|p| p.pid).collect(),
        (ClaimKind::Hold, Some(HoldBinding::Process { holder })) => vec![holder.pid],
        _ => Vec::new(),
    }
}

/// The claims requests that can never be granted, and the ones among them to refuse.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Deadlock {
    /// Every request in `S`, youngest (highest ticket) first.
    pub stuck: Vec<String>,
    /// The members of `S` on a cycle of its wait-for graph (a self-wait included), youngest
    /// first. In that graph a request points at every member whose ancestry holds a process it
    /// waits for: a blocker's on its candidates, or, for a stuck heavy waiter, a running heavy
    /// entry's. The others in `stuck` only queue behind a cycle, directly or down a chain, so the
    /// engine refuses a request at acquire only when it is a victim, and on a tick the first
    /// victim (L25). No victim (the heavy queue deadlocked on its own): nothing to refuse.
    pub victims: Vec<String>,
}

/// The greatest set `S` of waiting requests (claims and heavy) such that every claims request in
/// `S` has, on each candidate, a blocking claim with a process that is (an ancestor of) a member
/// of `S`, and every heavy request in `S` finds `capacity` running entries with such processes.
/// Requests in a live plan, candidates being seized, and empty candidate sets (a device may
/// appear) progress.
/// Complete: a request outside `S` has a candidate whose every blocker can end, or a free slot.
pub fn deadlocked(s: &State) -> Deadlock {
    // Per request and candidate, the processes of each blocker that can only end when they stop
    // waiting. A candidate with none of those can free up: the request is never in `S`.
    let mut claims_s: Vec<(&Pending, Vec<Vec<Vec<u32>>>)> = s
        .pending
        .iter()
        .filter(|p| !s.planned_requests.contains(&p.id))
        .filter_map(|q| {
            let mut per_candidate = Vec::new();
            for r in candidates(&q.selector, s.catalog) {
                if seizing(&r, s).is_some() {
                    return None;
                }
                let pids: Vec<Vec<u32>> = blocking(q, &r, s)
                    .into_iter()
                    .map(procs)
                    .filter(|pids| !pids.is_empty())
                    .collect();
                if pids.is_empty() {
                    return None;
                }
                per_candidate.push(pids);
            }
            (!per_candidate.is_empty()).then_some((q, per_candidate))
        })
        .collect();
    let mut heavy_s: Vec<&Vec<u32>> = s.heavy.waiting.iter().map(|(_, a)| a).collect();
    let running: Vec<Vec<u32>> = s
        .heavy
        .running
        .iter()
        .map(|r| {
            std::iter::once(&r.holder)
                .chain(&r.child)
                .map(|p| p.pid)
                .collect()
        })
        .collect();
    loop {
        let waiting: HashSet<u32> = claims_s
            .iter()
            .flat_map(|(q, _)| q.ancestry.iter())
            .chain(heavy_s.iter().flat_map(|a| a.iter()))
            .copied()
            .collect();
        let stuck = |pids: &Vec<u32>| pids.iter().any(|p| waiting.contains(p));
        let before = (claims_s.len(), heavy_s.len());
        // A request stays while each candidate keeps a blocker that waits on `S`.
        claims_s.retain(|(_, per_candidate)| per_candidate.iter().all(|b| b.iter().any(stuck)));
        // A slot frees once fewer than `capacity` running entries are stuck; running may exceed
        // a capacity lowered at runtime.
        if running.iter().filter(|pids| stuck(pids)).count() < usize::from(s.heavy.capacity) {
            heavy_s.clear();
        }
        if (claims_s.len(), heavy_s.len()) == before {
            break;
        }
    }
    // The wait-for graph inside `S`: the stuck claims requests, then the stuck heavy waiters. A
    // node points at every node whose ancestry holds a process it waits for.
    let waits_on: Vec<HashSet<u32>> = claims_s
        .iter()
        .map(|(_, per_candidate)| per_candidate.iter().flatten().flatten().copied().collect())
        .chain(
            heavy_s
                .iter()
                .map(|_| running.iter().flatten().copied().collect()),
        )
        .collect();
    let mut holding: HashMap<u32, Vec<usize>> = HashMap::new();
    for (j, ancestry) in claims_s
        .iter()
        .map(|(q, _)| &q.ancestry)
        .chain(heavy_s.iter().copied())
        .enumerate()
    {
        for &pid in ancestry {
            holding.entry(pid).or_default().push(j);
        }
    }
    let edges: Vec<Vec<usize>> = waits_on
        .iter()
        .map(|pids| {
            let mut to: Vec<usize> = pids
                .iter()
                .filter_map(|pid| holding.get(pid))
                .flatten()
                .copied()
                .collect();
            to.sort_unstable();
            to.dedup();
            to
        })
        .collect();
    let cyclic = on_cycles(&edges);
    let victims: HashSet<&str> = (0..claims_s.len())
        .filter(|&i| cyclic[i])
        .map(|i| claims_s[i].0.id.as_str())
        .collect();
    let mut left: Vec<&Pending> = claims_s.iter().map(|(q, _)| *q).collect();
    left.sort_by(|a, b| (b.ticket, &b.id).cmp(&(a.ticket, &a.id)));
    Deadlock {
        stuck: left.iter().map(|p| p.id.clone()).collect(),
        victims: left
            .iter()
            .filter(|p| victims.contains(p.id.as_str()))
            .map(|p| p.id.clone())
            .collect(),
    }
}

/// For each node, whether it lies on a cycle: in a strongly connected component of more than one
/// node, or with an edge to itself. Tarjan's algorithm without recursion, O(V + E).
fn on_cycles(edges: &[Vec<usize>]) -> Vec<bool> {
    const UNSEEN: usize = usize::MAX;
    let n = edges.len();
    let (mut index, mut low) = (vec![UNSEEN; n], vec![0; n]);
    let mut on_stack = vec![false; n];
    let (mut stack, mut cyclic, mut next) = (Vec::new(), vec![false; n], 0);
    for root in 0..n {
        if index[root] != UNSEEN {
            continue;
        }
        // (node, next edge to follow)
        let mut calls = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&(v, i)) = calls.last() {
            if let Some(&w) = edges[v].get(i) {
                calls.last_mut().expect("a call").1 += 1;
                if index[w] == UNSEEN {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    calls.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            calls.pop();
            if let Some(&(u, _)) = calls.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == index[v] {
                let mut component = Vec::new();
                loop {
                    let w = stack.pop().expect("v is on the stack");
                    on_stack[w] = false;
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                let cycle = component.len() > 1 || edges[v].contains(&v);
                for w in component {
                    cyclic[w] = cycle;
                }
            }
        }
    }
    cyclic
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::claims::ResourceCatalog;
    use crate::contract::ErrorCode;
    use crate::contract::claims::{ResourceKind, WorkspaceRef};

    fn ident(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            boot_id: "boot-1".into(),
            start_time: u64::from(pid) * 10,
        }
    }

    fn screen() -> ResourceInfo {
        crate::claims::ScreenCatalog
            .resources()
            .into_iter()
            .next()
            .unwrap()
    }

    fn dev(i: u32) -> ResourceInfo {
        ResourceInfo {
            id: ResourceId(format!("device:test:{i}")),
            kind: ResourceKind::Device,
            display: format!("fake-{i}"),
            attributes: BTreeMap::from([("test".to_owned(), "1".to_owned())]),
            available: true,
        }
    }

    fn acct(name: &str) -> ResourceInfo {
        account_info(&ResourceId::account("p-1", name)).unwrap()
    }

    /// The screen and `n` fake devices, as the engine's catalogs give them.
    fn catalog(n: u32) -> Vec<ResourceInfo> {
        std::iter::once(screen()).chain((0..n).map(dev)).collect()
    }

    fn claim(id: &str, res: &ResourceInfo, ws: &str, kind: ClaimKind) -> Claim {
        Claim {
            id: id.into(),
            resource: res.clone(),
            kind,
            parent: None,
            workspace: WorkspaceRef {
                id: ws.into(),
                path: format!("/work/{ws}"),
            },
            holder: None,
            child: None,
            group: None,
            binding: None,
            command: None,
            granted_at_ms: 1_000,
        }
    }

    fn lease(id: &str, res: &ResourceInfo, ws: &str, holder: u32, child: Option<u32>) -> Claim {
        Claim {
            holder: Some(ident(holder)),
            child: child.map(ident),
            command: Some("make test".into()),
            ..claim(id, res, ws, ClaimKind::Lease)
        }
    }

    fn ttl_hold(id: &str, res: &ResourceInfo, ws: &str, expires_at_ms: i64) -> Claim {
        Claim {
            holder: Some(ident(900)),
            binding: Some(HoldBinding::Ttl {
                ttl_ms: 1_800_000,
                expires_at_ms,
            }),
            ..claim(id, res, ws, ClaimKind::Hold)
        }
    }

    fn bound_hold(id: &str, res: &ResourceInfo, ws: &str, pid: u32) -> Claim {
        Claim {
            holder: Some(ident(901)),
            binding: Some(HoldBinding::Process { holder: ident(pid) }),
            ..claim(id, res, ws, ClaimKind::Hold)
        }
    }

    fn reservation(id: &str, res: &ResourceInfo, ws: &str) -> Claim {
        claim(id, res, ws, ClaimKind::Reservation)
    }

    fn linked(mut c: Claim, parent: &str) -> Claim {
        c.parent = Some(parent.into());
        c
    }

    fn exact(res: &ResourceInfo) -> Selector {
        Selector::Exact {
            resource: res.id.clone(),
        }
    }

    fn any_device() -> Selector {
        Selector::Any {
            kind: ResourceKind::Device,
            attributes: BTreeMap::from([("test".to_owned(), "1".to_owned())]),
        }
    }

    fn lease_purpose() -> Purpose {
        Purpose::Lease {
            command: "make test".into(),
        }
    }

    fn hold_purpose(distinct: bool) -> Purpose {
        Purpose::Hold {
            ttl_ms: Some(1_800_000),
            bound_to: None,
            distinct,
        }
    }

    /// `ancestry[0]` is the requester.
    fn req(
        id: &str,
        ticket: u64,
        sel: Selector,
        ws: &str,
        purpose: Purpose,
        ancestry: &[u32],
    ) -> Pending {
        Pending {
            id: id.into(),
            ticket,
            selector: sel,
            workspace_id: ws.into(),
            purpose,
            requester: ident(ancestry[0]),
            ancestry: ancestry.to_vec(),
        }
    }

    fn run(id: &str, ticket: u64, sel: Selector, ws: &str, ancestry: &[u32]) -> Pending {
        req(id, ticket, sel, ws, lease_purpose(), ancestry)
    }

    struct World {
        claims: Vec<Claim>,
        pending: Vec<Pending>,
        foreign: Vec<ForeignClaim>,
        catalog: Vec<ResourceInfo>,
        seizing: Vec<Seizing>,
        planned: Vec<String>,
        heavy: HeavyView,
        device_ends: Vec<DeviceEnd>,
        freshness: Freshness,
        unreadable: Option<String>,
    }

    impl World {
        fn new(catalog: Vec<ResourceInfo>) -> Self {
            World {
                claims: Vec::new(),
                pending: Vec::new(),
                foreign: Vec::new(),
                catalog,
                seizing: Vec::new(),
                planned: Vec::new(),
                heavy: HeavyView {
                    capacity: 1,
                    running: Vec::new(),
                    waiting: Vec::new(),
                },
                device_ends: Vec::new(),
                freshness: Freshness::NotTracked,
                unreadable: None,
            }
        }

        fn state(&self) -> State<'_> {
            State {
                claims: &self.claims,
                pending: &self.pending,
                foreign: &self.foreign,
                catalog: &self.catalog,
                seizing: &self.seizing,
                planned_requests: &self.planned,
                heavy: &self.heavy,
                device_ends: &self.device_ends,
                freshness: self.freshness,
                sessions_unreadable: self.unreadable.as_deref(),
            }
        }

        fn grants(&self) -> Vec<(String, ResourceId)> {
            grants(&self.state())
        }

        fn admit(&self, id: &str) -> Admission {
            admit(
                self.pending.iter().find(|p| p.id == id).unwrap(),
                &self.state(),
            )
        }

        fn deadlocked(&self) -> Vec<String> {
            deadlocked(&self.state()).stuck
        }

        fn victims(&self) -> Vec<String> {
            deadlocked(&self.state()).victims
        }

        fn drop_claim(&mut self, id: &str) {
            self.claims.retain(|c| c.id != id);
        }

        /// Applies one pass of grants as the engine would: each request becomes a claim.
        fn apply_grants(&mut self) -> Vec<(String, ResourceId)> {
            let granted = self.grants();
            for (id, r) in &granted {
                let p = self.pending.iter().find(|p| p.id == *id).unwrap().clone();
                let info = resource_info(r, &self.catalog).unwrap();
                let kind = match p.purpose {
                    Purpose::Lease { .. } => ClaimKind::Lease,
                    Purpose::Hold { .. } => ClaimKind::Hold,
                };
                self.claims.push(Claim {
                    holder: Some(p.requester.clone()),
                    ..claim(id, &info, &p.workspace_id, kind)
                });
                self.pending.retain(|q| q.id != *id);
            }
            granted
        }
    }

    fn g(pairs: &[(&str, &ResourceInfo)]) -> Vec<(String, ResourceId)> {
        pairs
            .iter()
            .map(|(id, r)| ((*id).to_owned(), r.id.clone()))
            .collect()
    }

    fn heavy_running(id: &str, holder: u32, child: Option<u32>) -> HeavyRunning {
        HeavyRunning {
            id: id.into(),
            holder: ident(holder),
            child: child.map(ident),
            group: None,
            holder_ancestry: vec![holder],
        }
    }

    // ---- FIFO and sets ----

    #[test]
    fn exact_requests_are_fifo_per_resource() {
        let mut w = World::new(catalog(0));
        w.claims.push(lease("l1", &screen(), "w1", 10, None));
        w.pending.push(run("r3", 3, exact(&screen()), "w3", &[30]));
        w.pending.push(run("r2", 2, exact(&screen()), "w2", &[20]));
        assert!(w.grants().is_empty());
        w.drop_claim("l1");
        assert_eq!(w.grants(), g(&[("r2", &screen())]));
        w.apply_grants();
        assert!(w.grants().is_empty(), "r3 waits behind r2's new lease");
        w.drop_claim("r2");
        assert_eq!(w.grants(), g(&[("r3", &screen())]));
    }

    #[test]
    fn any_request_takes_first_free_candidate() {
        let mut w = World::new(catalog(3));
        w.claims.push(lease("l0", &dev(0), "w2", 20, None));
        w.pending.push(run("r1", 1, any_device(), "w1", &[10]));
        assert_eq!(w.grants(), g(&[("r1", &dev(1))]));
    }

    #[test]
    fn any_request_waits_on_the_whole_set_and_takes_whichever_frees_first() {
        let mut w = World::new(catalog(2));
        w.claims.push(lease("l0", &dev(0), "w2", 20, None));
        w.claims.push(lease("l1", &dev(1), "w3", 30, None));
        w.pending.push(run("r1", 1, any_device(), "w1", &[10]));
        w.pending.push(run("r2", 2, exact(&dev(1)), "w4", &[40]));
        assert!(w.grants().is_empty());
        w.drop_claim("l1");
        assert_eq!(
            w.grants(),
            g(&[("r1", &dev(1))]),
            "the earlier set request first"
        );
    }

    #[test]
    fn kinds_do_not_block_each_other() {
        let mut w = World::new(catalog(1));
        w.claims.push(lease("l0", &screen(), "w2", 20, None));
        w.pending.push(run("r1", 1, exact(&screen()), "w1", &[10]));
        w.pending
            .push(run("r2", 2, exact(&acct("qa")), "w1", &[11]));
        w.pending.push(run("r3", 3, any_device(), "w3", &[30]));
        assert_eq!(w.grants(), g(&[("r2", &acct("qa")), ("r3", &dev(0))]));
    }

    #[test]
    fn earlier_any_can_take_a_later_exacts_resource() {
        let mut w = World::new(catalog(2));
        w.claims.push(lease("l1", &dev(1), "w3", 30, None));
        w.pending.push(run("r2", 2, exact(&dev(0)), "w2", &[20]));
        w.pending.push(run("r1", 1, any_device(), "w1", &[10]));
        assert_eq!(w.grants(), g(&[("r1", &dev(0))]));
    }

    #[test]
    fn no_double_grant_in_one_pass() {
        let mut w = World::new(catalog(2));
        w.pending.push(run("r1", 1, any_device(), "w1", &[10]));
        w.pending.push(run("r2", 2, exact(&dev(0)), "w2", &[20]));
        w.pending.push(run("r3", 3, any_device(), "w3", &[30]));
        w.pending.push(run("r4", 4, any_device(), "w4", &[40]));
        assert_eq!(w.grants(), g(&[("r1", &dev(0)), ("r3", &dev(1))]));
    }

    // ---- Joining and sharing ----

    #[test]
    fn lease_joins_own_hold_of_any_kind() {
        for res in [screen(), acct("qa"), dev(0)] {
            let mut w = World::new(catalog(1));
            w.claims.push(ttl_hold("h1", &res, "w1", 9_000));
            w.pending.push(run("r1", 1, exact(&res), "w1", &[10]));
            assert_eq!(
                w.admit("r1"),
                Admission::Join {
                    parent: "h1".into(),
                    resource: res.id.clone()
                },
                "{}",
                res.id.0
            );
        }
    }

    #[test]
    fn lease_joins_own_device_lease() {
        let mut w = World::new(catalog(2));
        w.claims.push(lease("l1", &dev(1), "w1", 10, Some(11)));
        w.pending.push(run("r1", 1, any_device(), "w1", &[12]));
        assert_eq!(
            w.admit("r1"),
            Admission::Join {
                parent: "l1".into(),
                resource: dev(1).id
            }
        );
        // Another workspace's run is not admitted; it queues and takes the free device.
        w.pending.push(run("r2", 2, any_device(), "w2", &[20]));
        assert_eq!(w.admit("r2"), Admission::Queue);
    }

    #[test]
    fn lease_joins_own_account_lease() {
        let mut w = World::new(catalog(0));
        w.claims.push(lease("l1", &acct("qa"), "w1", 10, None));
        w.pending
            .push(run("r1", 1, exact(&acct("qa")), "w1", &[12]));
        assert_eq!(
            w.admit("r1"),
            Admission::Join {
                parent: "l1".into(),
                resource: acct("qa").id
            }
        );
    }

    #[test]
    fn join_prefers_a_hold_then_the_oldest() {
        let mut w = World::new(catalog(2));
        w.claims.push(lease("l0", &dev(0), "w1", 10, None));
        w.claims.push(ttl_hold("h1", &dev(1), "w1", 9_000));
        w.claims.push(Claim {
            granted_at_ms: 2_000,
            ..linked(ttl_hold("h1b", &dev(1), "w1", 9_000), "h1")
        });
        w.pending.push(run("r1", 1, any_device(), "w1", &[12]));
        assert_eq!(
            w.admit("r1"),
            Admission::Join {
                parent: "h1".into(),
                resource: dev(1).id
            }
        );
    }

    #[test]
    fn screen_lease_beside_own_screen_lease_queues() {
        let mut w = World::new(catalog(0));
        w.claims.push(lease("l1", &screen(), "w1", 10, None));
        w.pending.push(run("r1", 1, exact(&screen()), "w1", &[20]));
        assert_eq!(w.admit("r1"), Admission::Queue);
        assert!(w.grants().is_empty());
        w.drop_claim("l1");
        assert_eq!(w.grants(), g(&[("r1", &screen())]));
        // A non-distinct screen hold does not join a screen lease either.
        let mut w = World::new(catalog(0));
        w.claims.push(lease("l1", &screen(), "w1", 10, None));
        w.pending.push(req(
            "h",
            1,
            exact(&screen()),
            "w1",
            hold_purpose(false),
            &[20],
        ));
        assert_eq!(w.admit("h"), Admission::Queue);
    }

    #[test]
    fn hold_joins_own_device_claim_unless_distinct() {
        let mut w = World::new(catalog(2));
        w.claims.push(lease("l0", &dev(0), "w1", 10, None));
        w.pending.push(req(
            "h1",
            1,
            exact(&dev(0)),
            "w1",
            hold_purpose(false),
            &[20],
        ));
        w.pending
            .push(req("h2", 2, any_device(), "w1", hold_purpose(true), &[21]));
        assert_eq!(
            w.admit("h1"),
            Admission::Join {
                parent: "l0".into(),
                resource: dev(0).id
            }
        );
        assert_eq!(w.admit("h2"), Admission::Queue);
        w.pending.retain(|p| p.id == "h2");
        assert_eq!(w.grants(), g(&[("h2", &dev(1))]));
    }

    #[test]
    fn distinct_holds_from_one_workspace_get_distinct_resources() {
        let mut w = World::new(catalog(3));
        // A non-distinct hold of the workspace already sits on fake-1.
        w.claims.push(ttl_hold("h0", &dev(1), "w1", 9_000));
        for i in 1..=3 {
            w.pending.push(req(
                &format!("d{i}"),
                i,
                any_device(),
                "w1",
                hold_purpose(true),
                &[10],
            ));
        }
        assert_eq!(w.apply_grants(), g(&[("d1", &dev(0)), ("d2", &dev(2))]));
        assert!(
            w.apply_grants().is_empty(),
            "d3 waits: every device is the workspace's"
        );
        w.drop_claim("d2");
        assert_eq!(w.apply_grants(), g(&[("d3", &dev(2))]));
        let mut held: Vec<&ResourceId> = w.claims.iter().map(|c| &c.resource.id).collect();
        held.sort();
        held.dedup();
        assert_eq!(
            held.len(),
            w.claims.len(),
            "no two claims of w1 on one device"
        );
    }

    #[test]
    fn distinct_exact_on_own_claim_is_refused() {
        for own in [
            lease("l0", &dev(0), "w1", 10, None),
            ttl_hold("l0", &dev(0), "w1", 9_000),
        ] {
            let mut w = World::new(catalog(1));
            w.claims.push(own);
            w.pending
                .push(req("d", 1, exact(&dev(0)), "w1", hold_purpose(true), &[20]));
            let Admission::Refuse(e) = w.admit("d") else {
                panic!("not refused");
            };
            assert_eq!(e.code, ErrorCode::BadRequest);
            assert_eq!(
                e.message,
                "fake-0 is already claimed by this workspace; --distinct needs a resource it \
                 does not hold"
            );
        }
        // Another workspace's claim, or the workspace's own reservation, is waited for.
        let mut w = World::new(catalog(1));
        w.claims.push(lease("l0", &dev(0), "w2", 10, None));
        w.pending
            .push(req("d", 1, exact(&dev(0)), "w1", hold_purpose(true), &[20]));
        assert_eq!(w.admit("d"), Admission::Queue);
        let mut w = World::new(catalog(1));
        w.claims.push(reservation("v0", &dev(0), "w1"));
        w.pending
            .push(req("d", 1, exact(&dev(0)), "w1", hold_purpose(true), &[20]));
        assert_eq!(w.admit("d"), Admission::Queue);
        assert_eq!(w.grants(), g(&[("d", &dev(0))]));
    }

    // ---- Precedence and blocking ----

    #[test]
    fn own_reservation_is_preferred() {
        let mut w = World::new(catalog(3));
        w.claims.push(reservation("v2", &dev(2), "w1"));
        w.pending.push(run("r1", 1, any_device(), "w2", &[20]));
        w.pending.push(run("r2", 2, any_device(), "w1", &[10]));
        w.pending
            .push(req("d3", 3, any_device(), "w1", hold_purpose(true), &[11]));
        assert_eq!(
            w.grants(),
            g(&[("r1", &dev(0)), ("r2", &dev(2)), ("d3", &dev(1))])
        );
    }

    #[test]
    fn foreign_reservation_blocks_others() {
        let mut w = World::new(catalog(0));
        w.claims.push(reservation("v", &screen(), "w2"));
        w.pending.push(run("r1", 1, exact(&screen()), "w1", &[10]));
        assert!(w.grants().is_empty());
        assert!(!is_free_for(&screen().id, "w1", &w.state()));
        w.pending.push(run("r2", 2, exact(&screen()), "w2", &[20]));
        assert_eq!(w.grants(), g(&[("r2", &screen())]));
    }

    #[test]
    fn foreign_claim_blocks() {
        let mut w = World::new(catalog(2));
        w.foreign.push(ForeignClaim {
            resource: dev(0).id,
            source: "fake".into(),
            description: "agent-device session".into(),
            workspace_path: None,
            since_ms: None,
            stop: None,
            workspace_id: None,
        });
        w.pending.push(run("r1", 1, exact(&dev(0)), "w1", &[10]));
        w.pending.push(run("r2", 2, any_device(), "w2", &[20]));
        assert_eq!(w.grants(), g(&[("r2", &dev(1))]));
        assert!(matches!(
            blockers("r1", &w.state()).as_slice(),
            [Blocker::Foreign { claim }] if claim.source == "fake"
        ));
    }

    /// An agent-device session on `res`, owned by `ws` (`None`: no registered workspace).
    fn session(res: &ResourceInfo, ws: Option<&str>, name: &str) -> ForeignClaim {
        ForeignClaim {
            resource: res.id.clone(),
            source: "agent-device".into(),
            description: format!("agent-device session {name}"),
            workspace_path: ws.map(|w| format!("/work/{w}")),
            since_ms: Some(500),
            stop: Some(serde_json::json!({
                "session": name,
                "platform": "android",
                "id": "emulator-5554",
            })),
            workspace_id: ws.map(str::to_owned),
        }
    }

    fn device_end(res: &ResourceInfo, seq: u64, ws: Option<&str>) -> DeviceEnd {
        DeviceEnd {
            resource: res.id.clone(),
            seq,
            workspace_id: ws.map(str::to_owned),
            forgotten: false,
        }
    }

    #[test]
    fn own_session_does_not_block_its_workspace() {
        let mut w = World::new(catalog(1));
        w.foreign.push(session(&dev(0), Some("w1"), "s1"));
        assert!(is_free_for(&dev(0).id, "w1", &w.state()));
        w.pending.push(run("r1", 1, exact(&dev(0)), "w1", &[10]));
        assert!(blockers("r1", &w.state()).is_empty(), "nothing waits on it");
        assert_eq!(w.grants(), g(&[("r1", &dev(0))]));
    }

    #[test]
    fn other_workspace_session_blocks() {
        let mut w = World::new(catalog(1));
        w.foreign.push(session(&dev(0), Some("w1"), "s1"));
        assert!(!is_free_for(&dev(0).id, "w2", &w.state()));
        w.pending.push(run("r2", 1, exact(&dev(0)), "w2", &[20]));
        assert!(w.grants().is_empty());
        assert!(matches!(
            blockers("r2", &w.state()).as_slice(),
            [Blocker::Foreign { claim }] if claim.description == "agent-device session s1"
        ));
    }

    #[test]
    fn unknown_workspace_session_blocks_everyone() {
        let mut w = World::new(catalog(1));
        w.foreign.push(session(&dev(0), None, "s1"));
        for ws in ["w1", "w2"] {
            assert!(!is_free_for(&dev(0).id, ws, &w.state()), "{ws}");
        }
        w.pending.push(run("r1", 1, exact(&dev(0)), "w1", &[10]));
        assert!(w.grants().is_empty());
        assert_eq!(blockers("r1", &w.state()).len(), 1);
    }

    /// `deadlocked` reads only hostler's claims: a foreign claim neither makes a request stuck
    /// nor lets a stuck one out (P2-19).
    #[test]
    fn deadlocked_is_unchanged_by_foreign_claims() {
        // A cycle: r1 (under lease A's run) waits for dev 1, held by B's run, which waits for dev 0.
        let mut w = World::new(catalog(2));
        w.claims.push(lease("A", &dev(0), "w1", 10, None));
        w.claims.push(lease("B", &dev(1), "w2", 20, None));
        w.pending
            .push(run("r1", 1, exact(&dev(1)), "w1", &[11, 10]));
        w.pending
            .push(run("r2", 2, exact(&dev(0)), "w2", &[21, 20]));
        let before = (w.deadlocked(), w.victims());
        assert_eq!(before.0.len(), 2);
        for f in [
            session(&dev(0), Some("w1"), "own"),
            session(&dev(1), None, "stranger"),
            session(&dev(0), Some("w2"), "theirs"),
        ] {
            w.foreign.push(f);
            assert_eq!((w.deadlocked(), w.victims()), before);
        }
        // A request blocked only by a foreign claim is not stuck: the session can end.
        let mut w = World::new(catalog(1));
        w.foreign.push(session(&dev(0), None, "s"));
        w.pending.push(run("r1", 1, exact(&dev(0)), "w1", &[10]));
        assert!(w.deadlocked().is_empty());
    }

    /// P-14: a device that is not in the catalog (removed, unregistered, gone) is free for nobody;
    /// a request for it waits. Accounts are not catalogued and stay free.
    #[test]
    fn uncatalogued_device_is_not_free() {
        let mut w = World::new(catalog(1));
        let gone = dev(7);
        assert!(!is_free_for(&gone.id, "w1", &w.state()));
        assert!(is_free_for(&dev(0).id, "w1", &w.state()));
        assert!(is_free_for(&acct("qa").id, "w1", &w.state()));
        w.pending.push(run("r1", 1, exact(&gone), "w1", &[10]));
        assert!(w.grants().is_empty());
        assert!(w.deadlocked().is_empty(), "it may come back");
    }

    #[test]
    fn freshness_gate_blocks_other_workspaces_until_a_read_started_after_the_end() {
        let mut w = World::new(catalog(2));
        // w1's claim on dev 0 ended as end 5; the last good read started at 4.
        w.device_ends.push(device_end(&dev(0), 5, Some("w1")));
        w.freshness = Freshness::Since(4);
        assert!(!is_free_for(&dev(0).id, "w2", &w.state()));
        assert!(
            is_free_for(&dev(1).id, "w2", &w.state()),
            "dev 1 is at start"
        );
        w.pending.push(run("r2", 1, exact(&dev(0)), "w2", &[20]));
        assert!(w.grants().is_empty());
        assert_eq!(
            blockers("r2", &w.state()),
            [Blocker::Checking {
                resource: "fake-0".into(),
                text: "checking for agent-device sessions on fake-0".into(),
            }]
        );
        // L54, P3bT9b-2: while the status cannot be read, the text names the fix and the
        // person's escape.
        w.unreadable = Some("node not found".into());
        assert_eq!(
            blockers("r2", &w.state()),
            [Blocker::Checking {
                resource: "fake-0".into(),
                text: format!(
                    "checking for agent-device sessions on fake-0 (agent-device status \
                     unreadable: node not found; {}; a person may release this device: hostler \
                     devices forget-sessions fake-0)",
                    crate::devices::agent_device::STATUS_FIX
                ),
            }]
        );
        w.unreadable = None;
        // `auto` takes the other device meanwhile.
        w.pending.push(run("r3", 2, any_device(), "w3", &[30]));
        assert_eq!(w.grants(), g(&[("r3", &dev(1))]));
        // A read that started at 5 (after the end) completed: open.
        w.freshness = Freshness::Since(5);
        assert!(is_free_for(&dev(0).id, "w2", &w.state()));
        assert!(blockers("r2", &w.state()).is_empty());
        assert!(w.grants().contains(&("r2".into(), dev(0).id)));
        // Not a timeout: a request blocked only by the gate is not a deadlock.
        w.freshness = Freshness::Since(4);
        assert!(w.deadlocked().is_empty());
    }

    #[test]
    fn freshness_gate_lets_the_ending_workspace_through() {
        let mut w = World::new(catalog(1));
        w.device_ends.push(device_end(&dev(0), 5, Some("w1")));
        w.freshness = Freshness::Since(4);
        assert!(is_free_for(&dev(0).id, "w1", &w.state()));
        assert!(!is_free_for(&dev(0).id, "w2", &w.state()));
        assert!(closed_to_others(&dev(0).id, &w.state()));
        w.pending.push(run("r1", 1, exact(&dev(0)), "w1", &[10]));
        assert_eq!(w.grants(), g(&[("r1", &dev(0))]));
        // An end with no single workspace closes it to everyone.
        w.device_ends[0].workspace_id = None;
        assert!(!is_free_for(&dev(0).id, "w1", &w.state()));
        // A person forgot it (L54): open to everyone.
        w.device_ends[0].forgotten = true;
        assert!(is_free_for(&dev(0).id, "w2", &w.state()));
        assert!(!closed_to_others(&dev(0).id, &w.state()));
    }

    #[test]
    fn not_yet_blocks_every_workspace_and_not_tracked_skips() {
        let mut w = World::new(catalog(2));
        w.device_ends.push(device_end(&dev(0), 3, Some("w1")));
        w.freshness = Freshness::NotYet;
        for ws in ["w1", "w2"] {
            for d in [dev(0), dev(1)] {
                assert!(!is_free_for(&d.id, ws, &w.state()), "{ws} {:?}", d.id);
            }
            // Only devices: the screen and accounts never wait for agent-device.
            assert!(is_free_for(&screen().id, ws, &w.state()));
            assert!(is_free_for(&acct("qa").id, ws, &w.state()));
        }
        // A forgotten device opens even before the first good read (L54).
        w.device_ends[0].forgotten = true;
        assert!(is_free_for(&dev(0).id, "w2", &w.state()));
        assert!(!is_free_for(&dev(1).id, "w2", &w.state()));

        w.freshness = Freshness::NotTracked;
        w.device_ends[0].forgotten = false;
        for d in [dev(0), dev(1)] {
            assert!(is_free_for(&d.id, "w2", &w.state()));
            assert!(!closed_to_others(&d.id, &w.state()));
        }
        // At start (no entry) a device is seq 1: closed until a read captured 1.
        w.freshness = Freshness::Since(0);
        assert!(!is_free_for(&dev(1).id, "w1", &w.state()));
        w.freshness = Freshness::Since(1);
        assert!(is_free_for(&dev(1).id, "w1", &w.state()));
    }

    /// P2-5: a `--distinct` hold does not take a device its workspace's own session is on, and
    /// a plain `auto` prefers it (after its reservation).
    #[test]
    fn distinct_skips_own_session_device_and_auto_prefers_it() {
        let mut w = World::new(catalog(3));
        w.foreign.push(session(&dev(0), Some("w1"), "mine"));
        w.pending
            .push(req("d", 1, any_device(), "w1", hold_purpose(true), &[10]));
        assert_eq!(w.grants(), g(&[("d", &dev(1))]));
        // Exact on it: waits.
        w.pending[0].selector = exact(&dev(0));
        assert!(w.grants().is_empty());

        let mut w = World::new(catalog(3));
        w.foreign.push(session(&dev(1), Some("w1"), "mine"));
        w.pending.push(run("a", 1, any_device(), "w1", &[10]));
        assert_eq!(w.grants(), g(&[("a", &dev(1))]));
        // Another workspace's session is never preferred, and blocks.
        w.foreign.push(session(&dev(0), Some("w2"), "theirs"));
        w.pending.push(run("b", 2, any_device(), "w2", &[20]));
        assert_eq!(w.grants(), g(&[("a", &dev(1)), ("b", &dev(0))]));
        // A reservation still comes first.
        let mut w = World::new(catalog(3));
        w.foreign.push(session(&dev(1), Some("w1"), "mine"));
        w.claims.push(reservation("v", &dev(2), "w1"));
        w.pending.push(run("a", 1, any_device(), "w1", &[10]));
        assert_eq!(w.grants(), g(&[("a", &dev(2))]));
    }

    /// P3-12: a distinct request waiting only on its own session names it, with the close
    /// command; a plain one does not wait on it at all.
    #[test]
    fn distinct_wait_on_own_session_names_it() {
        let mut w = World::new(catalog(1));
        w.foreign.push(session(&dev(0), Some("w1"), "mine"));
        w.pending
            .push(req("d", 1, any_device(), "w1", hold_purpose(true), &[10]));
        assert!(w.grants().is_empty());
        let found = blockers("d", &w.state());
        let [Blocker::Foreign { claim }] = found.as_slice() else {
            panic!("{found:?}");
        };
        assert_eq!(
            claim.description,
            "your own agent-device session mine; close it: agent-device close --session mine \
             --platform android --serial emulator-5554"
        );
        assert_eq!(claim.workspace_id.as_deref(), Some("w1"));
        // iOS names the UDID; without stop data, the session alone.
        let mut ios = session(&dev(0), Some("w1"), "m2");
        ios.stop = Some(serde_json::json!({ "session": "m2", "platform": "ios", "id": "UDID-1" }));
        assert!(own_session_text(&ios).ends_with("--platform ios --udid UDID-1"));
        ios.stop = None;
        assert_eq!(
            own_session_text(&ios),
            "your own agent-device session ?; close it: agent-device close --session ?"
        );

        w.pending.push(run("p", 2, any_device(), "w1", &[20]));
        assert!(blockers("p", &w.state()).is_empty());
        assert!(w.deadlocked().is_empty(), "a session can close");
    }

    #[test]
    fn seizing_resource_is_not_granted() {
        let mut w = World::new(catalog(0));
        w.seizing.push(Seizing {
            resource: ResourceId::screen(),
            by_workspace: "/work/w2".into(),
        });
        w.pending.push(run("r1", 1, exact(&screen()), "w1", &[10]));
        assert!(w.grants().is_empty());
        assert_eq!(
            blockers("r1", &w.state()),
            vec![Blocker::Seizing {
                by_workspace: "/work/w2".into()
            }]
        );
    }

    #[test]
    fn unavailable_is_not_granted() {
        let mut w = World::new(catalog(2));
        w.catalog[1].available = false;
        w.pending.push(run("r1", 1, exact(&dev(0)), "w1", &[10]));
        w.pending.push(run("r2", 2, any_device(), "w2", &[20]));
        assert_eq!(w.grants(), g(&[("r2", &dev(1))]));
        // Accounts are not catalogued and count as available.
        w.pending
            .push(run("r3", 3, exact(&acct("qa")), "w3", &[30]));
        assert!(is_free_for(&acct("qa").id, "w3", &w.state()));
    }

    #[test]
    fn a_resource_that_left_the_catalog_keeps_its_claims() {
        let mut w = World::new(catalog(0));
        w.claims.push(lease("l0", &dev(0), "w2", 20, None));
        w.pending.push(run("r1", 1, exact(&dev(0)), "w1", &[10]));
        assert!(w.grants().is_empty());
        assert_eq!(w.admit("r1"), Admission::Queue);
    }

    #[test]
    fn blockers_name_root_claims_with_their_linked_count() {
        let mut w = World::new(catalog(0));
        w.claims.push(ttl_hold("h1", &screen(), "w2", 9_000));
        w.claims
            .push(linked(lease("l1", &screen(), "w2", 20, Some(21)), "h1"));
        w.pending.push(run("r1", 1, exact(&screen()), "w1", &[10]));
        let found = blockers("r1", &w.state());
        let [Blocker::Claim { summary }] = found.as_slice() else {
            panic!("one claim blocker: {found:?}");
        };
        assert_eq!(summary.id, "h1");
        assert_eq!(summary.linked, 1);
        assert_eq!(summary.expires_at_ms, Some(9_000));
        assert_eq!(summary.workspace_path, "/work/w2");
        assert!(blockers("nope", &w.state()).is_empty());
    }

    #[test]
    fn summary_pid_is_a_process_that_holds() {
        // A TTL hold's `holder` is the finished `hostler hold` command: no pid to show.
        let hold = ttl_hold("h", &screen(), "w1", 9_000);
        assert_eq!(summary(&hold, &[]).pid, None);
        let bound = bound_hold("b", &screen(), "w1", 50);
        assert_eq!(summary(&bound, &[]).pid, Some(50));
        assert_eq!(summary(&bound, &[]).expires_at_ms, None);
        let run = lease("l", &screen(), "w1", 10, Some(11));
        assert_eq!(summary(&run, &[]).pid, Some(10));
    }

    // ---- Position ----

    #[test]
    fn position_counts_intersecting_requests_only() {
        let mut w = World::new(catalog(3));
        w.pending.push(run("a", 1, exact(&dev(0)), "w1", &[10]));
        w.pending.push(run("s", 2, exact(&screen()), "w2", &[20]));
        w.pending.push(run("b", 3, any_device(), "w3", &[30]));
        w.pending.push(run("c", 4, exact(&dev(2)), "w4", &[40]));
        w.pending.push(run("d", 5, exact(&dev(1)), "w5", &[50]));
        let s = w.state();
        assert_eq!(position("a", &s), Some((1, 2)));
        assert_eq!(position("s", &s), Some((1, 1)));
        assert_eq!(position("b", &s), Some((2, 4)));
        assert_eq!(position("c", &s), Some((2, 2)));
        assert_eq!(position("d", &s), Some((2, 2)));
        assert_eq!(position("x", &s), None);
    }

    // ---- Expiry and reaping ----

    #[test]
    fn ttl_expiry_by_clock() {
        let claims = vec![
            ttl_hold("h1", &screen(), "w1", 5_000),
            bound_hold("h2", &dev(0), "w1", 10),
            lease("l1", &dev(1), "w1", 10, None),
        ];
        assert!(expired(&claims, 4_999).is_empty());
        assert_eq!(expired(&claims, 5_000), ["h1"]);
        assert_eq!(expired(&claims, i64::MAX), ["h1"]);
    }

    #[test]
    fn hold_does_not_expire_while_a_linked_lease_lives() {
        let mut claims = vec![
            ttl_hold("h1", &screen(), "w1", 5_000),
            linked(lease("l1", &screen(), "w1", 10, None), "h1"),
        ];
        assert!(expired(&claims, 6_000).is_empty());
        claims.pop();
        assert_eq!(expired(&claims, 6_000), ["h1"]);
    }

    fn all_live(_: &ProcessIdentity) -> bool {
        true
    }

    fn lease_live(_: &Claim) -> bool {
        true
    }

    #[test]
    fn stranded_hold_when_workspace_path_is_gone() {
        let claims = vec![
            ttl_hold("h1", &screen(), "gone", 9_000),
            reservation("v1", &dev(0), "gone"),
            ttl_hold("h2", &dev(1), "here", 9_000),
            lease("l1", &dev(2), "gone", 10, None),
        ];
        let exists = |p: &str| p != "/work/gone";
        assert_eq!(
            reapable(&claims, &[], &all_live, &lease_live, &exists),
            [
                ("h1".to_owned(), EndReason::Stranded),
                ("v1".to_owned(), EndReason::Stranded)
            ]
        );
    }

    #[test]
    fn stranded_hold_kept_while_linked_lease_lives() {
        let claims = vec![
            ttl_hold("h1", &screen(), "gone", 9_000),
            linked(ttl_hold("h2", &screen(), "gone", 9_000), "h1"),
            linked(lease("l1", &screen(), "gone", 10, None), "h2"),
        ];
        let exists = |p: &str| p != "/work/gone";
        assert!(reapable(&claims, &[], &all_live, &lease_live, &exists).is_empty());
        // Once the lease is dead, it, the linked hold and the root all end in one pass.
        let dead_lease = |c: &Claim| c.id != "l1";
        assert_eq!(
            reapable(&claims, &[], &all_live, &dead_lease, &exists),
            [
                ("l1".to_owned(), EndReason::Reaped),
                ("h2".to_owned(), EndReason::Stranded),
                ("h1".to_owned(), EndReason::Stranded)
            ]
        );
    }

    #[test]
    fn dead_waiter_is_reaped() {
        let bound = Purpose::Hold {
            ttl_ms: None,
            bound_to: Some(ident(77)),
            distinct: true,
        };
        let pending = vec![
            run("r1", 1, exact(&screen()), "w1", &[10]),
            run("r2", 2, exact(&screen()), "w2", &[20]),
            req("r3", 3, any_device(), "w3", bound, &[30, 77]),
        ];
        let live = |p: &ProcessIdentity| p.pid != 20 && p.pid != 77;
        assert_eq!(
            reapable(&[], &pending, &live, &lease_live, &|_| true),
            [
                ("r2".to_owned(), EndReason::Reaped),
                ("r3".to_owned(), EndReason::Reaped)
            ]
        );
    }

    #[test]
    fn lease_alive_while_child_or_group_lives() {
        let mut c = lease("l1", &screen(), "w1", 10, Some(11));
        c.group = Some(RunGroup::UnixGroup {
            pgid: 11,
            boot_id: "boot-1".into(),
        });
        let group_live = |_: &RunGroup| true;
        let group_dead = |_: &RunGroup| false;
        let none = |_: &ProcessIdentity| false;
        let child_only = |p: &ProcessIdentity| p.pid == 11;
        assert!(lease_alive(&c, &all_live, &group_dead));
        assert!(lease_alive(&c, &child_only, &group_dead));
        assert!(
            lease_alive(&c, &none, &group_live),
            "a group survivor keeps it"
        );
        assert!(!lease_alive(&c, &none, &group_dead));
        let claims = [c];
        let lease_live = |c: &Claim| lease_alive(c, &none, &group_dead);
        assert_eq!(
            reapable(&claims, &[], &none, &lease_live, &|_| true),
            [("l1".to_owned(), EndReason::Reaped)]
        );
    }

    #[test]
    fn process_bound_hold_reaped_when_process_dies() {
        let claims = vec![
            bound_hold("h1", &dev(0), "w1", 50),
            bound_hold("h2", &dev(1), "w1", 51),
            ttl_hold("h3", &screen(), "w1", 9_000),
        ];
        // The holds' `holder` (the `hostler hold` command, 900/901) is gone; only the binding
        // counts.
        let live = |p: &ProcessIdentity| p.pid == 51;
        assert_eq!(
            reapable(&claims, &[], &live, &lease_live, &|_| true),
            [("h1".to_owned(), EndReason::Reaped)]
        );
    }

    // ---- Deadlocks ----

    #[test]
    fn heavy_then_screen_vs_screen_then_heavy_is_deadlocked() {
        let mut w = World::new(catalog(0));
        // R1 `run --heavy -- sh -c 'hostler run --screen'`: 100 runs heavy, 102 (under sh 101)
        // waits for the screen.
        w.heavy.running.push(heavy_running("hv1", 100, Some(101)));
        w.pending
            .push(run("inner", 2, exact(&screen()), "w1", &[102, 101, 100, 1]));
        // R2 `run --screen --heavy`: 200 holds the screen and waits for heavy.
        w.claims.push(lease("l2", &screen(), "w2", 200, None));
        w.heavy.waiting.push(("hv2".into(), vec![200, 1]));
        assert_eq!(w.deadlocked(), ["inner"]);
    }

    #[test]
    fn account_then_screen_vs_screen_then_account_is_deadlocked() {
        let mut w = World::new(catalog(0));
        w.claims.push(lease("la", &acct("qa"), "w1", 100, None));
        w.pending
            .push(run("r1", 3, exact(&screen()), "w1", &[100, 1]));
        w.claims.push(lease("ls", &screen(), "w2", 200, None));
        w.pending
            .push(run("r2", 4, exact(&acct("qa")), "w2", &[200, 1]));
        assert_eq!(w.deadlocked(), ["r2", "r1"]);
    }

    #[test]
    fn self_wait_on_own_ancestor_is_deadlocked() {
        let mut w = World::new(catalog(0));
        w.claims.push(lease("l1", &screen(), "w1", 100, Some(101)));
        // A nested run that lost its token, in the same or in another checkout.
        w.pending.push(run(
            "inner",
            1,
            exact(&screen()),
            "w1",
            &[103, 102, 101, 100, 1],
        ));
        assert_eq!(w.deadlocked(), ["inner"]);
        w.pending[0].workspace_id = "w2".into();
        assert_eq!(w.deadlocked(), ["inner"]);
        // Not an ancestor: it just waits.
        w.pending[0].ancestry = vec![103, 1];
        assert!(w.deadlocked().is_empty());
    }

    #[test]
    fn waiting_behind_a_ttl_hold_is_not_deadlocked() {
        let mut w = World::new(catalog(0));
        w.claims.push(ttl_hold("h1", &screen(), "w2", 9_000));
        w.claims.push(lease("la", &acct("qa"), "w1", 100, None));
        w.pending
            .push(run("r1", 1, exact(&screen()), "w1", &[100, 1]));
        // The hold's own `holder` pid (the finished `hostler hold`) is no process to wait for.
        w.pending
            .push(run("r2", 2, exact(&acct("qa")), "w2", &[900, 1]));
        assert!(w.deadlocked().is_empty());
        // Reservations progress too.
        w.claims[0] = reservation("v1", &screen(), "w2");
        assert!(w.deadlocked().is_empty());
        // A process-bound hold waits on its bound process only, not on `hostler hold` (901).
        w.claims[0] = bound_hold("h1", &screen(), "w2", 950);
        w.pending[1].ancestry = vec![901, 1];
        assert!(w.deadlocked().is_empty());
        w.pending[1].ancestry = vec![902, 950, 1];
        assert_eq!(w.deadlocked(), ["r2", "r1"]);
    }

    #[test]
    fn a_free_alternative_candidate_breaks_the_cycle() {
        let mut w = World::new(catalog(2));
        w.claims.push(lease("la", &acct("qa"), "w1", 100, None));
        w.pending.push(run("r1", 3, any_device(), "w1", &[100, 1]));
        w.claims.push(lease("l0", &dev(0), "w2", 200, None));
        w.pending
            .push(run("r2", 4, exact(&acct("qa")), "w2", &[200, 1]));
        assert!(w.deadlocked().is_empty(), "fake-1 is free");
        w.claims.push(lease("l1", &dev(1), "w3", 300, None));
        assert!(w.deadlocked().is_empty(), "fake-1's holder does not wait");
        w.pending
            .push(run("r3", 5, exact(&acct("qa")), "w3", &[300, 1]));
        assert_eq!(w.deadlocked(), ["r3", "r2", "r1"]);
        // A candidate being seized changes hands without waiting.
        w.seizing.push(Seizing {
            resource: dev(1).id,
            by_workspace: "/work/w4".into(),
        });
        assert!(w.deadlocked().is_empty());
    }

    #[test]
    fn free_heavy_slot_breaks_the_cycle() {
        let mut w = World::new(catalog(0));
        w.heavy.running.push(heavy_running("hv1", 100, Some(101)));
        w.pending
            .push(run("inner", 2, exact(&screen()), "w1", &[102, 101, 100, 1]));
        w.claims.push(lease("l2", &screen(), "w2", 200, None));
        w.heavy.waiting.push(("hv2".into(), vec![200, 1]));
        w.heavy.capacity = 2;
        assert!(w.deadlocked().is_empty());
        // Full again, but by a run that waits for nothing.
        w.heavy.running.push(heavy_running("hv3", 300, None));
        assert!(w.deadlocked().is_empty());
        w.heavy.capacity = 1;
        w.heavy.running.pop();
        assert_eq!(w.deadlocked(), ["inner"]);
    }

    #[test]
    fn running_beyond_a_lowered_capacity_must_drop_below_it() {
        let mut w = World::new(catalog(0));
        w.heavy.running.push(heavy_running("hv1", 100, Some(101)));
        w.pending
            .push(run("inner", 2, exact(&screen()), "w1", &[102, 101, 100, 1]));
        w.claims.push(lease("l2", &screen(), "w2", 200, None));
        w.heavy.waiting.push(("hv2".into(), vec![200, 1]));
        // Capacity went from 2 to 1 with two runs still going: the free one ending is not
        // enough.
        w.heavy.running.push(heavy_running("hv3", 300, None));
        assert_eq!(w.deadlocked(), ["inner"]);
    }

    #[test]
    fn youngest_member_comes_first() {
        let mut w = World::new(catalog(1));
        w.claims.push(lease("la", &acct("qa"), "w1", 100, None));
        w.pending
            .push(run("r1", 9, exact(&screen()), "w1", &[100, 1]));
        w.claims.push(lease("ls", &screen(), "w2", 200, None));
        w.pending
            .push(run("r2", 2, exact(&dev(0)), "w2", &[200, 1]));
        w.claims.push(lease("ld", &dev(0), "w3", 300, None));
        w.pending
            .push(run("r3", 5, exact(&acct("qa")), "w3", &[300, 1]));
        // A bystander waits on the cycle but holds nothing it needs: stuck, never a victim, so
        // neither refused at acquire nor on a tick (L25).
        w.pending
            .push(run("r4", 12, exact(&screen()), "w4", &[400, 1]));
        assert_eq!(w.deadlocked(), ["r4", "r1", "r3", "r2"]);
        assert_eq!(w.victims(), ["r1", "r3", "r2"]);
        // Refusing the youngest victim breaks the cycle; r4 then just waits for the screen.
        w.pending.retain(|p| p.id != "r1");
        assert!(w.deadlocked().is_empty());
    }

    #[test]
    fn victims_are_the_members_that_hold_what_the_cycle_needs() {
        // Account and screen cycle: both hold what the other waits for.
        let mut w = World::new(catalog(0));
        w.claims.push(lease("la", &acct("qa"), "w1", 100, None));
        w.pending
            .push(run("r1", 3, exact(&screen()), "w1", &[100, 1]));
        w.claims.push(lease("ls", &screen(), "w2", 200, None));
        w.pending
            .push(run("r2", 4, exact(&acct("qa")), "w2", &[200, 1]));
        assert_eq!(w.victims(), ["r2", "r1"]);
        // A self-wait is its own victim.
        let mut w = World::new(catalog(0));
        w.claims.push(lease("l1", &screen(), "w1", 100, Some(101)));
        w.pending
            .push(run("inner", 1, exact(&screen()), "w1", &[103, 101, 100, 1]));
        assert_eq!(w.victims(), ["inner"]);
        // Through the heavy queue: the inner run's ancestor runs the heavy entry the other
        // waits for.
        let mut w = World::new(catalog(0));
        w.heavy.running.push(heavy_running("hv1", 100, Some(101)));
        w.pending
            .push(run("inner", 2, exact(&screen()), "w1", &[102, 101, 100, 1]));
        w.claims.push(lease("l2", &screen(), "w2", 200, None));
        w.heavy.waiting.push(("hv2".into(), vec![200, 1]));
        w.pending
            .push(run("late", 9, exact(&screen()), "w3", &[300, 1]));
        assert_eq!(w.deadlocked(), ["late", "inner"]);
        assert_eq!(w.victims(), ["inner"]);
    }

    /// R1 and R2 deadlock on the screen and an account. X holds a device (wrapper 500) and its
    /// nested step x5 waits for the screen behind R2; Y waits for X's device.
    fn chain_behind_a_cycle() -> World {
        let mut w = World::new(catalog(1));
        w.claims.push(lease("la", &acct("qa"), "w1", 100, None));
        w.pending
            .push(run("r1", 1, exact(&screen()), "w1", &[100, 1]));
        w.claims.push(lease("ls", &screen(), "w2", 200, None));
        w.pending
            .push(run("r2", 2, exact(&acct("qa")), "w2", &[200, 1]));
        w.pending
            .push(run("x5", 9, exact(&screen()), "wx", &[501, 500, 1]));
        w
    }

    #[test]
    fn a_chain_behind_a_cycle_is_not_a_victim() {
        let mut w = chain_behind_a_cycle();
        w.claims.push(lease("ld", &dev(0), "wx", 500, None));
        w.pending.push(run("b", 7, exact(&dev(0)), "wy", &[600, 1]));
        assert_eq!(w.deadlocked(), ["x5", "b", "r2", "r1"]);
        assert_eq!(w.victims(), ["r2", "r1"]);
        // Breaking the cycle frees the whole chain.
        w.pending.retain(|p| p.id != "r2");
        assert!(w.deadlocked().is_empty());
    }

    #[test]
    fn a_heavy_chain_behind_a_cycle_is_not_a_victim() {
        // X runs the one heavy slot instead; Y waits for heavy.
        let mut w = chain_behind_a_cycle();
        w.heavy.running.push(heavy_running("hvx", 500, Some(501)));
        w.heavy.waiting.push(("hvy".into(), vec![600, 1]));
        assert_eq!(w.deadlocked(), ["x5", "r2", "r1"]);
        assert_eq!(w.victims(), ["r2", "r1"]);
    }

    /// 500 stuck requests near the 512-row bound, in groups of 10 under shared ancestors: each
    /// group's ancestor holds one of 50 devices. `any`: every request waits for any device, so
    /// every node points at all 500 (a dense graph). Otherwise a ring of Exact requests.
    fn large_stuck_world(any: bool) -> World {
        let mut w = World::new(catalog(50));
        for j in 0..50 {
            w.claims.push(lease(
                &format!("l{j}"),
                &dev(j),
                &format!("w{j}"),
                5_000 + j,
                None,
            ));
        }
        for i in 0..500u32 {
            let sel = if any {
                any_device()
            } else {
                exact(&dev((i / 10 + 1) % 50))
            };
            let ancestry: Vec<u32> = [10_000 + i, 5_000 + i / 10]
                .into_iter()
                .chain(1..20)
                .collect();
            w.pending.push(run(
                &format!("q{i}"),
                u64::from(i),
                sel,
                &format!("v{i}"),
                &ancestry,
            ));
        }
        w
    }

    #[test]
    fn deadlock_check_is_fast_on_a_large_stuck_set() {
        for any in [false, true] {
            let w = large_stuck_world(any);
            let started = std::time::Instant::now();
            let d = deadlocked(&w.state());
            let took = started.elapsed();
            assert_eq!(d.stuck.len(), 500);
            assert_eq!(d.victims.len(), 500, "one ring through every group");
            assert_eq!(d.victims[0], "q499");
            // Measured in a debug build: about 6 ms (ring) and 80 ms (dense); the bound only
            // leaves room for a loaded machine.
            assert!(
                took < std::time::Duration::from_millis(900),
                "any={any}: {took:?}"
            );
        }
    }

    #[test]
    fn a_heavy_only_deadlock_has_no_claims_victim() {
        // A nested heavy run without its token waits on its own ancestor's slot; the claims
        // request behind it is stuck, but refusing it would free nothing.
        let mut w = World::new(catalog(0));
        w.heavy.running.push(heavy_running("hv1", 100, Some(101)));
        w.heavy.waiting.push(("hv2".into(), vec![102, 101, 100, 1]));
        w.claims.push(lease("ls", &screen(), "w1", 102, None));
        w.pending
            .push(run("r1", 1, exact(&screen()), "w2", &[200, 1]));
        assert_eq!(w.deadlocked(), ["r1"]);
        assert!(w.victims().is_empty());
    }

    #[test]
    fn cycle_through_a_hold_kept_alive_by_a_linked_lease_is_deadlocked() {
        let mut w = World::new(catalog(0));
        // W1's TTL hold on the screen; run A (100) joined it and waits for heavy.
        w.claims.push(ttl_hold("h", &screen(), "w1", 9_000));
        w.claims
            .push(linked(lease("a", &screen(), "w1", 100, None), "h"));
        w.heavy.waiting.push(("hva".into(), vec![100, 1]));
        // B (200) holds heavy; its inner run (202) waits for the screen.
        w.heavy.running.push(heavy_running("hvb", 200, Some(201)));
        w.pending
            .push(run("inner", 7, exact(&screen()), "w2", &[202, 201, 200, 1]));
        assert_eq!(w.deadlocked(), ["inner"]);
        // Without the linked lease, the TTL hold expires and the cycle is gone.
        w.drop_claim("a");
        assert!(w.deadlocked().is_empty());
    }

    #[test]
    fn background_nested_run_is_a_documented_false_positive() {
        // P (100) holds the account and started a nested run (101) in the background; P goes on
        // with its work and would release the account, so nothing really waits forever. The
        // check cannot tell and counts P as waiting for its child (L22).
        let mut w = World::new(catalog(0));
        w.claims.push(lease("la", &acct("qa"), "w1", 100, None));
        w.pending
            .push(run("bg", 3, exact(&screen()), "w1", &[101, 100, 1]));
        w.claims.push(lease("ls", &screen(), "w2", 200, None));
        w.pending
            .push(run("r2", 4, exact(&acct("qa")), "w2", &[200, 1]));
        assert_eq!(w.deadlocked(), ["r2", "bg"]);
    }

    #[test]
    fn a_request_with_no_candidate_yet_is_not_deadlocked() {
        let mut w = World::new(catalog(0));
        w.claims.push(lease("la", &acct("qa"), "w1", 100, None));
        w.pending.push(run("r1", 1, any_device(), "w1", &[100, 1]));
        w.pending
            .push(run("r2", 2, exact(&acct("qa")), "w2", &[200, 1]));
        assert!(w.deadlocked().is_empty());
    }

    #[test]
    fn every_blocker_must_be_able_to_end() {
        // The screen frees only when both claims of W2 end; one of them waits on the cycle.
        let mut w = World::new(catalog(0));
        w.claims.push(lease("ls", &screen(), "w2", 200, None));
        w.claims.push(lease("ls2", &screen(), "w2", 210, None));
        w.claims.push(lease("la", &acct("qa"), "w1", 100, None));
        w.pending
            .push(run("r1", 1, exact(&screen()), "w1", &[100, 1]));
        w.pending
            .push(run("r2", 2, exact(&acct("qa")), "w2", &[210, 1]));
        assert_eq!(w.deadlocked(), ["r2", "r1"]);
    }

    // ---- Seizures and distinct holds ----

    #[test]
    fn no_join_on_a_seizing_resource() {
        let mut w = World::new(catalog(2));
        w.claims.push(ttl_hold("h0", &dev(0), "w1", 9_000));
        w.claims.push(lease("l1", &dev(1), "w1", 10, None));
        w.seizing.push(Seizing {
            resource: dev(0).id,
            by_workspace: "/work/w2".into(),
        });
        w.pending.push(run("r1", 1, any_device(), "w1", &[12]));
        assert_eq!(
            w.admit("r1"),
            Admission::Join {
                parent: "l1".into(),
                resource: dev(1).id
            },
            "the other device is still joined"
        );
        w.pending[0].selector = exact(&dev(0));
        assert_eq!(w.admit("r1"), Admission::Queue);
        assert!(w.grants().is_empty());
    }

    #[test]
    fn planned_taker_request_is_not_granted_elsewhere() {
        let mut w = World::new(catalog(2));
        w.claims.push(lease("l0", &dev(0), "w1", 10, None));
        w.seizing.push(Seizing {
            resource: dev(0).id,
            by_workspace: "/work/w2".into(),
        });
        w.pending.push(run("taker", 1, any_device(), "w2", &[20]));
        w.pending.push(run("r2", 2, any_device(), "w3", &[30]));
        w.planned.push("taker".into());
        assert_eq!(w.grants(), g(&[("r2", &dev(1))]));
        assert!(w.deadlocked().is_empty());
    }

    // ---- Randomized checks against independent oracles ----

    /// xorshift64*: deterministic, no dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn chance(&mut self, percent: u64) -> bool {
            self.next() % 100 < percent
        }
    }

    const PIDS: u32 = 12;

    /// A random world over the screen, three devices (one may be unavailable) and an account:
    /// processes 1..=12 in a random tree, claims of up to three workspaces (linked ones on their
    /// parent's resource), foreign claims, seizures, plans, and a heavy queue.
    fn random_world(rng: &mut Rng) -> World {
        let mut w = World::new(catalog(3));
        if rng.chance(20) {
            w.catalog[1 + rng.below(3)].available = false;
        }
        let resources: Vec<ResourceInfo> = w.catalog.iter().cloned().chain([acct("qa")]).collect();
        let parent: Vec<Option<u32>> = (0..=PIDS)
            .map(|p| (p > 1 && rng.chance(70)).then(|| 1 + rng.below(p as usize - 1) as u32))
            .collect();
        let ancestry = |mut p: u32| {
            let mut out = vec![p];
            while let Some(q) = parent[p as usize] {
                out.push(q);
                p = q;
            }
            out
        };
        let pid = |rng: &mut Rng| 1 + rng.below(PIDS as usize) as u32;
        let ws = |rng: &mut Rng| format!("w{}", rng.below(3));
        for i in 0..rng.below(7) {
            let res = &resources[rng.below(resources.len())];
            let id = format!("c{i}");
            let owner = ws(rng);
            let mut c = match rng.below(5) {
                0 | 1 => {
                    let child = rng.chance(50).then(|| pid(rng));
                    lease(&id, res, &owner, pid(rng), child)
                }
                2 => ttl_hold(&id, res, &owner, 9_000),
                3 => bound_hold(&id, res, &owner, pid(rng)),
                _ => reservation(&id, res, &owner),
            };
            if c.kind != ClaimKind::Reservation && rng.chance(40) {
                let parents: Vec<&Claim> = w
                    .claims
                    .iter()
                    .filter(|p| p.resource.id == res.id && p.kind != ClaimKind::Reservation)
                    .collect();
                if !parents.is_empty() {
                    let p = parents[rng.below(parents.len())];
                    c.parent = Some(p.id.clone());
                    c.workspace = p.workspace.clone();
                }
            }
            w.claims.push(c);
        }
        if rng.chance(20) {
            w.foreign.push(ForeignClaim {
                resource: resources[rng.below(resources.len())].id.clone(),
                source: "fake".into(),
                description: "x".into(),
                workspace_path: None,
                since_ms: None,
                stop: None,
                workspace_id: None,
            });
        }
        if rng.chance(15) {
            w.seizing.push(Seizing {
                resource: resources[rng.below(resources.len())].id.clone(),
                by_workspace: "/work/taker".into(),
            });
        }
        let mut tickets: Vec<u64> = (1..=8).collect();
        for i in 0..rng.below(6) {
            let selector = match rng.below(4) {
                0 => any_device(),
                1 if rng.chance(30) => Selector::Any {
                    kind: ResourceKind::Device,
                    attributes: BTreeMap::from([("test".to_owned(), "2".to_owned())]),
                },
                _ => exact(&resources[rng.below(resources.len())]),
            };
            let purpose = match rng.below(4) {
                0 => hold_purpose(false),
                1 => hold_purpose(true),
                _ => lease_purpose(),
            };
            let ticket = tickets.remove(rng.below(tickets.len()));
            let requester = pid(rng);
            w.pending.push(req(
                &format!("q{i}"),
                ticket,
                selector,
                &ws(rng),
                purpose,
                &ancestry(requester),
            ));
        }
        if !w.pending.is_empty() && rng.chance(15) {
            w.planned
                .push(w.pending[rng.below(w.pending.len())].id.clone());
        }
        w.heavy.capacity = 1 + rng.below(2) as u8;
        for i in 0..rng.below(3) {
            let child = rng.chance(50).then(|| pid(rng));
            let mut r = heavy_running(&format!("hr{i}"), pid(rng), child);
            r.holder_ancestry = ancestry(r.holder.pid);
            w.heavy.running.push(r);
        }
        for i in 0..rng.below(3) {
            w.heavy.waiting.push((format!("hw{i}"), ancestry(pid(rng))));
        }
        w
    }

    /// May `p` take `r` now, by the rules restated from the plan (not through `grants`)?
    fn usable(p: &Pending, r: &ResourceId, w: &World) -> bool {
        let wanted = match &p.selector {
            Selector::Exact { resource } => resource == r,
            Selector::Any { kind, attributes } => w.catalog.iter().any(|i| {
                i.id == *r
                    && i.kind == *kind
                    && attributes
                        .iter()
                        .all(|(k, v)| i.attributes.get(k) == Some(v))
            }),
        };
        let available = w.catalog.iter().all(|i| i.id != *r || i.available);
        wanted
            && available
            && !w.seizing.iter().any(|z| z.resource == *r)
            && !w.foreign.iter().any(|f| f.resource == *r)
            && w.claims
                .iter()
                .filter(|c| c.resource.id == *r)
                .all(|c| c.workspace.id == p.workspace_id && c.kind == ClaimKind::Reservation)
    }

    #[test]
    fn grants_are_fifo_single_and_maximal_on_random_worlds() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..20_000 {
            let mut w = random_world(&mut rng);
            let granted = w.grants();
            let mut order: Vec<&Pending> = w.pending.iter().collect();
            order.sort_by_key(|p| p.ticket);
            let mut taken: Vec<&ResourceId> = Vec::new();
            for p in order {
                let got = granted.iter().find(|(id, _)| *id == p.id).map(|(_, r)| r);
                if w.planned.contains(&p.id) {
                    assert!(got.is_none(), "round {round}: planned {} granted", p.id);
                    continue;
                }
                let reserved = |r: &ResourceId| {
                    w.claims.iter().any(|c| {
                        c.resource.id == *r
                            && c.workspace.id == p.workspace_id
                            && c.kind == ClaimKind::Reservation
                    })
                };
                let mut prefs = candidates(&p.selector, &w.catalog);
                prefs.sort_by_key(|r| !reserved(r));
                let first_open = prefs
                    .into_iter()
                    .find(|r| usable(p, r, &w) && !taken.contains(&r));
                assert_eq!(got, first_open.as_ref(), "round {round}: {}", p.id);
                if let Some(r) = got {
                    assert!(!taken.contains(&r), "round {round}: {} given twice", r.0);
                    if let Selector::Exact { resource } = &p.selector {
                        assert_eq!(resource, r, "round {round}: exact substituted");
                    }
                    taken.push(r);
                }
            }
            assert_eq!(
                granted.len(),
                taken.len(),
                "round {round}: grant of an unknown id"
            );
            // The answer does not depend on the order the engine lists requests in.
            w.pending.reverse();
            let mut again = w.grants();
            let mut first = granted.clone();
            again.sort();
            first.sort();
            assert_eq!(first, again, "round {round}: order-dependent");
        }
    }

    /// Forward simulation: end every claim whose processes are not waiting (holds also need
    /// their linked claims gone), finish every heavy run that does not wait, and let every
    /// request through whose candidate is clear, until nothing changes. A granted request adds
    /// no claim: the question is whether it can be granted from this state. What is left
    /// waiting can never be granted.
    ///
    /// The victims come from that final state: the requests left that lie on a cycle of its
    /// wait-for graph (a request → each request left whose ancestry holds a process of a claim
    /// still blocking it; a heavy waiter left → each one holding a heavy run left).
    fn never_granted(w: &World) -> Deadlock {
        let mut claims: Vec<&Claim> = w.claims.iter().collect();
        let mut pending: Vec<&Pending> = w
            .pending
            .iter()
            .filter(|p| !w.planned.contains(&p.id))
            .collect();
        let mut running: Vec<&HeavyRunning> = w.heavy.running.iter().collect();
        let mut heavy_waiting = w.heavy.waiting.len();
        loop {
            let busy: HashSet<u32> = pending
                .iter()
                .flat_map(|p| p.ancestry.iter().copied())
                .chain(
                    w.heavy.waiting[..heavy_waiting]
                        .iter()
                        .flat_map(|(_, a)| a.iter().copied()),
                )
                .collect();
            let free =
                |p: &Option<ProcessIdentity>| p.as_ref().is_none_or(|p| !busy.contains(&p.pid));
            let before = (claims.len(), pending.len(), running.len(), heavy_waiting);
            // Joining happens at once, before the joined claim can end.
            pending.retain(|p| {
                let cands = candidates(&p.selector, &w.catalog);
                let distinct = matches!(p.purpose, Purpose::Hold { distinct: true, .. });
                let through = cands.is_empty()
                    || cands.iter().any(|r| {
                        let mine = claims.iter().any(|c| {
                            c.resource.id == *r
                                && c.workspace.id == p.workspace_id
                                && (c.kind == ClaimKind::Hold
                                    || (c.kind == ClaimKind::Lease
                                        && shared_within_workspace(c.resource.kind)))
                        });
                        w.seizing.iter().any(|z| z.resource == *r)
                            || (!distinct && mine)
                            || claims.iter().filter(|c| c.resource.id == *r).all(|c| {
                                c.workspace.id == p.workspace_id && c.kind == ClaimKind::Reservation
                            })
                    });
                !through
            });
            let snapshot = claims.clone();
            claims.retain(|c| {
                let ends = match (&c.kind, &c.binding) {
                    (ClaimKind::Lease, _) => free(&c.holder) && free(&c.child),
                    (ClaimKind::Hold, Some(HoldBinding::Process { holder })) => {
                        free(&Some(holder.clone()))
                    }
                    _ => true,
                } && !snapshot
                    .iter()
                    .any(|l| l.parent.as_deref() == Some(c.id.as_str()));
                !ends
            });
            running.retain(|r| !(free(&Some(r.holder.clone())) && free(&r.child)));
            if running.len() < usize::from(w.heavy.capacity) {
                heavy_waiting = 0;
            }
            if before == (claims.len(), pending.len(), running.len(), heavy_waiting) {
                // The wait-for graph of what is left, closed transitively (Floyd–Warshall).
                let pids = |c: &Claim| -> Vec<u32> {
                    match (&c.kind, &c.binding) {
                        (ClaimKind::Lease, _) => {
                            c.holder.iter().chain(&c.child).map(|p| p.pid).collect()
                        }
                        (_, Some(HoldBinding::Process { holder })) => vec![holder.pid],
                        _ => Vec::new(),
                    }
                };
                let mut nodes: Vec<(Vec<u32>, &Vec<u32>)> = Vec::new();
                for p in &pending {
                    let mut waits = Vec::new();
                    for r in candidates(&p.selector, &w.catalog) {
                        for c in claims.iter().filter(|c| c.resource.id == r) {
                            if c.workspace.id != p.workspace_id || c.kind != ClaimKind::Reservation
                            {
                                waits.extend(pids(c));
                            }
                        }
                    }
                    nodes.push((waits, &p.ancestry));
                }
                for (_, ancestry) in &w.heavy.waiting[..heavy_waiting] {
                    let waits = running
                        .iter()
                        .flat_map(|r| std::iter::once(&r.holder).chain(&r.child))
                        .map(|p| p.pid)
                        .collect();
                    nodes.push((waits, ancestry));
                }
                let n = nodes.len();
                let mut reach: Vec<Vec<bool>> = nodes
                    .iter()
                    .map(|(waits, _)| {
                        nodes
                            .iter()
                            .map(|(_, ancestry)| ancestry.iter().any(|a| waits.contains(a)))
                            .collect()
                    })
                    .collect();
                for k in 0..n {
                    for i in 0..n {
                        for j in 0..n {
                            reach[i][j] = reach[i][j] || (reach[i][k] && reach[k][j]);
                        }
                    }
                }
                let cyclic: Vec<&str> = (0..pending.len())
                    .filter(|&i| reach[i][i])
                    .map(|i| pending[i].id.as_str())
                    .collect();
                let mut left: Vec<&Pending> = pending.clone();
                left.sort_by_key(|p| std::cmp::Reverse(p.ticket));
                return Deadlock {
                    stuck: left.iter().map(|p| p.id.clone()).collect(),
                    victims: left
                        .iter()
                        .filter(|p| cyclic.contains(&p.id.as_str()))
                        .map(|p| p.id.clone())
                        .collect(),
                };
            }
        }
    }

    /// L25 on one world: victims are stuck members, in stuck's order. Refusing the youngest
    /// victim again and again refuses only requests that were victims at the start and ends with
    /// no victim; anything still stuck then waits on a heavy queue deadlocked on its own.
    fn check_victims(mut w: World, round: usize) {
        let first = deadlocked(&w.state());
        let in_order: Vec<&String> = first
            .stuck
            .iter()
            .filter(|id| first.victims.contains(id))
            .collect();
        assert_eq!(
            in_order,
            first.victims.iter().collect::<Vec<_>>(),
            "round {round}: victims are stuck, youngest first"
        );
        loop {
            let now = deadlocked(&w.state());
            let Some(victim) = now.victims.first() else {
                if !now.stuck.is_empty() {
                    let heavy: HashSet<u32> = w
                        .heavy
                        .waiting
                        .iter()
                        .flat_map(|(_, a)| a.iter().copied())
                        .collect();
                    let stuck_running = w
                        .heavy
                        .running
                        .iter()
                        .filter(|r| {
                            std::iter::once(&r.holder)
                                .chain(&r.child)
                                .any(|p| heavy.contains(&p.pid))
                        })
                        .count();
                    assert!(
                        stuck_running >= usize::from(w.heavy.capacity),
                        "round {round}: stuck {:?} with no victim and no heavy deadlock",
                        now.stuck
                    );
                }
                return;
            };
            assert!(
                first.victims.contains(victim),
                "round {round}: refused bystander {victim}"
            );
            w.pending.retain(|p| p.id != *victim);
        }
    }

    #[test]
    fn deadlock_check_matches_a_forward_simulation_on_random_worlds() {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let (mut found, mut bystanders) = (0, 0);
        for round in 0..20_000 {
            let w = random_world(&mut rng);
            let expected = never_granted(&w);
            found += usize::from(!expected.stuck.is_empty());
            bystanders += usize::from(expected.victims.len() < expected.stuck.len());
            assert_eq!(deadlocked(&w.state()), expected, "round {round}");
            check_victims(w, round);
        }
        assert!(found > 500, "the generator makes deadlocks ({found})");
        assert!(bystanders > 100, "and bystanders ({bystanders})");
    }
}
