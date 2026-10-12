//! The watch daemon's kick-off condition evaluation (spec
//! `10-orchestrator.md` §9.36, issue #505): the configurable trigger set
//! that replaces the loop's implicit "ready queue non-empty → spawn".
//!
//! The daemon spawns a campaign session when none is in flight and at
//! least one configured condition holds. The five condition types are the
//! spec's fixed set:
//!
//! - [`KickoffCondition::NewEligibleWork`] — new eligible work appeared on
//!   the board (a ready-queue item the previous pass did not see ready);
//! - [`KickoffCondition::UnblockedPromotion`] — a blocked task became
//!   unblocked (§9.40 promotion; the reconcile path records the same
//!   transition as an `unblocked-task-promoted` event);
//! - [`KickoffCondition::SlotReleased`] — a lease or worker slot was
//!   released (a supervised run reached a terminal status);
//! - [`KickoffCondition::BudgetWindowReset`] — a budget window reset
//!   (§9.19.6 per-day budget scopes: a new UTC day restarts the daily
//!   spend window, so a spend-stopped loop may kick off again);
//! - [`KickoffCondition::ExternalEvent`] — an external GitHub event
//!   arrived (§9.47 queue). The condition type is defined now; the event
//!   source lands with issue #504, so the shipped source is the empty
//!   [`EmptyKickoffEvents`] no-op and the condition never fires.
//!
//! Conditions are operator-configurable through the §9.22 layered
//! configuration (`[watch.kickoff]`; built-in defaults ship as TOML data
//! in the daemon's built-in defaults layer per §9.22.1 — never compiled
//! constants in business logic). Disabling every condition yields an
//! idle-but-healthy daemon: the tick keeps polling and supervising, no
//! campaign session is ever kicked off, and the skip is a typed journal
//! event — never a silent drop.
//!
//! A budget stop is NOT part of this evaluation: the runner's budget
//! gates (spend soft cap, run budget) run BEFORE any kick-off evaluation
//! and produce the explicit `blocked`/`needs-human` state (§9.33.4) — a
//! budget stop is never a silent skip, and kick-off never bypasses a
//! budget to keep the loop moving.
//!
//! This crate implements no security primitive: kick-off gating is
//! orchestration logic (spec §9.27.3); the sandbox boundary remains
//! Arbitraitor's.

use std::collections::HashSet;

/// One §9.36 kick-off condition type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KickoffCondition {
    /// New eligible work appeared on the board.
    NewEligibleWork,
    /// A blocked task became unblocked (§9.40 promotion).
    UnblockedPromotion,
    /// A lease or worker slot was released.
    SlotReleased,
    /// A budget window reset (§9.19.6).
    BudgetWindowReset,
    /// An external GitHub event arrived (§9.47; source lands with #504).
    ExternalEvent,
}

impl KickoffCondition {
    /// Canonical store/journal spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewEligibleWork => "new-eligible-work",
            Self::UnblockedPromotion => "unblocked-promotion",
            Self::SlotReleased => "slot-released",
            Self::BudgetWindowReset => "budget-window-reset",
            Self::ExternalEvent => "external-event",
        }
    }
}

/// Operator-configurable kick-off condition enables (spec §9.36 via the
/// §9.22 layered configuration). `true` arms the condition; a disabled
/// condition never fires.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the condition set IS the spec's fixed five-way enable table (§9.36): each \
              condition is an independent operator-facing switch, and a bitflag or map would \
              hide the per-key layered-config surface the §9.22 TOML block exposes"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct KickoffSettings {
    /// Arm the new-eligible-work condition (default on).
    pub new_eligible_work: bool,
    /// Arm the unblocked-promotion condition (default on).
    pub unblocked_promotion: bool,
    /// Arm the slot-released condition (default on).
    pub slot_released: bool,
    /// Arm the budget-window-reset condition (default on).
    pub budget_window_reset: bool,
    /// Arm the external-event condition (default OFF: the §9.47 event
    /// source lands with issue #504; the shipped source is empty).
    pub external_event: bool,
}

impl KickoffSettings {
    /// The built-in default enables — the value the daemon's built-in
    /// defaults TOML layer ships (§9.22.1): every board/reconcile-derived
    /// condition armed, the external-event condition disarmed until the
    /// §9.47 source exists (#504).
    #[must_use]
    pub const fn builtin_defaults() -> Self {
        Self {
            new_eligible_work: true,
            unblocked_promotion: true,
            slot_released: true,
            budget_window_reset: true,
            external_event: false,
        }
    }
}

impl Default for KickoffSettings {
    fn default() -> Self {
        Self::builtin_defaults()
    }
}

