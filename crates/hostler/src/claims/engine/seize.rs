//! Takeover: the seize methods of `ClaimsEngine` (plan P3a, Task 6).
//!
//! `seize_begin` lists every live process that uses the resource for its holding workspace (and
//! the runs nested under them, L18) and freezes the resource; the taker's CLI stops them;
//! `seize_commit` moves the claims only when each one is verifiably gone (D9, L17, L23).

use std::collections::HashSet;

use super::{
    Ancestry, ClaimsEngine, Data, PLAN_TTL_MS, Plan, being_taken_over, binding_for, claim_kind,
    enclosing, info_for, internal, no_heavy, parse_nesting, token_of, with_linked,
};
use crate::claims::rules::{self, HeavyView};
use crate::contract::HostlerError;
use crate::contract::claims::{
    Claim, ClaimKind, ForeignClaim, HoldBinding, Purpose, ResourceKind, SeizeBegin, SeizeCommit,
    SeizeOutcome, SeizePlan, SeizedFrom, SeizureRecord, StopEvidence, StopTarget, errors,
};
use crate::contract::secrets::errors::{bad_request, service_stopping};
use crate::contract::wire::Event;
use crate::procs::{self, JobState, ProcessIdentity, RunGroup};
use crate::store::ClaimRow;

/// What `seize_begin` found, kept with the plan for `seize_commit`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Detail {
    pub plan_id: String,
    pub display: String,
    /// The holder's live claims on the resource at `seize_begin` (L17: no other may appear).
    pub claim_ids: Vec<String>,
    pub stops: Vec<StopTarget>,
    /// Every planned foreign claim: another workspace's (never the taker's own, P2-10).
    pub foreign: Vec<ForeignClaim>,
    /// Every target was already dead at `seize_begin`.
    pub stranded: bool,
}

/// How a target was found gone (L30).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Proof {
    Verified,
    /// Only on the taker's `JobGone` evidence: the service could not see the job.
    Attested,
}

/// L50: a foreign claim the taker's workspace does not own; the engine set `workspace_id`.
fn blocks_taker(f: &ForeignClaim, taker_workspace: &str) -> bool {
    f.workspace_id.as_deref() != Some(taker_workspace)
}

/// A session kept from the last good read while agent-device cannot be read (L51).
fn is_kept(f: &ForeignClaim) -> bool {
    f.stop
        .as_ref()
        .and_then(|s| s.get("kept"))
        .and_then(|k| k.as_bool())
        .unwrap_or(false)
}

fn no_plan(plan_id: &str) -> HostlerError {
    bad_request(format!(
        "no takeover plan {plan_id} (it was aborted, or it expired: a taker extends it with \
         claims.seize_progress)"
    ))
}

/// A planned target as it stands now (L27): a claim's current row, a heavy entry's current
/// state; `None` when it has ended (the planned target is checked then).
fn current_target(t: &StopTarget, data: &Data, heavy: &HeavyView) -> Option<StopTarget> {
    let StopTarget::RunGroup { claim_id, .. } = t else {
        return None;
    };
    match claim_id.strip_prefix("heavy:") {
        Some(entry) => heavy
            .running
            .iter()
            .find(|h| h.id == entry)
            .map(|h| StopTarget::RunGroup {
                claim_id: claim_id.clone(),
                holder: h.holder.clone(),
                child: h.child.clone(),
                group: h.group.clone(),
            }),
        None => data
            .claims
            .iter()
            .find(|c| c.id == *claim_id)
            .and_then(target_of),
    }
}

/// How a target is named in a refusal.
pub(crate) fn describe(t: &StopTarget) -> String {
    match t {
        StopTarget::RunGroup {
            claim_id,
            holder,
            child,
            ..
        } => {
            let pid = child.as_ref().unwrap_or(holder).pid;
            match claim_id.strip_prefix("heavy:") {
                Some(entry) => format!("heavy run {entry} (PID {pid})"),
                None => format!("the run of claim {claim_id} (PID {pid})"),
            }
        }
        StopTarget::Process { claim_id, process } => {
            format!("PID {}, the bound process of hold {claim_id}", process.pid)
        }
        StopTarget::Foreign { source, .. } => format!("{source}'s claim"),
    }
}

/// The targets of one of the holder's claims: a lease's run, a process-bound hold's process;
/// TTL holds and reservations have none.
fn target_of(c: &Claim) -> Option<StopTarget> {
    match (c.kind, &c.binding) {
        (ClaimKind::Lease, _) => c.holder.clone().map(|holder| StopTarget::RunGroup {
            claim_id: c.id.clone(),
            holder,
            child: c.child.clone(),
            group: c.group.clone(),
        }),
        (ClaimKind::Hold, Some(HoldBinding::Process { holder })) => Some(StopTarget::Process {
            claim_id: c.id.clone(),
            process: holder.clone(),
        }),
        _ => None,
    }
}

impl ClaimsEngine {
    pub fn seize_begin(
        &mut self,
        s: SeizeBegin,
        heavy: &HeavyView,
    ) -> Result<SeizePlan, HostlerError> {
        if self.stopping {
            return Err(service_stopping());
        }
        let now = self.now();
        self.plans.retain(|p| p.expires_at_ms > now);
        let sources = self.refresh_sources();
        let catalog = sources.catalog;
        self.check_request(&s.request, &catalog)?;
        let info = info_for(&s.resource, &catalog);
        let display = info.display.clone();
        if !rules::candidates(&s.request.selector, &catalog).contains(&s.resource) {
            return Err(bad_request(format!(
                "the takeover's request does not ask for {display}"
            )));
        }
        let token = token_of(&info);
        if s.confirmation != token {
            return Err(bad_request(format!("--confirmation must be {token}")));
        }
        if self.plans.iter().any(|p| p.request.id == s.request.id) {
            return Err(bad_request(format!(
                "request {} already has a takeover plan",
                s.request.id
            )));
        }
        if self.plans.iter().any(|p| p.resource == s.resource) {
            return Err(errors::seizure_failed(
                &display,
                "another takeover of it is in progress",
            ));
        }
        if self.store.claim(&s.request.id).map_err(internal)?.is_some()
            || self
                .store
                .history_item_of(&s.request.id)
                .map_err(internal)?
                .is_some()
        {
            return Err(bad_request(format!(
                "request {} is no longer waiting",
                s.request.id
            )));
        }

        let data = self.load()?;
        let taker_ws = s.request.workspace_id.clone();
        let held: Vec<&Claim> = data
            .claims
            .iter()
            .filter(|c| c.resource.id == s.resource && c.workspace.id != taker_ws)
            .collect();
        let holders: HashSet<&str> = held.iter().map(|c| c.workspace.id.as_str()).collect();
        if holders.len() > 1 {
            return Err(internal(anyhow::anyhow!(
                "{display} has claims of {} workspaces",
                holders.len()
            )));
        }
        // P2-10: the taker's own sessions are never stopped; agent-device would refuse its
        // next session otherwise only for others.
        let foreign: Vec<ForeignClaim> = data
            .foreign
            .iter()
            .filter(|f| f.resource == s.resource && blocks_taker(f, &taker_ws))
            .cloned()
            .collect();
        // L54: a session kept from the last good read cannot be closed while the status is
        // unreadable (nothing could see it gone); a person may release the device instead.
        let unreadable_why = sources
            .unreadable
            .as_deref()
            .filter(|_| s.resource.kind() == Some(ResourceKind::Device));
        let refusal = match (foreign.iter().find(|f| is_kept(f)), unreadable_why) {
            (Some(f), _) => Some((
                format!(
                    "{}; {}, so hostler cannot close it",
                    f.description,
                    if sources.unreadable.is_some() {
                        "agent-device's status cannot be read"
                    } else {
                        // P3bT9b-11: after a restart, before the first read.
                        "agent-device's status has not been read since the service started"
                    }
                ),
                format!(
                    "hostler devices tools --detect; or a person releases it: hostler devices \
                     forget-sessions {display}"
                ),
            )),
            // P3bT9b-3: refused before anything is stopped; the commit could not pass without
            // a good read, and the holder's work would be lost for nothing.
            (None, Some(why)) if !held.is_empty() || !foreign.is_empty() => Some((
                format!(
                    "agent-device status unreadable: {why}; the takeover could not see the \
                     device free, so nothing is stopped"
                ),
                format!(
                    "{}; then take it over again",
                    crate::devices::agent_device::STATUS_FIX
                ),
            )),
            _ => None,
        };
        if let Some((why, recovery)) = refusal {
            let mut e = errors::seizure_failed(&display, &why);
            e.recovery = Some(recovery);
            return Err(e);
        }
        if let Some(f) = foreign.iter().find(|f| f.stop.is_none()) {
            return Err(errors::seizure_failed(
                &display,
                &format!("{} holds it and hostler cannot stop it", f.source),
            ));
        }
        if held.is_empty() && foreign.is_empty() {
            return Err(bad_request(format!(
                "nothing to take over: {display} is free or yours"
            )));
        }

        let mut stops: Vec<StopTarget> = held.iter().filter_map(|c| target_of(c)).collect();
        let mut covered: HashSet<String> = held.iter().map(|c| c.id.clone()).collect();
        let table = self.table.clone();
        let mut lookup = Ancestry::new(table.as_ref());
        let heavy = lookup.heavy(heavy, &self.waiting_heavy_holders(heavy));
        stops.extend(self.nested_runs(&stops, &mut covered, &data, &heavy, &mut lookup));
        stops.extend(foreign.iter().map(|f| StopTarget::Foreign {
            source: f.source.clone(),
            resource: f.resource.clone(),
            data: f.stop.clone().unwrap_or_default(),
        }));
        let stranded = !stops.is_empty()
            && stops
                .iter()
                .all(|t| !matches!(t, StopTarget::Foreign { .. }) && self.gone(t, None).is_ok());

        let plan_id = uuid::Uuid::new_v4().to_string();
        let expires_at_ms = now + PLAN_TTL_MS;
        let by_workspace = data
            .paths
            .get(&taker_ws)
            .cloned()
            .unwrap_or_else(|| taker_ws.clone());
        let plan = SeizePlan {
            plan_id: plan_id.clone(),
            claims: held.iter().map(|c| (*c).clone()).collect(),
            foreign: foreign.clone(),
            stops: stops.clone(),
            expires_at_ms,
        };
        self.plans.push(Plan {
            resource: s.resource,
            by_workspace,
            request: s.request,
            expires_at_ms,
            detail: Detail {
                plan_id,
                display,
                claim_ids: held.iter().map(|c| c.id.clone()).collect(),
                stops,
                foreign: foreign.clone(),
                stranded,
            },
        });
        self.publish();
        Ok(plan)
    }

    /// L18/L28: every live lease (on any resource) and running heavy entry nested under
    /// `stops` — its holder descends from a target's process, or its stored `nested_in` names
    /// a target — until none is new. `covered` holds the ids already planned and grows.
    fn nested_runs(
        &self,
        stops: &[StopTarget],
        covered: &mut HashSet<String>,
        data: &Data,
        heavy: &HeavyView,
        lookup: &mut Ancestry<'_>,
    ) -> Vec<StopTarget> {
        // Seeded from live identities only (CR-A-1): a dead holder's bare pid may be a stranger's.
        let mut pids: HashSet<u32> = stops.iter().flat_map(|t| self.live_pids_of(t)).collect();
        for t in stops {
            if let StopTarget::RunGroup { claim_id, .. } | StopTarget::Process { claim_id, .. } = t
            {
                covered.insert(claim_id.clone());
            }
        }
        let leases: Vec<(&Claim, Vec<u32>)> = data
            .claims
            .iter()
            .filter(|c| c.kind == ClaimKind::Lease && self.lease_live(c))
            // From a live identity only (L47): the holder, else the child, else no chain (a
            // dead-wrapper lease keeps its real nesting through `nested_in`).
            .map(|c| {
                let from = [c.holder.as_ref(), c.child.as_ref()]
                    .into_iter()
                    .flatten()
                    .find(|p| self.live(p));
                (c, from.map(|p| lookup.chain(p.pid)).unwrap_or_default())
            })
            .collect();
        // Any enclosing run recorded at acquire, not only the nearest (T6-R2-4).
        let linked = |id: &str, covered: &HashSet<String>| {
            data.nesting
                .get(id)
                .is_some_and(|outer| outer.iter().any(|o| covered.contains(o)))
        };
        let mut found = Vec::new();
        loop {
            let mut added = false;
            for (c, chain) in &leases {
                if !covered.contains(&c.id)
                    && (chain.iter().any(|p| pids.contains(p)) || linked(&c.id, covered))
                {
                    let target = target_of(c).expect("a lease with a holder");
                    pids.extend(self.live_pids_of(&target));
                    covered.insert(c.id.clone());
                    found.push(target);
                    added = true;
                }
            }
            for h in &heavy.running {
                let id = format!("heavy:{}", h.id);
                if !covered.contains(&id)
                    && (h.holder_ancestry.iter().any(|p| pids.contains(p)) || linked(&id, covered))
                {
                    let target = StopTarget::RunGroup {
                        claim_id: id.clone(),
                        holder: h.holder.clone(),
                        child: h.child.clone(),
                        group: h.group.clone(),
                    };
                    pids.extend(self.live_pids_of(&target));
                    covered.insert(id);
                    found.push(target);
                    added = true;
                }
            }
            if !added {
                return found;
            }
        }
    }

    /// The pids of a target's processes that still live (CR-A-1): only those can have runs
    /// nested under them; a dead one's pid may have been reused.
    fn live_pids_of(&self, t: &StopTarget) -> Vec<u32> {
        let live = |p: &ProcessIdentity| self.live(p).then_some(p.pid);
        match t {
            StopTarget::RunGroup { holder, child, .. } => live(holder)
                .into_iter()
                .chain(child.iter().filter_map(live))
                .collect(),
            StopTarget::Process { process, .. } => live(process).into_iter().collect(),
            StopTarget::Foreign { .. } => Vec::new(),
        }
    }

    /// The live plans, expired ones dropped.
    fn live_plans_now(&self) -> impl Iterator<Item = &Plan> {
        let now = self.now();
        self.plans.iter().filter(move |p| p.expires_at_ms > now)
    }

    /// L27: whether `id` (a claim id or `heavy:<id>`) is in a live plan.
    pub(crate) fn planned_claim(&self, id: &str) -> bool {
        self.live_plans_now().any(|p| {
            p.detail.claim_ids.iter().any(|c| c == id)
                || p.detail.stops.iter().any(|t| {
                    matches!(t, StopTarget::RunGroup { claim_id, .. }
                        | StopTarget::Process { claim_id, .. } if claim_id == id)
                })
        })
    }

    /// L28: the planned target among these processes (an ancestry, pid first), if any.
    pub(crate) fn planned_ancestor(&self, ancestry: &[u32]) -> Option<String> {
        self.live_plans_now().find_map(|p| {
            p.detail
                .stops
                .iter()
                .find(|t| {
                    self.live_pids_of(t)
                        .iter()
                        .any(|pid| ancestry.contains(pid))
                })
                .map(describe)
        })
    }

    /// L28 for the heavy queue (`heavy.acquire`, under the heavy lock, then this one): refused
    /// under a planned target; else the enclosing run to record as the entry's `nested_in`.
    pub fn heavy_admission(
        &self,
        holder: &ProcessIdentity,
        heavy: &HeavyView,
    ) -> Result<Option<String>, HostlerError> {
        let table = self.table.clone();
        let mut lookup = Ancestry::new(table.as_ref());
        let (data, ancestry, heavy) = self.load_for(Some(&mut lookup), Some(holder.pid), heavy)?;
        if let Some(target) = self.planned_ancestor(&ancestry) {
            return Err(being_taken_over(&format!(
                "the run this heavy task comes from ({target})"
            )));
        }
        Ok(enclosing(&ancestry, &data, &heavy, &|p| self.live(p)))
    }

    /// Records a heavy entry's enclosing run (L28).
    pub fn record_heavy_nesting(&self, entry_id: &str, nested_in: &str) {
        if let Err(e) = self.store.set_heavy_nested_in(entry_id, nested_in) {
            tracing::warn!("cannot record heavy entry {entry_id}'s enclosing run: {e:#}");
        }
    }

    /// L27 for `heavy.started`: a heavy entry being taken over, or nested under a run being taken
    /// over (T6-R3-1), never starts its command.
    pub fn heavy_start_allowed(
        &self,
        entry_id: &str,
        child: &ProcessIdentity,
    ) -> Result<(), HostlerError> {
        if self.start_refused(&format!("heavy:{entry_id}"), child) {
            return Err(being_taken_over(&format!("heavy entry {entry_id}")));
        }
        Ok(())
    }

    /// L27, T6-R3-1: whether the run of `id` (a claim id or `heavy:<id>`) starting `child` must
    /// not start: it is planned itself, or it runs under a planned target — by the child's live
    /// ancestry (one walk; the parent is the wrapper), or by the runs it was recorded under. A
    /// run queued before the plan and granted during it is caught here.
    pub(crate) fn start_refused(&self, id: &str, child: &ProcessIdentity) -> bool {
        if self.live_plans_now().next().is_none() {
            return false;
        }
        if self.planned_claim(id) {
            return true;
        }
        let table = self.table.clone();
        let chain = Ancestry::new(table.as_ref()).chain(child.pid);
        if self.planned_ancestor(&chain).is_some() {
            return true;
        }
        match self.store.nestings() {
            Ok(nesting) => nesting
                .get(id)
                .is_some_and(|outer| parse_nesting(outer).iter().any(|o| self.planned_claim(o))),
            // Unknown: refused, as nothing may start under a takeover in doubt.
            Err(_) => true,
        }
    }

    /// T6-7: the live plans, for an idle stop's refusal.
    pub fn plan_summaries(&self) -> Vec<serde_json::Value> {
        self.live_plans_now()
            .map(|p| {
                serde_json::json!({
                    "plan_id": p.detail.plan_id,
                    "resource": p.detail.display,
                    "by_workspace": p.by_workspace,
                })
            })
            .collect()
    }

    /// L29: the taker keeps its plan alive while it stops target after target.
    pub fn seize_progress(
        &mut self,
        plan_id: &str,
        requester: &ProcessIdentity,
    ) -> Result<i64, HostlerError> {
        let now = self.now();
        self.plans.retain(|p| p.expires_at_ms > now);
        let plan = self
            .plans
            .iter_mut()
            .find(|p| p.detail.plan_id == plan_id)
            .ok_or_else(|| no_plan(plan_id))?;
        if plan.request.requester != *requester {
            return Err(bad_request(
                "only the process that began this takeover may extend it".into(),
            ));
        }
        plan.expires_at_ms = now + PLAN_TTL_MS;
        Ok(plan.expires_at_ms)
    }

    /// `Ok` when target `t` is gone — verified, or only on the taker's word (L30); else why not.
    /// `evidence` is the taker's for it.
    fn gone(&self, t: &StopTarget, evidence: Option<&StopEvidence>) -> Result<Proof, String> {
        let still = || format!("{} is still running", describe(t));
        match t {
            StopTarget::RunGroup {
                child: Some(child),
                group,
                ..
            } => {
                if self.live(child) {
                    return Err(still());
                }
                let Some(group) = group else {
                    return Ok(Proof::Verified);
                };
                match procs::group_state(group, self.table.as_ref()) {
                    JobState::Gone => Ok(Proof::Verified),
                    // Another session's job whose name went with its last handle, and its
                    // launcher is dead (checked above): gone, as in its own session (P2-11, L31).
                    JobState::NotFound => Ok(Proof::Verified),
                    JobState::Alive => Err(still()),
                    JobState::Unknown => {
                        let seen = match (group, evidence) {
                            (
                                RunGroup::WindowsJob {
                                    session: Some(recorded),
                                    ..
                                },
                                Some(StopEvidence::JobGone { session, .. }),
                            ) => recorded == session,
                            _ => false,
                        };
                        if seen {
                            Ok(Proof::Attested)
                        } else {
                            Err(format!(
                                "{} cannot be checked: its Windows job is not visible to the \
                                 service",
                                describe(t)
                            ))
                        }
                    }
                }
            }
            StopTarget::RunGroup {
                holder,
                child: None,
                ..
            } => {
                if self.live(holder) {
                    Err(still())
                } else {
                    Ok(Proof::Verified)
                }
            }
            StopTarget::Process { process, .. } => {
                if self.live(process) {
                    Err(still())
                } else {
                    Ok(Proof::Verified)
                }
            }
            // Checked by re-collecting the foreign sources.
            StopTarget::Foreign { .. } => Ok(Proof::Verified),
        }
    }

    pub fn seize_commit(
        &mut self,
        c: SeizeCommit,
        heavy: &HeavyView,
    ) -> Result<SeizeOutcome, HostlerError> {
        self.seize_commit_after_read(c, heavy, &Ok(()))
    }

    /// `seize_commit` after the server's fresh agent-device read (P-2). The commit grants the
    /// taker directly, past the freshness gate, so a failed read refuses a commit on a device
    /// that ends a hostler claim, stops a foreign one, or carries a kept session (P3-1).
    pub fn seize_commit_after_read(
        &mut self,
        c: SeizeCommit,
        heavy: &HeavyView,
        read: &Result<(), String>,
    ) -> Result<SeizeOutcome, HostlerError> {
        let now = self.now();
        self.plans.retain(|p| p.expires_at_ms > now);
        let Some(index) = self
            .plans
            .iter()
            .position(|p| p.detail.plan_id == c.plan_id)
        else {
            return Err(no_plan(&c.plan_id));
        };
        if self.plans[index].request.requester != c.requester {
            // The real taker's plan stays.
            return Err(bad_request(
                "only the process that began this takeover may commit it".into(),
            ));
        }
        let plan = self.plans.remove(index);
        if self.stopping {
            self.publish();
            return Err(service_stopping());
        }
        if let Err(why) = read
            && plan.resource.kind() == Some(ResourceKind::Device)
        {
            let kept = self
                .refresh_sources()
                .foreign
                .iter()
                .any(|f| f.resource == plan.resource && is_kept(f));
            let foreign_stops = plan
                .detail
                .stops
                .iter()
                .any(|t| matches!(t, StopTarget::Foreign { .. }));
            if !plan.detail.claim_ids.is_empty() || foreign_stops || kept {
                self.settle(heavy);
                return Err(errors::seizure_failed(
                    &plan.detail.display,
                    &format!("agent-device status unreadable: {why}"),
                ));
            }
        }
        match self.verify(&plan, &c.evidence, heavy) {
            Ok((held, attested)) => self.commit(plan, held, attested, &c, heavy),
            Err(why) => {
                self.settle(heavy);
                Err(errors::seizure_failed(&plan.detail.display, &why))
            }
        }
    }