/// One external GitHub event from the §9.47 ingress queue (issue #504).
/// Events carry identifiers only — never board or PR content.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct KickoffExternalEvent {
    /// Stable event identity (the ingress queue's dedup id).
    pub id: String,
}

/// The §9.47 external-event source. The production source (relay PUSH +
/// conditional POLL ingress feeding the dedup queue) lands with issue
/// #504; until then the shipped implementation is [`EmptyKickoffEvents`],
/// the documented empty source, so the condition is wired but never fires.
pub trait KickoffEventSource: Send + Sync {
    /// Peeks the currently pending external events (identifiers only).
    /// A PEEK is non-destructive: the events stay pending until
    /// [`Self::drain_pending`] removes them. Implementations must
    /// return the same events for repeated peeks until a drain.
    fn pending(&self) -> Vec<KickoffExternalEvent>;

    /// Destructively removes every pending event (consumed when a
    /// kick-off's planner actually selects a task). Returns the
    /// drained events.
    fn drain_pending(&self) -> Vec<KickoffExternalEvent> {
        Vec::new()
    }
}

/// The shipped empty §9.47 source (issue #504 pending): the external-event
/// condition is defined and configurable but can never fire.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyKickoffEvents;

impl KickoffEventSource for EmptyKickoffEvents {
    fn pending(&self) -> Vec<KickoffExternalEvent> {
        Vec::new()
    }
}

/// What one kick-off evaluation observes. All board-derived sets carry
/// worker task ids (`task_id_for` slugs) — identifiers, never board
/// content.
#[derive(Debug, Clone, Default)]
pub struct KickoffObservation {
    /// Ready-queue task ids of THIS pass (after the runner's exclusions:
    /// in-flight and budget-excluded tasks are not kick-off candidates).
    pub ready_ids: HashSet<String>,
    /// Ready-queue task ids the previous pass observed (empty before the
    /// first pass of an invocation: a restart re-observes fresh).
    pub previous_ready_ids: HashSet<String>,
    /// Blocked-candidate task ids the previous pass observed (§9.40: a
    /// candidate that reaches the ready queue was promoted).
    pub previous_blocked_ids: HashSet<String>,
    /// A supervised run reached a terminal status since the last pass (a
    /// lease/worker slot was released).
    pub slot_released: bool,
    /// The UTC day boundary moved since the last pass (§9.19.6: the
    /// per-day budget window reset). `false` on a pass-less start.
    pub budget_window_reset: bool,
    /// Pending external events drained from the §9.47 source.
    pub external_events: Vec<KickoffExternalEvent>,
}

/// The outcome of one kick-off evaluation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct KickoffVerdict {
    /// Every condition that held this pass, in spec order. Empty = no
    /// condition fired (the pass is skipped).
    pub fired: Vec<KickoffCondition>,
}

impl KickoffVerdict {
    /// Whether at least one armed condition held (kick off a session).
    #[must_use]
    pub fn kick_off(&self) -> bool {
        !self.fired.is_empty()
    }
}