    /// Steps 2–5 of a commit: the holder's claims on the resource when every check passes, and
    /// whether a target passed only on the taker's word (L30).
    fn verify(
        &self,
        plan: &Plan,
        evidence: &[StopEvidence],
        heavy: &HeavyView,
    ) -> Result<(Vec<Claim>, bool), String> {
        let table = self.table.clone();
        let mut lookup = Ancestry::new(table.as_ref());
        let data = self.load().map_err(|e| e.message)?;
        let heavy = lookup.heavy(heavy, &self.waiting_heavy_holders(heavy));
        let mut attested = false;
        for (k, t) in plan.detail.stops.iter().enumerate() {
            let mine = evidence.iter().find(|e| match e {
                StopEvidence::Gone { target } | StopEvidence::JobGone { target, .. } => {
                    *target == k
                }
            });
            // The target as planned (what the taker stopped), and as it stands now: a child
            // registered since `seize_begin` must be gone too (L27).
            attested |= self.gone(t, mine)? == Proof::Attested;
            if let Some(now) = current_target(t, &data, &heavy)
                && now != *t
            {
                attested |= self.gone(&now, mine)? == Proof::Attested;
            }
        }
        // L28: no live run nested under the targets that the plan does not cover.
        let mut covered: HashSet<String> = plan.detail.claim_ids.iter().cloned().collect();
        if let Some(new) = self
            .nested_runs(&plan.detail.stops, &mut covered, &data, &heavy, &mut lookup)
            .into_iter()
            .find(|t| self.gone(t, None).is_err())
        {
            return Err(format!(
                "{} runs under the holder's runs and was not part of the takeover",
                describe(&new)
            ));
        }
        // Read again now, not from the last collection: no foreign claim but the taker's own
        // may be left on the device, planned or new (P2-10).
        let listed: Vec<ForeignClaim> = self.refresh_sources().foreign;
        if let Some(f) = listed
            .iter()
            .find(|f| f.resource == plan.resource && blocks_taker(f, &plan.request.workspace_id))
        {
            return Err(format!("{} still holds it", f.source));
        }
        let held: Vec<Claim> = data
            .claims
            .iter()
            .filter(|cl| {
                cl.resource.id == plan.resource && cl.workspace.id != plan.request.workspace_id
            })
            .cloned()
            .collect();
        if let Some(extra) = held
            .iter()
            .find(|cl| !plan.detail.claim_ids.contains(&cl.id))
        {
            return Err(format!(
                "{} took a claim on it ({}) after the takeover began",
                extra.workspace.path, extra.id
            ));
        }
        Ok((held, attested))
    }

    fn commit(
        &mut self,
        plan: Plan,
        held: Vec<Claim>,
        attested: bool,
        c: &SeizeCommit,
        heavy: &HeavyView,
    ) -> Result<SeizeOutcome, HostlerError> {
        let now = self.now();
        let r = &plan.request;
        let pending = self
            .store
            .claim_requests()
            .map_err(internal)?
            .into_iter()
            .any(|p| p.id == r.id);
        if !pending
            && self
                .store
                .history_item_of(&r.id)
                .map_err(internal)?
                .is_some()
        {
            self.settle(heavy);
            return Err(bad_request(format!(
                "the takeover's request {} ended before the commit",
                r.id
            )));
        }
        if !held.is_empty() {
            let all = self.load()?.claims;
            let mut ended = HashSet::new();
            let mut ids = Vec::new();
            for claim in &held {
                for id in with_linked(&claim.id, &all) {
                    if ended.insert(id.clone()) {
                        ids.push(id);
                    }
                }
            }
            // Their history `detail` is the taker's workspace path (`seized_by`).
            self.end_claims_as(
                &ids,
                crate::contract::claims::EndReason::Seized,
                None,
                Some(&plan.by_workspace),
            )
            .map_err(internal)?;
            let root = held
                .iter()
                .find(|cl| cl.parent.is_none())
                .unwrap_or(&held[0]);
            let record = SeizureRecord {
                id: uuid::Uuid::new_v4().to_string(),
                resource: plan.resource.0.clone(),
                from_workspace_path: root.workspace.path.clone(),
                from_kind: root.kind.into(),
                from_source: None,
                from_pid: rules::summary(root, &all).pid,
                by_workspace_path: plan.by_workspace.clone(),
                by_pid: c.requester.pid,
                at_ms: now,
                stranded: plan.detail.stranded,
                // L30: what the service could not verify is the taker's assertion.
                forced: attested,
            };
            self.store.record_seizure(&record).map_err(internal)?;
            let _ = self.events.send(Event::ClaimsSeized(SeizureRecord {
                resource: plan.detail.display.clone(),
                ..record
            }));
        }
        // Every foreign claim it stopped: one record each, from its source (P2-12).
        for f in &plan.detail.foreign {
            let record = SeizureRecord {
                id: uuid::Uuid::new_v4().to_string(),
                resource: plan.resource.0.clone(),
                from_workspace_path: f
                    .workspace_path
                    .clone()
                    .unwrap_or_else(|| "an unknown workspace".into()),
                from_kind: SeizedFrom::Foreign,
                from_source: Some(f.source.clone()),
                from_pid: None,
                by_workspace_path: plan.by_workspace.clone(),
                by_pid: c.requester.pid,
                at_ms: now,
                stranded: false,
                forced: attested,
            };
            self.store.record_seizure(&record).map_err(internal)?;
            let _ = self.events.send(Event::ClaimsSeized(SeizureRecord {
                resource: plan.detail.display.clone(),
                ..record
            }));
        }
        // The taker goes ahead of every waiter.
        let binding = binding_for(&r.purpose, now);
        if pending {
            self.store
                .grant(
                    &r.id,
                    &plan.resource,
                    claim_kind(&r.purpose),
                    None,
                    binding.as_ref(),
                    now,
                )
                .map_err(internal)?;
        } else {
            self.store
                .insert_claim(&ClaimRow {
                    id: r.id.clone(),
                    resource: plan.resource.clone(),
                    kind: claim_kind(&r.purpose),
                    parent: None,
                    workspace_id: r.workspace_id.clone(),
                    holder: Some(r.requester.clone()),
                    child: None,
                    group: None,
                    binding,
                    command: match &r.purpose {
                        Purpose::Lease { command } => Some(command.clone()),
                        Purpose::Hold { .. } => None,
                    },
                    granted_at_ms: now,
                })
                .map_err(internal)?;
        }
        self.settle(heavy);
        let outcome = self
            .known(&r.id)?
            .ok_or_else(|| internal(anyhow::anyhow!("the taker's claim is missing")))?;
        Ok(SeizeOutcome { outcome, attested })
    }