/// Evaluates one §9.36 kick-off pass: kick off when none is in flight and
/// at least one ARMED condition holds. A disarmed condition never fires,
/// even when its underlying observation would satisfy it.
#[must_use]
pub fn evaluate(settings: &KickoffSettings, observation: &KickoffObservation) -> KickoffVerdict {
    let mut fired = Vec::new();
    if settings.new_eligible_work
        && observation
            .ready_ids
            .iter()
            .any(|id| !observation.previous_ready_ids.contains(id))
    {
        fired.push(KickoffCondition::NewEligibleWork);
    }
    if settings.unblocked_promotion
        && observation
            .ready_ids
            .iter()
            .any(|id| observation.previous_blocked_ids.contains(id))
    {
        fired.push(KickoffCondition::UnblockedPromotion);
    }
    if settings.slot_released && observation.slot_released {
        fired.push(KickoffCondition::SlotReleased);
    }
    if settings.budget_window_reset && observation.budget_window_reset {
        fired.push(KickoffCondition::BudgetWindowReset);
    }
    if settings.external_event && !observation.external_events.is_empty() {
        fired.push(KickoffCondition::ExternalEvent);
    }
    KickoffVerdict { fired }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    // Test-only allowances mirror the crate's integration tests: a failed
    // assertion must fail the test loudly.

    use super::*;

    /// The all-armed default settings.
    fn armed() -> KickoffSettings {
        KickoffSettings::builtin_defaults()
    }

    /// An observation with the given ready ids and nothing else.
    fn obs(ready: &[&str]) -> KickoffObservation {
        KickoffObservation {
            ready_ids: ready.iter().map(|id| (*id).to_string()).collect(),
            ..KickoffObservation::default()
        }
    }

    /// §9.36 condition 1, trigger: a ready item the previous pass did not
    /// see ready is new eligible work.
    #[test]
    fn new_eligible_work_fires_on_a_fresh_ready_item() {
        let mut observation = obs(&["t-1"]);
        observation.previous_ready_ids = HashSet::new();
        let verdict = evaluate(&armed(), &observation);
        assert_eq!(verdict.fired, vec![KickoffCondition::NewEligibleWork]);
    }

    /// §9.36 condition 1, non-trigger (§21.4): work the previous pass
    /// already saw ready is NOT new — re-arming on a stale queue would
    /// re-kick-off every tick.
    #[test]
    fn new_eligible_work_does_not_fire_on_a_stale_ready_queue() {
        let mut observation = obs(&["t-1"]);
        observation.previous_ready_ids = observation.ready_ids.clone();
        let verdict = evaluate(&armed(), &observation);
        assert!(
            verdict.fired.is_empty(),
            "stale ready work must not fire, got {:?}",
            verdict.fired
        );
    }

    /// §9.36 condition 1, non-trigger: the condition is operator-
    /// configurable — disarmed, fresh work never fires it.
    #[test]
    fn new_eligible_work_does_not_fire_when_disarmed() {
        let settings = KickoffSettings {
            new_eligible_work: false,
            ..armed()
        };
        let verdict = evaluate(&settings, &obs(&["t-1"]));
        assert!(
            !verdict.fired.contains(&KickoffCondition::NewEligibleWork),
            "disarmed condition fired: {:?}",
            verdict.fired
        );
    }

    /// §9.36 condition 2, trigger: a ready item the previous pass saw
    /// only as a blocked candidate is a §9.40 promotion.
    #[test]
    fn unblocked_promotion_fires_when_a_blocked_candidate_turns_ready() {
        let mut observation = obs(&["t-2"]);
        observation.previous_blocked_ids = observation.ready_ids.clone();
        let verdict = evaluate(&armed(), &observation);
        assert!(
            verdict
                .fired
                .contains(&KickoffCondition::UnblockedPromotion),
            "the promotion must fire: {:?}",
            verdict.fired
        );
    }

    /// §9.36 condition 2, non-trigger (§21.4): a ready item that was
    /// never blocked is not a promotion (it is condition 1's subject).
    #[test]
    fn unblocked_promotion_does_not_fire_for_never_blocked_work() {
        let mut observation = obs(&["t-1"]);
        observation.previous_blocked_ids = ["t-other".to_string()].into_iter().collect();
        let verdict = evaluate(&armed(), &observation);
        assert!(
            !verdict
                .fired
                .contains(&KickoffCondition::UnblockedPromotion),
            "never-blocked work must not fire the promotion condition: {:?}",
            verdict.fired
        );
    }

    /// §9.36 condition 2, non-trigger: disarmed, a real promotion never
    /// fires it.
    #[test]
    fn unblocked_promotion_does_not_fire_when_disarmed() {
        let settings = KickoffSettings {
            unblocked_promotion: false,
            ..armed()
        };
        let mut observation = obs(&["t-2"]);
        observation.previous_blocked_ids = observation.ready_ids.clone();
        let verdict = evaluate(&settings, &observation);
        assert!(
            !verdict
                .fired
                .contains(&KickoffCondition::UnblockedPromotion),
            "disarmed promotion fired: {:?}",
            verdict.fired
        );
    }

    /// §9.36 condition 3, trigger: a released worker slot re-arms
    /// kick-off.
    #[test]
    fn slot_released_fires_when_a_run_turned_terminal() {
        let observation = KickoffObservation {
            slot_released: true,
            ..KickoffObservation::default()
        };
        let verdict = evaluate(&armed(), &observation);
        assert_eq!(verdict.fired, vec![KickoffCondition::SlotReleased]);
    }

    /// §9.36 condition 3, non-trigger (§21.4): with every slot still
    /// occupied (no terminal reap since the last pass) the condition does
    /// not fire.
    #[test]
    fn slot_released_does_not_fire_while_no_slot_was_freed() {
        let observation = KickoffObservation::default();
        let verdict = evaluate(&armed(), &observation);
        assert!(
            !verdict.fired.contains(&KickoffCondition::SlotReleased),
            "no release observed but the condition fired: {:?}",
            verdict.fired
        );
    }

    /// §9.36 condition 3, non-trigger: disarmed, a real release never
    /// fires it.
    #[test]
    fn slot_released_does_not_fire_when_disarmed() {
        let settings = KickoffSettings {
            slot_released: false,
            ..armed()
        };
        let observation = KickoffObservation {
            slot_released: true,
            ..KickoffObservation::default()
        };
        let verdict = evaluate(&settings, &observation);
        assert!(verdict.fired.is_empty());
    }

    /// §9.36 condition 4, trigger: a UTC day boundary moved since the
    /// last pass — the §9.19.6 per-day budget window reset.
    #[test]
    fn budget_window_reset_fires_on_a_day_boundary() {
        let observation = KickoffObservation {
            budget_window_reset: true,
            ..KickoffObservation::default()
        };
        let verdict = evaluate(&armed(), &observation);
        assert_eq!(verdict.fired, vec![KickoffCondition::BudgetWindowReset]);
    }

    /// §9.36 condition 4, non-trigger (§21.4): within the same budget
    /// window (no day boundary) the condition does not fire.
    #[test]
    fn budget_window_reset_does_not_fire_within_the_same_window() {
        let observation = KickoffObservation::default();
        let verdict = evaluate(&armed(), &observation);
        assert!(
            !verdict.fired.contains(&KickoffCondition::BudgetWindowReset),
            "same window but the condition fired: {:?}",
            verdict.fired
        );
    }

    /// §9.36 condition 4, non-trigger: disarmed, a real window reset
    /// never fires it.
    #[test]
    fn budget_window_reset_does_not_fire_when_disarmed() {
        let settings = KickoffSettings {
            budget_window_reset: false,
            ..armed()
        };
        let observation = KickoffObservation {
            budget_window_reset: true,
            ..KickoffObservation::default()
        };
        let verdict = evaluate(&settings, &observation);
        assert!(verdict.fired.is_empty());
    }

    /// §9.36 condition 5, trigger: an armed external-event condition
    /// fires when the §9.47 source reports pending events.
    #[test]
    fn external_event_fires_when_the_source_reports_events() {
        let settings = KickoffSettings {
            external_event: true,
            ..armed()
        };
        let observation = KickoffObservation {
            external_events: vec![KickoffExternalEvent {
                id: "e-1".to_string(),
            }],
            ..KickoffObservation::default()
        };
        let verdict = evaluate(&settings, &observation);
        assert_eq!(verdict.fired, vec![KickoffCondition::ExternalEvent]);
    }

    /// §9.36 condition 5, non-trigger (§21.4): the shipped EMPTY no-op
    /// source ([`EmptyKickoffEvents`], the no-ingress fallback the
    /// daemon hands the runner when `[watch.ingress].enabled` is false)
    /// never fires the condition even when armed.
    #[test]
    fn external_event_never_fires_on_the_shipped_empty_source() {
        let settings = KickoffSettings {
            external_event: true,
            ..armed()
        };
        let observation = KickoffObservation {
            external_events: EmptyKickoffEvents.pending(),
            ..KickoffObservation::default()
        };
        let verdict = evaluate(&settings, &observation);
        assert!(
            verdict.fired.is_empty(),
            "the empty §9.47 source fired the condition: {:?}",
            verdict.fired
        );
    }

    /// §9.36 condition 5, non-trigger: disarmed (the built-in default),
    /// pending events never fire it.
    #[test]
    fn external_event_does_not_fire_when_disarmed() {
        let observation = KickoffObservation {
            external_events: vec![KickoffExternalEvent {
                id: "e-1".to_string(),
            }],
            ..KickoffObservation::default()
        };
        let verdict = evaluate(&armed(), &observation);
        assert!(verdict.fired.is_empty());
    }

    /// §9.36 idle-but-healthy (§21.4 negative case): with every condition
    /// disabled, ready work present and a slot released still fire
    /// nothing — the daemon idles instead of kicking off.
    #[test]
    fn disabling_every_condition_yields_no_kick_off() {
        let settings = KickoffSettings {
            new_eligible_work: false,
            unblocked_promotion: false,
            slot_released: false,
            budget_window_reset: false,
            external_event: false,
        };
        let mut observation = obs(&["t-1", "t-2"]);
        observation.slot_released = true;
        observation.budget_window_reset = true;
        let verdict = evaluate(&settings, &observation);
        assert!(
            !verdict.kick_off(),
            "all-disabled settings must never kick off: {:?}",
            verdict.fired
        );
    }

    /// Any one armed condition is sufficient (the spec's "at least one
    /// condition holds"); the verdict records every fired condition.
    #[test]
    fn multiple_conditions_are_all_recorded() {
        let observation = KickoffObservation {
            ready_ids: ["t-1".to_string()].into_iter().collect(),
            slot_released: true,
            ..KickoffObservation::default()
        };
        let verdict = evaluate(&armed(), &observation);
        assert_eq!(
            verdict.fired,
            vec![
                KickoffCondition::NewEligibleWork,
                KickoffCondition::SlotReleased
            ]
        );
    }

    /// The empty source stays empty: the #504 source is the only place
    /// that can change this.
    #[test]
    fn the_shipped_event_source_is_empty() {
        assert!(EmptyKickoffEvents.pending().is_empty());
    }
}