    /// Idempotent: an unknown or expired plan is already gone.
    pub fn seize_abort(&mut self, plan_id: &str) -> Result<(), HostlerError> {
        let before = self.plans.len();
        self.plans.retain(|p| p.detail.plan_id != plan_id);
        if self.plans.len() != before {
            self.settle(&no_heavy());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::{Arc, Mutex};

    use tokio::sync::broadcast;

    use super::super::no_heavy;
    use super::*;
    use crate::claims::rules::HeavyRunning;
    use crate::claims::{ForeignClaimSource, ResourceCatalog, ScreenCatalog};
    use crate::contract::ErrorCode;
    use crate::contract::claims::ResourceId;
    use crate::contract::claims::{
        ClaimOutcome, ClaimRelease, ClaimRequest, EndReason, ResourceInfo, ResourceKind, Selector,
    };
    use crate::heavy::engine::Clock;
    use crate::procs::ProcessIdentity;
    use crate::procs::fake::FakeTable;
    use crate::project::display_path;
    use crate::store::Store;

    const BOOT: &str = "b1";

    struct TestClock(AtomicI64);

    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct Devices;

    impl ResourceCatalog for Devices {
        fn resources(&self) -> Vec<ResourceInfo> {
            (0..2)
                .map(|i| ResourceInfo {
                    id: dev(i),
                    kind: ResourceKind::Device,
                    display: format!("fake-{i}"),
                    attributes: BTreeMap::new(),
                    available: true,
                })
                .collect()
        }
    }

    #[derive(Clone, Default)]
    struct Foreign(Arc<Mutex<Vec<ForeignClaim>>>);

    impl ForeignClaimSource for Foreign {
        fn foreign(&self) -> Vec<ForeignClaim> {
            self.0.lock().unwrap().clone()
        }
    }

    fn dev(i: u32) -> ResourceId {
        ResourceId(format!("device:test:{i}"))
    }

    fn screen() -> ResourceId {
        ResourceId::screen()
    }

    struct World {
        engine: ClaimsEngine,
        table: Arc<FakeTable>,
        clock: Arc<TestClock>,
        foreign: Foreign,
        dir: tempfile::TempDir,
        events: broadcast::Receiver<Event>,
    }

    fn world() -> World {
        let table = Arc::new(FakeTable::new(BOOT));
        let clock = Arc::new(TestClock(AtomicI64::new(100 * 60_000)));
        let (tx, events) = broadcast::channel(4096);
        let foreign = Foreign::default();
        let engine = ClaimsEngine::new(
            Store::open_in_memory().unwrap(),
            table.clone(),
            clock.clone(),
            vec![Box::new(ScreenCatalog), Box::new(Devices)],
            vec![Box::new(foreign.clone())],
            tx,
        );
        World {
            engine,
            table,
            clock,
            foreign,
            dir: tempfile::tempdir().unwrap(),
            events,
        }
    }

    impl World {
        fn now(&self) -> i64 {
            self.clock.now_ms()
        }

        fn proc(&self, pid: u32, parent: Option<u32>) -> ProcessIdentity {
            self.table.insert(pid, parent, Some(u64::from(pid) * 10));
            ProcessIdentity {
                pid,
                boot_id: BOOT.into(),
                start_time: u64::from(pid) * 10,
            }
        }

        fn workspace(&self, name: &str) -> String {
            let root = self.dir.path().join(name);
            std::fs::create_dir_all(root.join(".git")).unwrap();
            let common = display_path(&root.join(".git"));
            let project = self
                .engine
                .store
                .upsert_project(&common, name, self.now())
                .unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            self.engine
                .store
                .upsert_workspace(&id, &project.id, &display_path(&root), &common, self.now())
                .unwrap();
            id
        }

        fn request(&self, ws: &str, pid: u32, r: ResourceId, purpose: Purpose) -> ClaimRequest {
            let me = self.proc(pid, None);
            ClaimRequest {
                id: uuid::Uuid::new_v4().to_string(),
                selector: Selector::Exact { resource: r },
                workspace_id: ws.into(),
                requester: me,
                purpose,
                reentrant_tokens: Vec::new(),
                no_wait: false,
            }
        }

        fn lease(&self, ws: &str, pid: u32, r: ResourceId) -> ClaimRequest {
            self.request(
                ws,
                pid,
                r,
                Purpose::Lease {
                    command: "make test".into(),
                },
            )
        }

        fn ttl_hold(&self, ws: &str, r: ResourceId) -> ClaimRequest {
            self.request(
                ws,
                900,
                r,
                Purpose::Hold {
                    ttl_ms: Some(30 * 60_000),
                    bound_to: None,
                    distinct: false,
                },
            )
        }

        fn acquire(&mut self, r: &ClaimRequest) -> ClaimOutcome {
            self.engine.acquire(r.clone(), &no_heavy()).unwrap()
        }

        /// A granted lease whose command runs as `pid + 1` in group `pid + 1`.
        fn running(&mut self, ws: &str, pid: u32, r: ResourceId) -> (ClaimRequest, RunGroup) {
            let req = self.lease(ws, pid, r);
            assert!(matches!(
                self.acquire(&req),
                ClaimOutcome::Granted { .. } | ClaimOutcome::Joined { .. }
            ));
            let group = self.start(&req.id, pid + 1, pid);
            (req, group)
        }

        fn start(&mut self, claim_id: &str, child: u32, parent: u32) -> RunGroup {
            let child = self.proc(child, Some(parent));
            let group = RunGroup::UnixGroup {
                pgid: child.pid,
                boot_id: BOOT.into(),
            };
            self.table.set_group_alive(&group, true);
            self.engine
                .started(crate::contract::claims::ClaimStarted {
                    claim_id: claim_id.into(),
                    child,
                    group: group.clone(),
                })
                .unwrap();
            group
        }

        fn kill_run(&self, pid: u32, group: &RunGroup) {
            self.table.remove(pid);
            self.table.remove(pid + 1);
            self.table.set_group_alive(group, false);
        }

        fn begin(
            &mut self,
            taker: &ClaimRequest,
            r: &ResourceId,
        ) -> Result<SeizePlan, HostlerError> {
            self.begin_with(taker, r, &no_heavy())
        }

        fn begin_with(
            &mut self,
            taker: &ClaimRequest,
            r: &ResourceId,
            heavy: &HeavyView,
        ) -> Result<SeizePlan, HostlerError> {
            let token = token_of(&info_for(r, &self.engine.catalog()));
            self.engine.seize_begin(
                SeizeBegin {
                    resource: r.clone(),
                    confirmation: token,
                    request: taker.clone(),
                },
                heavy,
            )
        }

        fn commit(
            &mut self,
            plan: &SeizePlan,
            taker: &ClaimRequest,
            evidence: Vec<StopEvidence>,
        ) -> Result<ClaimOutcome, HostlerError> {
            self.commit_with(plan, taker, evidence, &no_heavy())
                .map(|o| o.outcome)
        }

        fn commit_with(
            &mut self,
            plan: &SeizePlan,
            taker: &ClaimRequest,
            evidence: Vec<StopEvidence>,
            heavy: &HeavyView,
        ) -> Result<SeizeOutcome, HostlerError> {
            self.engine.seize_commit(
                SeizeCommit {
                    plan_id: plan.plan_id.clone(),
                    requester: taker.requester.clone(),
                    evidence,
                },
                heavy,
            )
        }

        fn ended(&self, id: &str) -> Option<EndReason> {
            self.engine
                .store
                .history_item_of(id)
                .unwrap()
                .map(|i| i.reason)
        }

        fn seizures(&mut self) -> Vec<SeizureRecord> {
            let mut out = Vec::new();
            while let Ok(e) = self.events.try_recv() {
                if let Event::ClaimsSeized(s) = e {
                    out.push(s);
                }
            }
            out
        }

        fn seizing(&self, r: &ResourceId) -> bool {
            self.engine
                .snapshot()
                .unwrap()
                .resources
                .iter()
                .any(|s| s.resource.id == *r && s.seizing)
        }

        fn holder_of(&self, r: &ResourceId) -> Vec<String> {
            self.engine
                .store
                .claims()
                .unwrap()
                .into_iter()
                .filter(|c| c.resource == *r)
                .map(|c| c.id)
                .collect()
        }
    }

    fn granted_id(o: ClaimOutcome) -> String {
        match o {
            ClaimOutcome::Granted { claim } => claim.id,
            other => panic!("not granted: {other:?}"),
        }
    }

    fn failed(e: HostlerError) -> String {
        assert_eq!(e.code, ErrorCode::SeizureFailed, "{e:?}");
        e.message
    }

    // ---- fix round 1 (L27–L30) ----

    /// L27: a claim in a live plan never starts its command; after the plan it may.
    #[test]
    fn started_during_a_plan_is_refused() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let held = w.lease(&a, 100, screen());
        w.acquire(&held);
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        let child = w.proc(101, Some(100));
        let group = RunGroup::UnixGroup {
            pgid: 101,
            boot_id: BOOT.into(),
        };
        let started = |w: &mut World| {
            w.engine.started(crate::contract::claims::ClaimStarted {
                claim_id: held.id.clone(),
                child: child.clone(),
                group: group.clone(),
            })
        };
        let e = started(&mut w).unwrap_err();
        assert_eq!(e.code, ErrorCode::TakenOver);
        assert!(e.message.contains("is being taken over"), "{}", e.message);
        assert!(
            w.engine
                .store
                .claim(&held.id)
                .unwrap()
                .unwrap()
                .child
                .is_none()
        );
        w.engine.seize_abort(&plan.plan_id).unwrap();
        started(&mut w).unwrap();
    }

    /// L27: the commit checks a target as it stands now: a child registered after
    /// `seize_begin` (here written past the refusal) must be gone too.
    #[test]
    fn commit_checks_a_child_registered_after_begin() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let held = w.lease(&a, 100, screen());
        w.acquire(&held);
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        let child = w.proc(101, Some(100));
        let group = RunGroup::UnixGroup {
            pgid: 101,
            boot_id: BOOT.into(),
        };
        w.table.set_group_alive(&group, true);
        w.engine
            .store
            .set_claim_started(&held.id, &child, &group)
            .unwrap();
        // The planned target (the wrapper) is stopped; the command it started is not.
        w.table.remove(100);
        let why = failed(w.commit(&plan, &taker, Vec::new()).unwrap_err());
        assert!(why.contains("(PID 101) is still running"), "{why}");
        assert_eq!(w.holder_of(&screen()), vec![held.id.clone()]);
    }

    /// L27 for heavy: an L18 heavy target never starts its command during the plan.
    #[test]
    fn heavy_target_cannot_start_during_a_plan() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        w.running(&a, 100, screen());
        let heavy = HeavyView {
            capacity: 1,
            running: vec![HeavyRunning {
                id: "h1".into(),
                holder: w.proc(150, Some(101)),
                child: None,
                group: None,
                holder_ancestry: vec![150],
            }],
            waiting: Vec::new(),
        };
        let taker = w.lease(&b, 700, screen());
        assert!(
            w.engine
                .heavy_start_allowed("h1", &w.proc(999, None))
                .is_ok()
        );
        let plan = w.begin_with(&taker, &screen(), &heavy).unwrap();
        assert!(plan.stops.iter().any(|t| matches!(t,
            StopTarget::RunGroup { claim_id, .. } if claim_id == "heavy:h1")));
        let e = w
            .engine
            .heavy_start_allowed("h1", &w.proc(999, None))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::TakenOver);
        assert!(
            w.engine
                .heavy_start_allowed("h2", &w.proc(998, None))
                .is_ok()
        );
    }

    /// T6-R3-1: a run queued under a target before the plan and granted during it never starts
    /// its command — found through its child's ancestry, or through its recorded links.
    #[test]
    fn a_run_granted_during_the_plan_under_a_target_cannot_start() {
        let mut w = world();
        let (a, b, c) = (w.workspace("a"), w.workspace("b"), w.workspace("c"));
        w.running(&a, 100, screen());
        // c holds fake-0; a nested run of a's command waits for it.
        let c_hold = w.ttl_hold(&c, dev(0));
        w.acquire(&c_hold);
        let nested = w.lease(&a, 150, dev(0));
        w.proc(150, Some(101));
        assert!(matches!(w.acquire(&nested), ClaimOutcome::Queued { .. }));
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        // c's hold ends: the nested run is granted while the plan lives.
        w.engine
            .release(ClaimRelease {
                claim_id: c_hold.id.clone(),
                workspace_id: c.clone(),
                requester: c_hold.requester.clone(),
                exit_code: None,
            })
            .unwrap();
        assert!(matches!(
            w.engine.known(&nested.id).unwrap(),
            Some(ClaimOutcome::Granted { .. })
        ));
        let started = |w: &mut World, child: ProcessIdentity| {
            w.engine.started(crate::contract::claims::ClaimStarted {
                claim_id: nested.id.clone(),
                group: RunGroup::UnixGroup {
                    pgid: child.pid,
                    boot_id: BOOT.into(),
                },
                child,
            })
        };
        let child = w.proc(151, Some(150));
        assert_eq!(
            started(&mut w, child).unwrap_err().code,
            ErrorCode::TakenOver
        );
        // By the recorded link alone: a child no longer under the target's processes.
        let orphan = w.proc(152, Some(1));
        assert_eq!(
            started(&mut w, orphan.clone()).unwrap_err().code,
            ErrorCode::TakenOver
        );
        // A heavy run under the target's command neither; one elsewhere starts.
        let under = w.proc(161, Some(160));
        w.proc(160, Some(101));
        assert_eq!(
            w.engine.heavy_start_allowed("h9", &under).unwrap_err().code,
            ErrorCode::TakenOver
        );
        assert!(
            w.engine
                .heavy_start_allowed("h8", &w.proc(171, None))
                .is_ok()
        );
        // After the plan, all of it starts.
        w.engine.seize_abort(&plan.plan_id).unwrap();
        assert!(w.engine.heavy_start_allowed("h9", &under).is_ok());
        started(&mut w, orphan).unwrap();
    }

    /// CR-A-1: a stale parent pid (Windows keeps a dead parent's pid, now a stranger's) links
    /// nothing — no recorded nesting, no deadlock victim, no L18 target, no TakenOver.
    #[test]
    fn a_stale_parent_pid_links_no_run() {
        let mut w = world();
        let (ws, x, y) = (w.workspace("w"), w.workspace("x"), w.workspace("y"));
        // W's screen run: wrapper 4100, command 4101 (started at 41010).
        let (w_run, _) = w.running(&ws, 4100, screen());
        // X's agent started at 500; its recorded parent pid 4101 is W's command now.
        w.table.insert(7000, Some(4101), Some(500));
        let under_agent = |w: &World, pid: u32, r: ResourceId| {
            let req = w.lease(&x, pid, r);
            w.table.insert(pid, Some(7000), Some(u64::from(pid) * 10));
            req
        };
        // Waiting for the screen W holds is no self-wait: queued, not a deadlock victim.
        let waits = under_agent(&w, 8002, screen());
        assert!(matches!(w.acquire(&waits), ClaimOutcome::Queued { .. }));
        w.engine.withdraw(&waits.id, false).unwrap();
        // X's run on fake-0 is not recorded as nested under W's run.
        let x_run = under_agent(&w, 8000, dev(0));
        assert!(matches!(w.acquire(&x_run), ClaimOutcome::Granted { .. }));
        assert!(!w.engine.store.nestings().unwrap().contains_key(&x_run.id));
        // Y takes the screen over: X's run is no target, and X may go on meanwhile.
        let taker = w.lease(&y, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        assert_eq!(plan.stops.len(), 1, "{:?}", plan.stops);
        assert!(matches!(&plan.stops[0],
            StopTarget::RunGroup { claim_id, .. } if *claim_id == w_run.id));
        let more = under_agent(&w, 8001, dev(1));
        assert!(matches!(w.acquire(&more), ClaimOutcome::Granted { .. }));
        let child = w.proc(8003, Some(8000));
        w.engine
            .started(crate::contract::claims::ClaimStarted {
                claim_id: x_run.id.clone(),
                group: RunGroup::UnixGroup {
                    pgid: 8003,
                    boot_id: BOOT.into(),
                },
                child,
            })
            .unwrap();
    }

    /// FR-1/L47 (the review's case A): a lease whose wrapper died is walked from its live
    /// command, never from the dead wrapper's pid — which a process under W's command reuses.
    #[test]
    fn a_dead_wrappers_reused_pid_never_links_its_lease() {
        let mut w = world();
        let (ws, x, y) = (w.workspace("w"), w.workspace("x"), w.workspace("y"));
        w.running(&ws, 4100, screen());
        w.running(&x, 6000, dev(0));
        // X's wrapper dies: its command is reparented, and the pid goes to a process under W's.
        w.table.insert(6001, Some(1), Some(60010));
        w.table.insert(6000, Some(4101), Some(900_000));
        let plan = w.begin(&w.lease(&y, 700, screen()), &screen()).unwrap();
        assert_eq!(plan.stops.len(), 1, "{:?}", plan.stops);
    }

    /// FR-1/L47, heavy: a running heavy entry whose holder died stands for its live child.
    #[test]
    fn a_dead_heavy_holders_reused_pid_never_links_its_entry() {
        let mut w = world();
        let (ws, y) = (w.workspace("w"), w.workspace("y"));
        w.running(&ws, 4100, screen());
        let holder = w.proc(6000, None);
        let child = w.proc(6001, Some(6000));
        let heavy = HeavyView {
            capacity: 1,
            running: vec![HeavyRunning {
                id: "h1".into(),
                holder,
                child: Some(child),
                group: None,
                holder_ancestry: vec![6000],
            }],
            waiting: Vec::new(),
        };
        w.table.insert(6001, Some(1), Some(60010));
        w.table.insert(6000, Some(4101), Some(900_000));
        let plan = w
            .begin_with(&w.lease(&y, 700, screen()), &screen(), &heavy)
            .unwrap();
        assert_eq!(plan.stops.len(), 1, "{:?}", plan.stops);
    }

    /// FR-1/L47 (the review's case B): a dead wrapper's pid, reused by a stranger, counts in no
    /// deadlock rule — no false `claim_deadlock` for a requester that holds nothing.
    #[test]
    fn a_dead_wrappers_reused_pid_closes_no_cycle() {
        let mut w = world();
        let (ws, x, y) = (w.workspace("w"), w.workspace("x"), w.workspace("y"));
        w.running(&ws, 4100, dev(0));
        w.running(&x, 5000, dev(1));
        // W's wrapper dies (its command lives on); a stranger gets the pid.
        w.table.insert(4101, Some(1), Some(41010));
        w.table.insert(4100, Some(1), Some(900_000));
        let q2 = w.lease(&x, 5002, dev(0));
        w.table.insert(5002, Some(5001), Some(50020));
        assert!(matches!(w.acquire(&q2), ClaimOutcome::Queued { .. }));
        let q1 = w.lease(&y, 95_000, dev(1));
        w.table.insert(95_000, Some(4100), Some(950_000));
        let got = w.engine.acquire(q1, &no_heavy());
        assert!(matches!(got, Ok(ClaimOutcome::Queued { .. })), "{got:?}");
    }

    /// CR-A-1 (Unix): a lease whose wrapper died lives on through its command; the wrapper's pid,
    /// reused by an unrelated process, seeds no nested-run search.
    #[test]
    fn a_dead_holders_reused_pid_seeds_nothing() {
        let mut w = world();
        let (ws, x, y) = (w.workspace("w"), w.workspace("x"), w.workspace("y"));
        let (w_run, _) = w.running(&ws, 4100, screen());
        // The wrapper 4100 dies; a stranger gets its pid (started at 500, before X's run).
        w.table.insert(4100, Some(1), Some(500));
        let x_run = w.lease(&x, 9000, dev(0));
        w.table.insert(9000, Some(4100), Some(90000));
        assert!(matches!(w.acquire(&x_run), ClaimOutcome::Granted { .. }));
        let taker = w.lease(&y, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        assert_eq!(plan.stops.len(), 1, "{:?}", plan.stops);
        assert!(matches!(&plan.stops[0],
            StopTarget::RunGroup { claim_id, .. } if *claim_id == w_run.id));
    }

    /// L28: nothing new starts under a planned target while the plan lives.
    #[test]
    fn acquire_under_a_planned_target_is_refused_while_the_plan_lives() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        w.running(&a, 100, screen());
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        let nested = w.lease(&a, 150, dev(0));
        w.proc(150, Some(101));
        let e = w.engine.acquire(nested.clone(), &no_heavy()).unwrap_err();
        assert_eq!(e.code, ErrorCode::TakenOver);
        assert!(e.message.contains("is being taken over"), "{}", e.message);
        let holder = w.proc(160, Some(101));
        assert_eq!(
            w.engine
                .heavy_admission(&holder, &no_heavy())
                .unwrap_err()
                .code,
            ErrorCode::TakenOver
        );
        // An unrelated process is not affected.
        let other = w.lease(&b, 800, dev(1));
        assert!(w.engine.acquire(other, &no_heavy()).is_ok());
        w.engine.seize_abort(&plan.plan_id).unwrap();
        assert!(w.engine.acquire(nested, &no_heavy()).is_ok());
        // Outside a plan the heavy entry gets its enclosing run.
        let outer = w.engine.heavy_admission(&holder, &no_heavy()).unwrap();
        assert!(outer.is_some());
    }

    /// L28: a nested lease whose wrapper died (its command lives on, reparented) is found
    /// through the link recorded at acquire.
    #[test]
    fn orphaned_nested_lease_is_planned() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let (outer, _) = w.running(&a, 100, screen());
        let nested = w.lease(&a, 150, dev(0));
        w.proc(150, Some(101));
        w.acquire(&nested);
        w.start(&nested.id, 151, 150);
        assert_eq!(
            w.engine.store.nestings().unwrap().get(&nested.id),
            Some(&serde_json::to_string(&[&outer.id]).unwrap())
        );
        // The nested wrapper dies; its command is reparented to init.
        w.table.remove(150);
        w.table.insert(151, Some(1), Some(1510));
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        assert!(
            plan.stops.iter().any(|t| matches!(t,
                StopTarget::RunGroup { claim_id, .. } if *claim_id == nested.id)),
            "{:?}",
            plan.stops
        );
    }

    /// T6-R2-4: lease L1 → heavy run H → lease L2. When H ends first and L2's wrapper dies
    /// (its command lives on), L2 is still found through every run it was recorded under.
    #[test]
    fn a_chain_stays_whole_when_its_middle_run_ends() {
        use crate::contract::heavy::{HeavyAcquire, HeavyOutcome};

        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let (l1, g1) = w.running(&a, 100, screen());
        // H: a heavy run under L1's command (wrapper 150 under 101, command 151).
        let h = HeavyAcquire {
            id: uuid::Uuid::new_v4().to_string(),
            kind: "gradle".into(),
            command: "./gradlew build".into(),
            cwd: "/work".into(),
            workspace_root: None,
            holder: w.proc(150, Some(101)),
            reentrant_token: None,
            no_wait: false,
        };
        w.engine.store.insert_waiting(&h, w.now()).unwrap();
        w.engine.store.mark_running(&h.id, w.now()).unwrap();
        let outer = w
            .engine
            .heavy_admission(&h.holder, &no_heavy())
            .unwrap()
            .unwrap();
        w.engine.record_heavy_nesting(&h.id, &outer);
        let heavy = HeavyView {
            capacity: 1,
            running: vec![HeavyRunning {
                id: h.id.clone(),
                holder: h.holder.clone(),
                child: Some(w.proc(151, Some(150))),
                group: None,
                holder_ancestry: vec![150],
            }],
            waiting: Vec::new(),
        };
        // L2: a lease of H's command (wrapper 160 under 151, command 161).
        let l2 = w.lease(&a, 160, dev(0));
        w.proc(160, Some(151));
        assert!(matches!(
            w.engine.acquire(l2.clone(), &heavy).unwrap(),
            ClaimOutcome::Granted { .. }
        ));
        w.start(&l2.id, 161, 160);
        let recorded: Vec<String> =
            serde_json::from_str(&w.engine.store.nestings().unwrap()[&l2.id]).unwrap();
        assert_eq!(recorded, [format!("heavy:{}", h.id), l1.id.clone()]);
        // L2's wrapper dies (its command lives on, reparented); then H ends.
        w.table.remove(160);
        w.table.insert(161, Some(1), Some(1610));
        w.engine
            .store
            .finish(&h.id, Some(0), HeavyOutcome::Ok, w.now())
            .unwrap();
        w.table.remove(150);
        w.table.remove(151);
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        assert!(
            plan.stops.iter().any(|t| matches!(t,
                StopTarget::RunGroup { claim_id, .. } if *claim_id == l2.id)),
            "{:?}",
            plan.stops
        );
        // The commit refuses while L2's command lives.
        w.kill_run(100, &g1);
        let why = failed(w.commit(&plan, &taker, Vec::new()).unwrap_err());
        assert!(why.contains("(PID 161) is still running"), "{why}");
    }

    /// L28: the commit re-runs the discovery and refuses a live nested run it did not plan.
    #[test]
    fn commit_refuses_a_nested_run_started_after_begin() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let (outer, group) = w.running(&a, 100, screen());
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        // A nested run recorded under the outer one slipped in (its wrapper got past the check).
        let late = uuid::Uuid::new_v4().to_string();
        w.proc(150, None);
        w.engine
            .store
            .insert_claim(&ClaimRow {
                id: late.clone(),
                resource: dev(0),
                kind: ClaimKind::Lease,
                parent: None,
                workspace_id: a.clone(),
                holder: Some(w.proc(150, None)),
                child: None,
                group: None,
                binding: None,
                command: Some("late".into()),
                granted_at_ms: w.now(),
            })
            .unwrap();
        w.engine.store.set_nested_in(&late, &outer.id).unwrap();
        w.kill_run(100, &group);
        let why = failed(w.commit(&plan, &taker, Vec::new()).unwrap_err());
        assert!(
            why.contains(&format!("the run of claim {late} (PID 150) runs under")),
            "{why}"
        );
        // Nothing moved to the taker (the dead outer run is reaped as usual).
        assert!(!w.holder_of(&screen()).contains(&taker.id));
        assert_eq!(w.ended(&outer.id), Some(EndReason::Reaped));
    }

    /// L29: the taker extends its plan before each target; without that it lapses.
    #[test]
    fn plan_survives_long_sequential_stops() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        w.acquire(&w.ttl_hold(&a, screen()));
        let runs: Vec<(ClaimRequest, RunGroup)> = (0..5)
            .map(|i| w.running(&a, 100 + 10 * i, screen()))
            .collect();
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        assert_eq!(plan.stops.len(), 5);
        // Another process cannot extend it.
        let other = w.proc(701, None);
        let e = w.engine.seize_progress(&plan.plan_id, &other).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        for (i, (run, group)) in runs.iter().enumerate() {
            let expires = w
                .engine
                .seize_progress(&plan.plan_id, &taker.requester)
                .unwrap();
            assert_eq!(expires, w.now() + PLAN_TTL_MS);
            // Each stop takes its whole grace and the SIGKILL wait: 15 s.
            w.clock.0.fetch_add(15_000, Ordering::SeqCst);
            w.kill_run(100 + 10 * i as u32, group);
            let _ = run;
        }
        assert_eq!(
            granted_id(w.commit(&plan, &taker, Vec::new()).unwrap()),
            taker.id
        );
        // Without progress the same plan lapses after 60 s.
        let c = w.workspace("c");
        let late = w.lease(&c, 800, screen());
        let plan = w.begin(&late, &screen()).unwrap();
        w.clock.0.fetch_add(PLAN_TTL_MS, Ordering::SeqCst);
        let e = w
            .engine
            .seize_progress(&plan.plan_id, &late.requester)
            .unwrap_err();
        assert!(e.message.contains("no takeover plan"), "{}", e.message);
    }

    /// T6-7: the live plans, for an idle stop's refusal.
    #[test]
    fn live_plans_are_listed_for_an_idle_stop() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        w.acquire(&w.ttl_hold(&a, screen()));
        assert!(w.engine.plan_summaries().is_empty());
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        let plans = w.engine.plan_summaries();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0]["plan_id"], plan.plan_id.as_str());
        assert_eq!(plans[0]["resource"], "screen");
        w.clock.0.fetch_add(PLAN_TTL_MS, Ordering::SeqCst);
        assert!(w.engine.plan_summaries().is_empty());
    }

    /// The snapshot names the taker of a resource being taken over (`status`).
    #[test]
    fn the_snapshot_names_the_taker() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        w.acquire(&w.ttl_hold(&a, screen()));
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        let state = |w: &World| {
            w.engine
                .snapshot()
                .unwrap()
                .resources
                .into_iter()
                .find(|s| s.resource.id == screen())
                .unwrap()
        };
        let b_path = w.engine.store.workspace(&b).unwrap().unwrap().path;
        let seized = state(&w);
        assert!(seized.seizing);
        assert_eq!(seized.seizing_by, Some(b_path));
        w.engine.seize_abort(&plan.plan_id).unwrap();
        let free = state(&w);
        assert!(!free.seizing);
        assert_eq!(free.seizing_by, None);
    }

    /// The service's refusals and the CLI's messages name a target the same way.
    #[test]
    fn targets_are_named_like_the_cli_names_them() {
        let id = ProcessIdentity {
            pid: 7,
            boot_id: BOOT.into(),
            start_time: 1,
        };
        for t in [
            StopTarget::RunGroup {
                claim_id: "c1".into(),
                holder: id.clone(),
                child: None,
                group: None,
            },
            StopTarget::RunGroup {
                claim_id: "heavy:h1".into(),
                holder: id.clone(),
                child: Some(id.clone()),
                group: None,
            },
            StopTarget::Process {
                claim_id: "c2".into(),
                process: id,
            },
            StopTarget::Foreign {
                source: "fake".into(),
                resource: screen(),
                data: serde_json::json!({}),
            },
        ] {
            assert_eq!(describe(&t), crate::cli::takeover::describe(&t));
        }
    }

    #[test]
    fn confirmation_must_match_the_token() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        w.acquire(&w.ttl_hold(&a, screen()));
        w.acquire(&w.ttl_hold(&a, dev(0)));
        let taker = w.lease(&b, 700, screen());
        let begin = |w: &mut World, r: ResourceId, confirmation: &str| {
            let request = ClaimRequest {
                selector: Selector::Exact {
                    resource: r.clone(),
                },
                id: uuid::Uuid::new_v4().to_string(),
                ..taker.clone()
            };
            w.engine.seize_begin(
                SeizeBegin {
                    resource: r,
                    confirmation: confirmation.into(),
                    request,
                },
                &no_heavy(),
            )
        };
        for wrong in ["Screen", "screen ", "", "device:test:0"] {
            let e = begin(&mut w, screen(), wrong).unwrap_err();
            assert_eq!(e.code, ErrorCode::BadRequest);
            assert_eq!(e.message, "--confirmation must be screen");
        }
        assert!(!w.seizing(&screen()));
        begin(&mut w, screen(), "screen").unwrap();
        // A device's token is its display (T1-3), not its id.
        let e = begin(&mut w, dev(0), "device:test:0").unwrap_err();
        assert_eq!(e.message, "--confirmation must be fake-0");
        begin(&mut w, dev(0), "fake-0").unwrap();
    }

    #[test]
    fn seizing_freezes_grants_until_commit_or_abort() {
        let mut w = world();
        let (a, b, c) = (w.workspace("a"), w.workspace("b"), w.workspace("c"));
        let (held, group) = w.running(&a, 100, screen());
        let waiter = w.lease(&c, 300, screen());
        assert!(matches!(w.acquire(&waiter), ClaimOutcome::Queued { .. }));
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        assert!(w.seizing(&screen()));
        // The holder ends on its own: the waiter still gets nothing while the plan lives.
        w.kill_run(100, &group);
        w.engine
            .release(ClaimRelease {
                claim_id: held.id.clone(),
                workspace_id: a.clone(),
                requester: held.requester.clone(),
                exit_code: None,
            })
            .unwrap();
        w.engine.tick(&no_heavy());
        assert!(w.holder_of(&screen()).is_empty());
        assert!(matches!(
            w.engine.known(&waiter.id).unwrap(),
            Some(ClaimOutcome::Queued { .. })
        ));
        // Abort: the freeze lifts and the waiter is granted.
        w.engine.seize_abort(&plan.plan_id).unwrap();
        w.engine.seize_abort(&plan.plan_id).unwrap();
        assert!(!w.seizing(&screen()));
        assert_eq!(w.holder_of(&screen()), vec![waiter.id.clone()]);
    }

    #[test]
    fn plan_lists_every_linked_lease_and_the_bound_process() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let shell = w.proc(500, None);
        w.proc(501, Some(500));
        let mut hold = w.request(
            &a,
            501,
            screen(),
            Purpose::Hold {
                ttl_ms: None,
                bound_to: Some(shell.clone()),
                distinct: false,
            },
        );
        hold.requester = w.proc(501, Some(500));
        w.acquire(&hold);
        let (run1, _) = w.running(&a, 600, screen());
        let (run2, _) = w.running(&a, 610, screen());
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        assert_eq!(plan.claims.len(), 3);
        assert!(plan.stops.contains(&StopTarget::Process {
            claim_id: hold.id.clone(),
            process: shell,
        }));
        for (run, pid) in [(&run1, 600), (&run2, 610)] {
            assert!(plan.stops.iter().any(|t| matches!(t,
                StopTarget::RunGroup { claim_id, holder, child: Some(child), group: Some(_) }
                    if *claim_id == run.id && holder.pid == pid && child.pid == pid + 1)));
        }
        assert_eq!(plan.stops.len(), 3);
        assert_eq!(plan.expires_at_ms, w.now() + PLAN_TTL_MS);
    }

    #[test]
    fn commit_refused_while_any_target_lives() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let (held, group) = w.running(&a, 100, screen());
        let taker = w.lease(&b, 700, screen());
        // The child lives.
        let plan = w.begin(&taker, &screen()).unwrap();
        let why = failed(w.commit(&plan, &taker, Vec::new()).unwrap_err());
        assert_eq!(
            why,
            format!(
                "screen stays with its holder: the run of claim {} (PID 101) is still running",
                held.id
            )
        );
        // The plan is cleared, the claim stays.
        let e = w.commit(&plan, &taker, Vec::new()).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert!(!w.seizing(&screen()));
        assert_eq!(w.holder_of(&screen()), vec![held.id.clone()]);
        // The group is gone but the command itself lives (it left the group).
        w.table.set_group_alive(&group, false);
        let plan = w.begin(&taker, &screen()).unwrap();
        failed(w.commit(&plan, &taker, Vec::new()).unwrap_err());
        w.table.set_group_alive(&group, true);
        // The child is dead but a group member survives.
        w.table.remove(101);
        let plan = w.begin(&taker, &screen()).unwrap();
        failed(w.commit(&plan, &taker, Vec::new()).unwrap_err());
        assert_eq!(w.holder_of(&screen()), vec![held.id.clone()]);
        // All gone: it moves (the wrapper itself may live on to report).
        w.table.set_group_alive(&group, false);
        let plan = w.begin(&taker, &screen()).unwrap();
        assert_eq!(
            granted_id(w.commit(&plan, &taker, Vec::new()).unwrap()),
            taker.id
        );
        assert_eq!(w.ended(&held.id), Some(EndReason::Seized));
    }

    #[test]
    fn unstarted_lease_needs_its_holder_dead() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let held = w.lease(&a, 100, screen());
        w.acquire(&held);
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        assert_eq!(
            plan.stops,
            [StopTarget::RunGroup {
                claim_id: held.id.clone(),
                holder: held.requester.clone(),
                child: None,
                group: None,
            }]
        );
        let why = failed(w.commit(&plan, &taker, Vec::new()).unwrap_err());
        assert!(why.ends_with("(PID 100) is still running"), "{why}");
        w.table.remove(100);
        let plan = w.begin(&taker, &screen()).unwrap();
        assert_eq!(
            granted_id(w.commit(&plan, &taker, Vec::new()).unwrap()),
            taker.id
        );
    }

    /// P2-11: another session's job that is not found is gone once its launcher is dead,
    /// verified by the service itself (not forced); while the launcher lives it is refused.
    #[test]
    fn other_session_not_found_job_is_gone_once_its_launcher_is_dead() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let held = w.lease(&a, 100, screen());
        w.acquire(&held);
        let child = w.proc(101, Some(100));
        let job = RunGroup::WindowsJob {
            name: "Local\\hostler-heavy-nf".into(),
            boot_id: BOOT.into(),
            session: Some(5),
        };
        w.engine
            .started(crate::contract::claims::ClaimStarted {
                claim_id: held.id.clone(),
                child,
                group: job.clone(),
            })
            .unwrap();
        w.table.set_group_not_found(&job, true);
        let taker = w.lease(&b, 700, screen());
        // The taker's evidence carries its own session, never the job's.
        let evidence = || {
            vec![StopEvidence::JobGone {
                target: 0,
                session: 1,
            }]
        };
        let plan = w.begin(&taker, &screen()).unwrap();
        let why = failed(w.commit(&plan, &taker, evidence()).unwrap_err());
        assert!(why.ends_with("(PID 101) is still running"), "{why}");
        assert_eq!(w.holder_of(&screen()), vec![held.id.clone()]);
        w.table.remove(101);
        let plan = w.begin(&taker, &screen()).unwrap();
        w.seizures();
        let committed = w
            .commit_with(&plan, &taker, evidence(), &no_heavy())
            .unwrap();
        assert_eq!(granted_id(committed.outcome), taker.id);
        assert!(!committed.attested);
        assert!(!w.seizures()[0].forced);
    }

    #[test]
    fn unknown_job_state_blocks_unless_matching_evidence() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let held = w.lease(&a, 100, screen());
        w.acquire(&held);
        let child = w.proc(101, Some(100));
        let job = RunGroup::WindowsJob {
            name: "Local\\hostler-heavy-x".into(),
            boot_id: BOOT.into(),
            session: Some(3),
        };
        w.engine
            .started(crate::contract::claims::ClaimStarted {
                claim_id: held.id.clone(),
                child,
                group: job.clone(),
            })
            .unwrap();
        w.table.remove(101);
        w.table.set_group_unknown(&job, true);
        let taker = w.lease(&b, 700, screen());
        for evidence in [
            vec![],
            vec![StopEvidence::Gone { target: 0 }],
            vec![StopEvidence::JobGone {
                target: 0,
                session: 4,
            }],
            vec![StopEvidence::JobGone {
                target: 1,
                session: 3,
            }],
        ] {
            let plan = w.begin(&taker, &screen()).unwrap();
            let why = failed(w.commit(&plan, &taker, evidence).unwrap_err());
            assert!(why.contains("cannot be checked"), "{why}");
            assert_eq!(w.holder_of(&screen()), vec![held.id.clone()]);
        }
        let plan = w.begin(&taker, &screen()).unwrap();
        let evidence = vec![StopEvidence::JobGone {
            target: 0,
            session: 3,
        }];
        w.seizures();
        let committed = w.commit_with(&plan, &taker, evidence, &no_heavy()).unwrap();
        assert_eq!(granted_id(committed.outcome), taker.id);
        // L30: on the taker's word alone — recorded as forced, and the answer says so.
        assert!(committed.attested);
        assert!(w.seizures()[0].forced);
        // A job seen alive is never overridden by evidence.
        let w2_job = RunGroup::WindowsJob {
            name: "Local\\hostler-heavy-y".into(),
            boot_id: BOOT.into(),
            session: Some(3),
        };
        w.table.set_group_alive(&w2_job, true);
        let target = StopTarget::RunGroup {
            claim_id: "c1".into(),
            holder: w.proc(800, None),
            child: Some(ProcessIdentity {
                pid: 801,
                boot_id: BOOT.into(),
                start_time: 1,
            }),
            group: Some(w2_job),
        };
        assert!(
            w.engine
                .gone(
                    &target,
                    Some(&StopEvidence::JobGone {
                        target: 0,
                        session: 3
                    })
                )
                .is_err()
        );
    }

    #[test]
    fn commit_requires_the_taker() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        w.acquire(&w.ttl_hold(&a, screen()));
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        let other = ClaimRequest {
            requester: w.proc(701, None),
            ..taker.clone()
        };
        let e = w.commit(&plan, &other, Vec::new()).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert!(e.message.contains("only the process that began"));
        // The real taker's plan is still there.
        assert!(w.seizing(&screen()));
        assert_eq!(
            granted_id(w.commit(&plan, &taker, Vec::new()).unwrap()),
            taker.id
        );
    }

    #[test]
    fn commit_moves_everything_and_records_one_seizure() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let hold = w.ttl_hold(&a, screen());
        w.acquire(&hold);
        let (run1, g1) = w.running(&a, 600, screen());
        let (run2, g2) = w.running(&a, 610, screen());
        w.seizures();
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        w.kill_run(600, &g1);
        w.kill_run(610, &g2);
        assert_eq!(
            granted_id(w.commit(&plan, &taker, Vec::new()).unwrap()),
            taker.id
        );
        for id in [&hold.id, &run1.id, &run2.id] {
            assert_eq!(w.ended(id), Some(EndReason::Seized));
        }
        assert_eq!(w.holder_of(&screen()), vec![taker.id.clone()]);
        let seizures = w.seizures();
        assert_eq!(seizures.len(), 1);
        let s = &seizures[0];
        assert_eq!(s.resource, "screen");
        assert_eq!(s.from_kind, ClaimKind::Hold);
        assert!(s.from_workspace_path.ends_with("a"));
        assert!(s.by_workspace_path.ends_with("b"));
        assert_eq!(s.by_pid, 700);
        assert!(!s.stranded && !s.forced);
        // The holder's wrapper, ending its run, learns who took it over (Task 8's summary).
        for (id, holder) in [(&run1.id, &run1.requester), (&hold.id, &hold.requester)] {
            let outcome = w
                .engine
                .release(ClaimRelease {
                    claim_id: id.clone(),
                    workspace_id: a.clone(),
                    requester: holder.clone(),
                    exit_code: Some(143),
                })
                .unwrap();
            assert_eq!(
                outcome.seized_by.as_deref(),
                Some(s.by_workspace_path.as_str())
            );
            assert!(outcome.parent.is_none());
        }
    }

    #[test]
    fn stranded_when_every_target_was_already_dead() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let (_, group) = w.running(&a, 100, screen());
        w.kill_run(100, &group);
        w.seizures();
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        granted_id(w.commit(&plan, &taker, Vec::new()).unwrap());
        assert!(w.seizures()[0].stranded);
    }

    #[test]
    fn takers_queued_request_is_consumed() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        w.acquire(&w.ttl_hold(&a, screen()));
        let taker = w.lease(&b, 700, screen());
        assert!(matches!(w.acquire(&taker), ClaimOutcome::Queued { .. }));
        let plan = w.begin(&taker, &screen()).unwrap();
        assert_eq!(
            granted_id(w.commit(&plan, &taker, Vec::new()).unwrap()),
            taker.id
        );
        assert!(w.engine.store.claim_requests().unwrap().is_empty());
        assert_eq!(w.holder_of(&screen()), vec![taker.id.clone()]);
        // Granted from its row: no stale row was withdrawn into the history.
        assert_eq!(w.ended(&taker.id), None);
        w.engine.tick(&no_heavy());
        assert_eq!(w.ended(&taker.id), None);
        // A request that is no longer waiting cannot begin a takeover.
        let e = w.begin(&taker, &screen()).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
    }

    #[test]
    fn holder_released_during_plan_grants_without_seizure() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let hold = w.ttl_hold(&a, screen());
        w.acquire(&hold);
        w.seizures();
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        w.engine
            .release(ClaimRelease {
                claim_id: hold.id.clone(),
                workspace_id: a.clone(),
                requester: hold.requester.clone(),
                exit_code: None,
            })
            .unwrap();
        assert_eq!(
            granted_id(w.commit(&plan, &taker, Vec::new()).unwrap()),
            taker.id
        );
        assert_eq!(w.ended(&hold.id), Some(EndReason::Released));
        assert!(w.seizures().is_empty());
    }

    #[test]
    fn taker_goes_ahead_of_waiters() {
        let mut w = world();
        let (a, b, c) = (w.workspace("a"), w.workspace("b"), w.workspace("c"));
        w.acquire(&w.ttl_hold(&a, screen()));
        let waiter = w.lease(&c, 300, screen());
        assert!(matches!(w.acquire(&waiter), ClaimOutcome::Queued { .. }));
        let taker = w.lease(&b, 700, screen());
        assert!(matches!(w.acquire(&taker), ClaimOutcome::Queued { .. }));
        let plan = w.begin(&taker, &screen()).unwrap();
        assert_eq!(
            granted_id(w.commit(&plan, &taker, Vec::new()).unwrap()),
            taker.id
        );
        assert!(matches!(
            w.engine.known(&waiter.id).unwrap(),
            Some(ClaimOutcome::Queued { .. })
        ));
    }

    #[test]
    fn ttl_hold_without_runs_moves_without_stops() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let hold = w.ttl_hold(&a, screen());
        w.acquire(&hold);
        w.seizures();
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        assert!(plan.stops.is_empty());
        assert_eq!(plan.claims.len(), 1);
        granted_id(w.commit(&plan, &taker, Vec::new()).unwrap());
        assert_eq!(w.ended(&hold.id), Some(EndReason::Seized));
        let s = w.seizures();
        assert_eq!(s.len(), 1);
        assert!(!s[0].stranded);
    }

    #[test]
    fn plan_expires_after_60_s() {
        let mut w = world();
        let (a, b, c) = (w.workspace("a"), w.workspace("b"), w.workspace("c"));
        let hold = w.ttl_hold(&a, screen());
        w.acquire(&hold);
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        w.clock.0.fetch_add(PLAN_TTL_MS - 1, Ordering::SeqCst);
        w.engine.tick(&no_heavy());
        assert!(w.seizing(&screen()));
        w.clock.0.fetch_add(1, Ordering::SeqCst);
        w.engine.tick(&no_heavy());
        assert!(!w.seizing(&screen()));
        let e = w.commit(&plan, &taker, Vec::new()).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert!(e.message.contains("expired"), "{}", e.message);
        assert_eq!(w.holder_of(&screen()), vec![hold.id.clone()]);
        // Another takeover may begin.
        let other = w.lease(&c, 800, screen());
        w.begin(&other, &screen()).unwrap();
    }

    #[test]
    fn own_claim_is_nothing_to_take() {
        let mut w = world();
        let a = w.workspace("a");
        let taker = w.lease(&a, 700, screen());
        let e = w.begin(&taker, &screen()).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert_eq!(e.message, "nothing to take over: screen is free or yours");
        w.acquire(&w.ttl_hold(&a, screen()));
        let e = w.begin(&taker, &screen()).unwrap_err();
        assert_eq!(e.message, "nothing to take over: screen is free or yours");
        assert!(!w.seizing(&screen()));
    }

    fn foreign_claim(r: ResourceId, stop: Option<serde_json::Value>) -> ForeignClaim {
        ForeignClaim {
            resource: r,
            source: "fake".into(),
            description: "a session".into(),
            workspace_path: None,
            since_ms: None,
            stop,
            workspace_id: None,
        }
    }

    #[test]
    fn foreign_stoppable_claim_becomes_a_foreign_target() {
        let mut w = world();
        let b = w.workspace("b");
        let data = serde_json::json!({ "session": 7 });
        w.foreign
            .0
            .lock()
            .unwrap()
            .push(foreign_claim(dev(0), Some(data.clone())));
        let taker = w.lease(&b, 700, dev(0));
        let plan = w.begin(&taker, &dev(0)).unwrap();
        assert!(plan.claims.is_empty());
        assert_eq!(plan.foreign.len(), 1);
        assert_eq!(
            plan.stops,
            [StopTarget::Foreign {
                source: "fake".into(),
                resource: dev(0),
                data,
            }]
        );
    }

    #[test]
    fn commit_refused_while_foreign_claim_is_listed() {
        let mut w = world();
        let b = w.workspace("b");
        w.foreign
            .0
            .lock()
            .unwrap()
            .push(foreign_claim(dev(0), Some(serde_json::json!({}))));
        let taker = w.lease(&b, 700, dev(0));
        let plan = w.begin(&taker, &dev(0)).unwrap();
        let why = failed(w.commit(&plan, &taker, Vec::new()).unwrap_err());
        assert_eq!(why, "fake-0 stays with its holder: fake still holds it");
        w.foreign.0.lock().unwrap().clear();
        let plan = w.begin(&taker, &dev(0)).unwrap_err();
        // Nothing left to take over: the device is free and the taker just acquires it.
        assert_eq!(plan.code, ErrorCode::BadRequest);
        w.foreign
            .0
            .lock()
            .unwrap()
            .push(foreign_claim(dev(0), Some(serde_json::json!({}))));
        let plan = w.begin(&taker, &dev(0)).unwrap();
        w.foreign.0.lock().unwrap().clear();
        assert_eq!(
            granted_id(w.commit(&plan, &taker, Vec::new()).unwrap()),
            taker.id
        );
    }

    #[test]
    fn unstoppable_foreign_claim_is_refused() {
        let mut w = world();
        let b = w.workspace("b");
        w.foreign
            .0
            .lock()
            .unwrap()
            .push(foreign_claim(dev(1), None));
        let taker = w.lease(&b, 700, dev(1));
        let why = failed(w.begin(&taker, &dev(1)).unwrap_err());
        assert_eq!(
            why,
            "fake-1 stays with its holder: fake holds it and hostler cannot stop it"
        );
        assert!(!w.seizing(&dev(1)));
    }

    #[test]
    fn plan_includes_nested_leases_and_heavy_runs_of_targets() {
        let mut w = world();
        let (a, b, c) = (w.workspace("a"), w.workspace("b"), w.workspace("c"));
        // a's run on the screen: wrapper 100, command 101.
        let (outer, _) = w.running(&a, 100, screen());
        // Nested under the command: a run on fake-0 (wrapper 150 under 101)…
        let nested = w.lease(&a, 150, dev(0));
        w.proc(150, Some(101));
        w.acquire(&nested);
        w.start(&nested.id, 151, 150);
        // …and, under that one's command, a run of another checkout on fake-1.
        let deeper = w.lease(&c, 160, dev(1));
        w.proc(160, Some(151));
        w.acquire(&deeper);
        // Unrelated: a run of c's wrapper 200 elsewhere is not nested.
        let unrelated = w.lease(&c, 200, screen());
        assert!(matches!(w.acquire(&unrelated), ClaimOutcome::Queued { .. }));
        let heavy = HeavyView {
            capacity: 2,
            running: vec![
                HeavyRunning {
                    id: "h1".into(),
                    holder: w.proc(170, Some(151)),
                    child: None,
                    group: None,
                    // As the server sends it: the holder alone; the engine fills it in.
                    holder_ancestry: vec![170],
                },
                HeavyRunning {
                    id: "h2".into(),
                    holder: w.proc(180, None),
                    child: None,
                    group: None,
                    holder_ancestry: vec![180],
                },
            ],
            waiting: Vec::new(),
        };
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin_with(&taker, &screen(), &heavy).unwrap();
        let ids: Vec<&str> = plan
            .stops
            .iter()
            .map(|t| match t {
                StopTarget::RunGroup { claim_id, .. } => claim_id.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(ids[0], outer.id);
        assert_eq!(ids.len(), 4, "{ids:?}");
        for id in [nested.id.as_str(), deeper.id.as_str(), "heavy:h1"] {
            assert!(ids.contains(&id), "{id} missing from {ids:?}");
        }
        // Only the screen's claims move; the nested runs are only stopped.
        assert_eq!(plan.claims.len(), 1);
    }

    #[test]
    fn commit_refused_for_an_uncovered_claim_of_the_holder() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let hold = w.ttl_hold(&a, screen());
        w.acquire(&hold);
        let taker = w.lease(&b, 700, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        // A join that slipped in before the freeze took effect.
        let slipped = uuid::Uuid::new_v4().to_string();
        w.engine
            .store
            .insert_claim(&ClaimRow {
                id: slipped.clone(),
                resource: screen(),
                kind: ClaimKind::Hold,
                parent: Some(hold.id.clone()),
                workspace_id: a.clone(),
                holder: None,
                child: None,
                group: None,
                binding: None,
                command: None,
                granted_at_ms: w.now(),
            })
            .unwrap();
        let why = failed(w.commit(&plan, &taker, Vec::new()).unwrap_err());
        assert!(
            why.contains(&format!(
                "took a claim on it ({slipped}) after the takeover began"
            )),
            "{why}"
        );
        assert_eq!(w.ended(&hold.id), None);
    }

    #[test]
    fn begin_refuses_a_second_plan_and_a_stopping_service() {
        let mut w = world();
        let (a, b, c) = (w.workspace("a"), w.workspace("b"), w.workspace("c"));
        w.acquire(&w.ttl_hold(&a, screen()));
        let taker = w.lease(&b, 700, screen());
        w.begin(&taker, &screen()).unwrap();
        let other = w.lease(&c, 800, screen());
        failed(w.begin(&other, &screen()).unwrap_err());
        w.engine.begin_stopping();
        let e = w.begin(&w.lease(&c, 801, dev(0)), &dev(0)).unwrap_err();
        assert_eq!(e.code, ErrorCode::ServiceUnavailable);
    }

    // ---- P3b Task 9b: agent-device sessions in a takeover ----

    fn path_of(w: &World, ws: &str) -> String {
        w.engine.store.workspace(ws).unwrap().unwrap().path
    }

    /// A session of the workspace at `path` (`None`: no workspace owns it).
    fn session(r: ResourceId, path: Option<&str>, name: &str) -> ForeignClaim {
        ForeignClaim {
            workspace_path: path.map(str::to_owned),
            description: format!("agent-device session {name}"),
            ..foreign_claim(r, Some(serde_json::json!({ "session": name })))
        }
    }

    fn sessions(w: &World) -> std::sync::MutexGuard<'_, Vec<ForeignClaim>> {
        w.foreign.0.lock().unwrap()
    }

    fn commit_after(
        w: &mut World,
        plan: &SeizePlan,
        taker: &ClaimRequest,
        read: Result<(), String>,
    ) -> Result<SeizeOutcome, HostlerError> {
        w.engine.seize_commit_after_read(
            SeizeCommit {
                plan_id: plan.plan_id.clone(),
                requester: taker.requester.clone(),
                evidence: Vec::new(),
            },
            &no_heavy(),
            &read,
        )
    }

    /// P2-10: the holder's sessions (and unowned ones) after its claims' targets, never the
    /// taker's own.
    #[test]
    fn seize_begin_plans_the_holders_sessions_after_its_claims() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let (a_path, b_path) = (path_of(&w, &a), path_of(&w, &b));
        let (_run, _group) = w.running(&a, 600, dev(0));
        sessions(&w).extend([
            session(dev(0), Some(&a_path), "holders"),
            session(dev(0), None, "nobodys"),
            session(dev(0), Some(&b_path), "takers"),
        ]);
        let taker = w.lease(&b, 700, dev(0));
        let plan = w.begin(&taker, &dev(0)).unwrap();
        let kinds: Vec<&str> = plan
            .stops
            .iter()
            .map(|t| match t {
                StopTarget::RunGroup { .. } => "run",
                StopTarget::Process { .. } => "process",
                StopTarget::Foreign { data, .. } => data["session"].as_str().unwrap(),
            })
            .collect();
        assert_eq!(kinds, ["run", "holders", "nobodys"]);
        assert_eq!(plan.foreign.len(), 2);
    }

    #[test]
    fn seize_never_plans_or_verifies_the_takers_own_session() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let b_path = path_of(&w, &b);
        let hold = w.ttl_hold(&a, dev(0));
        w.acquire(&hold);
        sessions(&w).push(session(dev(0), Some(&b_path), "takers"));
        let taker = w.lease(&b, 700, dev(0));
        let plan = w.begin(&taker, &dev(0)).unwrap();
        assert!(
            !plan
                .stops
                .iter()
                .any(|t| matches!(t, StopTarget::Foreign { .. }))
        );
        // Still listed at the commit: the taker's own, so the commit passes.
        assert_eq!(
            granted_id(w.commit(&plan, &taker, Vec::new()).unwrap()),
            taker.id
        );
        // A session of another workspace appearing after the plan does not.
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let hold = w.ttl_hold(&a, dev(0));
        w.acquire(&hold);
        let taker = w.lease(&b, 700, dev(0));
        let plan = w.begin(&taker, &dev(0)).unwrap();
        let a_path = path_of(&w, &a);
        sessions(&w).push(session(dev(0), Some(&a_path), "late"));
        let why = failed(w.commit(&plan, &taker, Vec::new()).unwrap_err());
        assert!(why.ends_with("fake still holds it"), "{why}");
    }

    /// P3-1: the commit grants the taker directly; without a good fresh read it is refused on a
    /// device whenever it would end a hostler claim, also one released before the read.
    #[test]
    fn failed_refresh_refuses_a_commit_that_ends_a_hostler_claim() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let hold = w.ttl_hold(&a, dev(0));
        w.acquire(&hold);
        let taker = w.lease(&b, 700, dev(0));
        let plan = w.begin(&taker, &dev(0)).unwrap();
        let why =
            failed(commit_after(&mut w, &plan, &taker, Err("node not found".into())).unwrap_err());
        assert_eq!(
            why,
            "fake-0 stays with its holder: agent-device status unreadable: node not found"
        );
        assert!(
            w.engine.store.claim(&hold.id).unwrap().is_some(),
            "A keeps it"
        );
        // The holder releases before the read: still refused (the plan ended its claim).
        let plan = w.begin(&taker, &dev(0)).unwrap();
        w.engine
            .release(crate::contract::claims::ClaimRelease {
                claim_id: hold.id.clone(),
                workspace_id: a.clone(),
                requester: hold.requester.clone(),
                exit_code: None,
            })
            .unwrap();
        let why =
            failed(commit_after(&mut w, &plan, &taker, Err("node not found".into())).unwrap_err());
        assert!(why.contains("agent-device status unreadable"), "{why}");
        // Not a device: agent-device is not asked.
        let hold = w.ttl_hold(&a, screen());
        w.acquire(&hold);
        let taker = w.lease(&b, 701, screen());
        let plan = w.begin(&taker, &screen()).unwrap();
        let outcome = commit_after(&mut w, &plan, &taker, Err("node not found".into())).unwrap();
        assert_eq!(granted_id(outcome.outcome), taker.id);
    }

    /// L54: a kept session (status unreadable) cannot be taken over; the person's escape is named.
    #[test]
    fn takeover_of_a_kept_session_is_refused_while_unreadable() {
        let mut w = world();
        let b = w.workspace("b");
        sessions(&w).push(ForeignClaim {
            stop: Some(serde_json::json!({ "session": "s1", "kept": true })),
            description: "agent-device session s1 (last known; agent-device status unreadable: \
                          node not found)"
                .into(),
            ..foreign_claim(dev(0), None)
        });
        let taker = w.lease(&b, 700, dev(0));
        let e = w.begin(&taker, &dev(0)).unwrap_err();
        assert_eq!(e.code, ErrorCode::SeizureFailed);
        // This source says nothing is unreadable: kept from before a restart, not read yet
        // (P3bT9b-11).
        assert!(
            e.message
                .contains("has not been read since the service started"),
            "{}",
            e.message
        );
        assert_eq!(
            e.recovery.as_deref(),
            Some(
                "hostler devices tools --detect; or a person releases it: hostler devices \
                 forget-sessions fake-0"
            )
        );
    }

    /// P2-12: one record for the holder's claims and one per foreign claim stopped.
    #[test]
    fn takeover_records_claim_and_foreign_seizures() {
        let mut w = world();
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let a_path = path_of(&w, &a);
        let hold = w.ttl_hold(&a, dev(0));
        w.acquire(&hold);
        sessions(&w).push(session(dev(0), Some(&a_path), "s1"));
        w.seizures();
        let taker = w.lease(&b, 700, dev(0));
        let plan = w.begin(&taker, &dev(0)).unwrap();
        // The stopper closed the session.
        sessions(&w).clear();
        granted_id(w.commit(&plan, &taker, Vec::new()).unwrap());
        let records = w.seizures();
        assert_eq!(records.len(), 2, "{records:?}");
        assert_eq!(records[0].from_kind, ClaimKind::Hold);
        assert_eq!(records[0].from_source, None);
        assert_eq!(records[1].from_kind, SeizedFrom::Foreign);
        assert_eq!(records[1].from_source.as_deref(), Some("fake"));
        assert_eq!(records[1].from_workspace_path, a_path);
        assert_eq!(records[1].resource, "fake-0");
        let stored = w
            .engine
            .store
            .seizures(&crate::contract::heavy::HistoryQuery::default())
            .unwrap();
        assert_eq!(stored.len(), 2);
    }
}
