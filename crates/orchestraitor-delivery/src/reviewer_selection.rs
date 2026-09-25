//! Reviewer selection for the change-set review loop (spec §9.33.4).
//!
//! §9.33.4 requires reviewer selection to be driven by changed files and
//! symbols, languages and frameworks, task domains, dependency and
//! configuration changes, risk classification, Arbitraitor findings, and
//! project policy. This module turns a pre-classified [`ChangeSetProfile`]
//! plus the loop's [`ReviewLoopConfig`] into a deterministic
//! [`ReviewerSet`]: one [`ReviewerSlot`] per selected reviewer domain.
//!
//! Interpretation notes (documented so the mapping stays honest):
//!
//! - The profile's boolean flags are produced upstream. Path-, symbol-, and
//!   language-level classification of changed files belongs to the runner
//!   and context layers; this module consumes their verdicts and never
//!   re-derives them from file paths itself.
//! - The §9.33.4 example's backend-for-service/API and frontend-for-UI rows
//!   are illustrations of the task-domain match: such tasks declare the
//!   `backend`/`frontend` domain, so [`SelectionReason::TaskDomainMatch`]
//!   covers them. No per-domain if/else taxonomy is built here (§9.22.4
//!   forbids hardcoded task taxonomies and §9.33.1 makes roles configurable;
//!   the authoritative catalog lives in §9.19.1). The
//!   [`ChangeSetProfile::involves_ui`] and [`ChangeSetProfile::languages`]
//!   inputs are carried as catalog/routing data; they do not mint reviewer
//!   domains of their own.
//!
//! [`select_reviewers`] appends candidates in a fixed priority sequence —
//! general baseline, security triggers, Arbitraitor findings, task-domain
//! match, coverage/testing, then config-required domains — deduplicating by
//! domain so the first (highest-priority) reason wins. Required domains from
//! [`ReviewLoopConfig::required_reviewer_domains`] are always present, always
//! survive the `max_reviewers` truncation, and count toward the cap; when the
//! required list alone exceeds the cap the configuration is unsatisfiable
//! ([`ReviewerSelectionError::ConfigUnsatisfiable`]).
//!
//! Roles are configurable (§9.33.1); slots currently carry the `reviewing`
//! role ([`ROLE_REVIEWING`]) and the §9.19.1 agent catalog maps
//! `(domain, role)` pairs to concrete agents.
//!
//! Security boundary (spec §9.33.7): this module only proposes reviewers. It
//! MUST NOT make security decisions (allow/deny/verdict), gate the change, or
//! render a verdict — blocking decisions belong to the runner and policy
//! layer, and all policy, approval, promotion, and receipt authority belongs
//! to Arbitraitor (§2.2).

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::metadata::{DomainId, RiskClass};
use crate::review_loop::{ReviewLoopConfig, ReviewLoopConfigError};

/// Domain of the general baseline reviewer — always selected first; yields to
/// required domains under a tight `max_reviewers` cap (§9.33.4 example:
/// "general reviewer").
pub const GENERAL_DOMAIN: &str = "general";

/// Domain of the security reviewer (§9.33.4 example: "security reviewer for
/// auth, permissions, dependencies, CI, scripts, or execution").
pub const SECURITY_DOMAIN: &str = "security";

/// Domain of the testing reviewer (§9.33.4 example: "testing reviewer when
/// coverage or verification changed").
pub const TESTING_DOMAIN: &str = "testing";

/// Default role carried by every selected slot (§9.19.1 role names; §9.33.1
/// keeps roles configurable, so this is a default, not a taxonomy).
pub const ROLE_REVIEWING: &str = "reviewing";

/// Language observed in the change set (§9.33.4 "languages and frameworks"
/// driver).
///
/// The ordering derives exist only so profiles can store languages in a
/// `BTreeSet` with deterministic (de)serialization; they carry no semantic
/// priority. Unknown tags deserialize as [`Language::Other`] so a newer
/// producer never breaks an older consumer at the daemon boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// Rust sources.
    Rust,
    /// TypeScript sources.
    TypeScript,
    /// JavaScript sources.
    JavaScript,
    /// Python sources.
    Python,
    /// Go sources.
    Go,
    /// Shell scripts.
    Bash,
    /// TOML manifests and configuration.
    Toml,
    /// Markdown documentation.
    Markdown,
    /// Anything not classified above, or a tag from a newer schema.
    #[serde(other)]
    Other,
}

/// Pre-classified description of the change set under review — the input to
/// reviewer selection (§9.33.4 driver list).
///
/// Every flag is a distilled verdict produced upstream (runner/context
/// classification over changed files and symbols); this struct performs no
/// path heuristics of its own.
// allow: the flags are independent spec-named drivers (§9.33.4), not a state
// machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChangeSetProfile {
    /// Files touched by the change set (provenance for the `changed_files`
    /// driver; classification itself happens upstream).
    pub changed_files: Vec<PathBuf>,
    /// Languages observed in the change set (catalog/routing input).
    pub languages: BTreeSet<Language>,
    /// Declared §9.19.1 task domain, when the task carries one.
    pub task_domain: Option<DomainId>,
    /// Delivery-board risk classification, when classified (§9.33.2).
    pub risk: Option<RiskClass>,
    /// Dependency or configuration changes (Cargo.toml, package.json-style)
    /// — the example's "dependencies" security trigger.
    pub involves_dependencies_or_config: bool,
    /// Auth, permissions, CI, scripts, or execution surface — the example's
    /// primary security trigger.
    pub involves_auth_permissions_scripts_execution: bool,
    /// UI surface (catalog/routing input; the example's frontend row flows
    /// through the task domain instead).
    pub involves_ui: bool,
    /// Coverage or verification changed (test files present) — the example's
    /// testing-reviewer trigger.
    pub involves_coverage_or_verification: bool,
    /// Arbitraitor findings are attached to the change set (§9.33.4 driver).
    pub arbitraitor_findings: bool,
}

impl ChangeSetProfile {
    /// An empty profile: no files, languages, domain, risk, or triggers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the changed-file list.
    #[must_use]
    pub fn with_changed_files(mut self, files: impl IntoIterator<Item = PathBuf>) -> Self {
        self.changed_files = files.into_iter().collect();
        self
    }

    /// Sets the observed languages.
    #[must_use]
    pub fn with_languages(mut self, languages: impl IntoIterator<Item = Language>) -> Self {
        self.languages = languages.into_iter().collect();
        self
    }

    /// Sets the declared task domain.
    #[must_use]
    pub fn with_task_domain(mut self, domain: DomainId) -> Self {
        self.task_domain = Some(domain);
        self
    }

    /// Sets the risk classification.
    #[must_use]
    pub fn with_risk(mut self, risk: RiskClass) -> Self {
        self.risk = Some(risk);
        self
    }

    /// Sets the dependencies/configuration trigger flag.
    #[must_use]
    pub fn with_dependencies_or_config(mut self, value: bool) -> Self {
        self.involves_dependencies_or_config = value;
        self
    }

    /// Sets the auth/permissions/scripts/execution trigger flag.
    #[must_use]
    pub fn with_auth_permissions_scripts_execution(mut self, value: bool) -> Self {
        self.involves_auth_permissions_scripts_execution = value;
        self
    }

    /// Sets the UI-surface flag.
    #[must_use]
    pub fn with_ui(mut self, value: bool) -> Self {
        self.involves_ui = value;
        self
    }

    /// Sets the coverage/verification trigger flag.
    #[must_use]
    pub fn with_coverage_or_verification(mut self, value: bool) -> Self {
        self.involves_coverage_or_verification = value;
        self
    }

    /// Sets the Arbitraitor-findings flag.
    #[must_use]
    pub fn with_arbitraitor_findings(mut self, value: bool) -> Self {
        self.arbitraitor_findings = value;
        self
    }
}

/// Why a reviewer domain was selected (§9.33.4 drivers and example rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionReason {
    /// The general baseline reviewer from the §9.33.4 example — always
    /// selected first; yields to required domains under a tight cap.
    GeneralBaseline,
    /// The change touches auth, permissions, dependencies, CI, scripts, or
    /// execution, carries critical risk, or declares the security task domain
    /// (§9.33.4 example: security reviewer).
    SecurityTriggered,
    /// The reviewer covers the task's declared §9.19.1 domain (the example's
    /// backend-for-service/API and frontend-for-UI rows).
    TaskDomainMatch,
    /// Reserved: a language/framework-keyed reviewer from the §9.19.1
    /// catalog. [`select_reviewers`] does not emit this today because no
    /// per-language reviewer taxonomy is defined in the spec.
    LanguageMatch,
    /// Coverage or verification changed (§9.33.4 example: testing reviewer).
    CoverageChanged,
    /// Arbitraitor findings attached to the change set require security
    /// review (§9.33.4 driver list).
    ArbitraitorFindings,
    /// Mandated by [`ReviewLoopConfig::required_reviewer_domains`] — presence
    /// and truncation override: a required domain is guaranteed a slot; on
    /// dedup the first (trigger-driven) reason is retained.
    RequiredByConfig,
    /// Reserved: additional project-policy reviewers beyond the required
    /// list (§9.33.4 "project policy" driver). Not emitted today.
    ProjectPolicy,
}

fn default_role() -> String {
    ROLE_REVIEWING.to_string()
}

/// One selected reviewer: a `(domain, role)` pair the §9.19.1 catalog maps to
/// a concrete agent, plus the reason it was selected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerSlot {
    /// Reviewer domain (§9.19.1).
    pub domain: DomainId,
    /// Catalog role; defaults to [`ROLE_REVIEWING`]. §9.33.1 keeps roles
    /// configurable, so future catalogs may select other `(domain, role)`
    /// pairs through [`ReviewerSlot::with_role`].
    #[serde(default = "default_role")]
    role: String,
    /// Why this domain was selected.
    pub reason: SelectionReason,
}

impl ReviewerSlot {
    /// A slot in the given domain selected for the given reason, carrying the
    /// default [`ROLE_REVIEWING`] role.
    #[must_use]
    pub fn new(domain: DomainId, reason: SelectionReason) -> Self {
        Self {
            domain,
            role: default_role(),
            reason,
        }
    }

    /// Overrides the catalog role (§9.33.1 roles are configurable).
    #[must_use]
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = role.into();
        self
    }

    /// Returns the catalog role of this slot.
    #[must_use]
    pub fn role(&self) -> &str {
        &self.role
    }
}

/// The deterministic result of reviewer selection: ordered slots, deduplicated
/// by domain, capped at `max_reviewers` with required domains guaranteed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReviewerSet {
    slots: Vec<ReviewerSlot>,
}

impl ReviewerSet {
    fn new(slots: Vec<ReviewerSlot>) -> Self {
        Self { slots }
    }

    /// Ordered reviewer slots (selection-priority order).
    #[must_use]
    pub fn slots(&self) -> &[ReviewerSlot] {
        &self.slots
    }

    /// Iterates the selected reviewer domains in selection order.
    pub fn domains(&self) -> impl Iterator<Item = &DomainId> + '_ {
        self.slots.iter().map(|slot| &slot.domain)
    }

    /// Number of selected reviewers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// True when no reviewers were selected. (The general baseline makes this
    /// unreachable through [`select_reviewers`].)
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// True when the set contains a reviewer for the given domain.
    #[must_use]
    pub fn contains_domain(&self, domain: &DomainId) -> bool {
        self.slots.iter().any(|slot| &slot.domain == domain)
    }

    /// Iterates the slots in selection order.
    pub fn iter(&self) -> std::slice::Iter<'_, ReviewerSlot> {
        self.slots.iter()
    }
}

impl<'a> IntoIterator for &'a ReviewerSet {
    type Item = &'a ReviewerSlot;
    type IntoIter = std::slice::Iter<'a, ReviewerSlot>;

    fn into_iter(self) -> Self::IntoIter {
        self.slots.iter()
    }
}

/// Reviewer-selection failure (spec §9.33.4).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReviewerSelectionError {
    /// The review-loop configuration failed structural validation.
    #[error("invalid review-loop configuration: {0}")]
    InvalidConfig(#[from] ReviewLoopConfigError),
    /// The required reviewer domains alone exceed `max_reviewers`; required
    /// domains always count toward the cap, so such a configuration can never
    /// be satisfied. Duplicate entries count toward this total, matching the
    /// configured list verbatim.
    #[error(
        "required reviewer domains ({required}) exceed max_reviewers ({max_reviewers}); required domains count toward the cap"
    )]
    ConfigUnsatisfiable {
        /// Number of configured required reviewer domains.
        required: usize,
        /// Configured reviewer cap.
        max_reviewers: u32,
    },
}

/// Appends a slot unless the domain is already selected, so the first
/// (highest-priority) reason wins.
fn push_slot(slots: &mut Vec<ReviewerSlot>, domain: DomainId, reason: SelectionReason) {
    if !slots.iter().any(|slot| slot.domain == domain) {
        slots.push(ReviewerSlot::new(domain, reason));
    }
}

/// Selects reviewers for a change set (spec §9.33.4).
///
/// Candidates are appended in this fixed priority sequence, deduplicating by
/// domain (first match wins the recorded reason):
///
/// 1. general baseline — always selected first; survives truncation while
///    budget allows — required domains take precedence under a tight
///    `max_reviewers` cap ([`SelectionReason::GeneralBaseline`]);
/// 2. security reviewer when the change touches auth/permissions/scripts/
///    execution, touches dependencies/configuration, carries
///    [`RiskClass::Critical`], or declares the `security` task domain
///    ([`SelectionReason::SecurityTriggered`]);
/// 3. security reviewer when Arbitraitor findings are attached
///    ([`SelectionReason::ArbitraitorFindings`]);
/// 4. the task-domain reviewer when `task_domain` is set
///    ([`SelectionReason::TaskDomainMatch`]; a missing task domain skips this
///    row and is not an error — the general baseline and required domains
///    still cover the change);
/// 5. the testing reviewer when coverage or verification changed
///    ([`SelectionReason::CoverageChanged`]);
/// 6. any [`ReviewLoopConfig::required_reviewer_domains`] not already
///    selected, sorted by domain for determinism
///    ([`SelectionReason::RequiredByConfig`]).
///
/// The result is truncated to `max_reviewers` with required domains
/// guaranteed: required slots are kept first (they count toward the cap) and
/// the remaining budget is filled in sequence order.
///
/// # Errors
///
/// Returns [`ReviewerSelectionError::InvalidConfig`] when the loop
/// configuration fails [`ReviewLoopConfig::validate`], or
/// [`ReviewerSelectionError::ConfigUnsatisfiable`] when the required reviewer
/// domains alone exceed `max_reviewers`.
pub fn select_reviewers(
    profile: &ChangeSetProfile,
    config: &ReviewLoopConfig,
) -> Result<ReviewerSet, ReviewerSelectionError> {
    config.validate()?;

    let required = config.required_reviewer_domains.len();
    if required > config.max_reviewers as usize {
        return Err(ReviewerSelectionError::ConfigUnsatisfiable {
            required,
            max_reviewers: config.max_reviewers,
        });
    }

    let mut slots = Vec::new();

    // 1. General baseline (§9.33.4 example: always "general reviewer").
    push_slot(
        &mut slots,
        DomainId::new(GENERAL_DOMAIN),
        SelectionReason::GeneralBaseline,
    );

    // 2. Security triggers (§9.33.4 example: auth, permissions, dependencies,
    // CI, scripts, or execution; plus risk classification and task domain).
    let security_triggered = profile.involves_auth_permissions_scripts_execution
        || profile.involves_dependencies_or_config
        || profile.risk == Some(RiskClass::Critical)
        || profile.task_domain.as_ref().map(DomainId::as_str) == Some(SECURITY_DOMAIN);
    if security_triggered {
        push_slot(
            &mut slots,
            DomainId::new(SECURITY_DOMAIN),
            SelectionReason::SecurityTriggered,
        );
    }

    // 3. Arbitraitor findings attached to the change set demand security
    // review (§9.33.4 driver list).
    if profile.arbitraitor_findings {
        push_slot(
            &mut slots,
            DomainId::new(SECURITY_DOMAIN),
            SelectionReason::ArbitraitorFindings,
        );
    }

    // 4. Task-domain match; covers the example's backend-for-service/API and
    // frontend-for-UI rows through the declared domain.
    if let Some(domain) = &profile.task_domain {
        push_slot(&mut slots, domain.clone(), SelectionReason::TaskDomainMatch);
    }

    // 5. Coverage or verification changed → testing reviewer (§9.33.4
    // example).
    if profile.involves_coverage_or_verification {
        push_slot(
            &mut slots,
            DomainId::new(TESTING_DOMAIN),
            SelectionReason::CoverageChanged,
        );
    }

    // 6. Config-required domains not already selected (sorted for
    // determinism); a required domain is guaranteed a slot, but on dedup the
    // first (trigger-driven) reason is retained.
    let required_missing: BTreeSet<&DomainId> = config
        .required_reviewer_domains
        .iter()
        .filter(|domain| !slots.iter().any(|slot| &slot.domain == *domain))
        .collect();
    for domain in required_missing {
        push_slot(
            &mut slots,
            domain.clone(),
            SelectionReason::RequiredByConfig,
        );
    }

    // Required domains always survive truncation; the remaining budget is
    // filled in sequence order. The ConfigUnsatisfiable check above
    // guarantees the required set fits, so this subtraction cannot underflow.
    let max = config.max_reviewers as usize;
    if slots.len() > max {
        let required_set: BTreeSet<&DomainId> = config.required_reviewer_domains.iter().collect();
        // Budget asymmetry: this budget uses the deduplicated required set
        // (BTreeSet), while the ConfigUnsatisfiable check above uses the raw
        // configured list length. A config rejected by the raw-length check
        // never reaches this truncation.
        let optional_budget = max - required_set.len();
        let mut used_optional = 0usize;
        slots.retain(|slot| {
            if required_set.contains(&slot.domain) {
                true
            } else if used_optional < optional_budget {
                used_optional += 1;
                true
            } else {
                false
            }
        });
    }

    Ok(ReviewerSet::new(slots))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Config with no required domains, so trigger reasons are observable
    /// instead of the default `["security"]` requirement masking them.
    fn trigger_config() -> ReviewLoopConfig {
        ReviewLoopConfig {
            required_reviewer_domains: Vec::new(),
            ..ReviewLoopConfig::default()
        }
    }

    fn domains_of(set: &ReviewerSet) -> Vec<&str> {
        set.domains().map(DomainId::as_str).collect()
    }

    #[test]
    fn general_baseline_is_always_first() -> TestResult {
        let config = trigger_config();
        for profile in [
            ChangeSetProfile::default(),
            ChangeSetProfile::default().with_coverage_or_verification(true),
            ChangeSetProfile::default()
                .with_task_domain(DomainId::new("backend"))
                .with_auth_permissions_scripts_execution(true),
        ] {
            let set = select_reviewers(&profile, &config)?;
            assert_eq!(set.slots()[0].domain.as_str(), GENERAL_DOMAIN);
            assert_eq!(set.slots()[0].reason, SelectionReason::GeneralBaseline);
            assert!(set.slots().iter().all(|s| s.role() == ROLE_REVIEWING));
        }
        // A bland change set needs only the general reviewer.
        let set = select_reviewers(&ChangeSetProfile::default(), &config)?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN]);
        Ok(())
    }

    #[test]
    fn security_trigger_from_auth_permissions_scripts_execution() -> TestResult {
        let profile = ChangeSetProfile::default().with_auth_permissions_scripts_execution(true);
        let set = select_reviewers(&profile, &trigger_config())?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN, SECURITY_DOMAIN]);
        assert_eq!(set.slots()[1].reason, SelectionReason::SecurityTriggered);
        Ok(())
    }

    #[test]
    fn security_trigger_from_dependencies_or_config() -> TestResult {
        let profile = ChangeSetProfile::default()
            .with_changed_files([PathBuf::from("Cargo.toml")])
            .with_dependencies_or_config(true);
        let set = select_reviewers(&profile, &trigger_config())?;
        assert!(set.contains_domain(&DomainId::new(SECURITY_DOMAIN)));
        assert_eq!(set.slots()[1].reason, SelectionReason::SecurityTriggered);
        Ok(())
    }

    #[test]
    fn security_trigger_from_critical_risk() -> TestResult {
        let profile = ChangeSetProfile::default().with_risk(RiskClass::Critical);
        let set = select_reviewers(&profile, &trigger_config())?;
        assert_eq!(set.slots()[1].domain.as_str(), SECURITY_DOMAIN);
        assert_eq!(set.slots()[1].reason, SelectionReason::SecurityTriggered);
        // Lower risk classes do not trigger the security reviewer.
        for risk in [RiskClass::Low, RiskClass::Medium, RiskClass::High] {
            let set = select_reviewers(
                &ChangeSetProfile::default().with_risk(risk),
                &trigger_config(),
            )?;
            assert!(!set.contains_domain(&DomainId::new(SECURITY_DOMAIN)));
        }
        Ok(())
    }

    #[test]
    fn security_trigger_from_security_task_domain() -> TestResult {
        let profile = ChangeSetProfile::default().with_task_domain(DomainId::new(SECURITY_DOMAIN));
        let set = select_reviewers(&profile, &trigger_config())?;
        assert_eq!(set.slots()[1].domain.as_str(), SECURITY_DOMAIN);
        assert_eq!(set.slots()[1].reason, SelectionReason::SecurityTriggered);
        Ok(())
    }

    #[test]
    fn security_trigger_from_arbitraitor_findings() -> TestResult {
        let profile = ChangeSetProfile::default().with_arbitraitor_findings(true);
        let set = select_reviewers(&profile, &trigger_config())?;
        assert_eq!(set.slots()[1].domain.as_str(), SECURITY_DOMAIN);
        assert_eq!(set.slots()[1].reason, SelectionReason::ArbitraitorFindings);
        Ok(())
    }

    #[test]
    fn security_required_by_project_policy() -> TestResult {
        // The §9.33.4 "project policy" driver: the default config requires the
        // security domain even for a bland change set.
        let config = ReviewLoopConfig::default();
        let set = select_reviewers(&ChangeSetProfile::default(), &config)?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN, SECURITY_DOMAIN]);
        assert_eq!(set.slots()[1].reason, SelectionReason::RequiredByConfig);
        Ok(())
    }

    #[test]
    fn task_domain_match_adds_domain_reviewer() -> TestResult {
        // §9.33.4 example: "backend reviewer for service/API changes".
        let profile = ChangeSetProfile::default()
            .with_changed_files([PathBuf::from("crates/orchestraitor-daemon/src/server.rs")])
            .with_languages([Language::Rust])
            .with_task_domain(DomainId::new("backend"));
        let set = select_reviewers(&profile, &trigger_config())?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN, "backend"]);
        assert_eq!(set.slots()[1].reason, SelectionReason::TaskDomainMatch);
        Ok(())
    }

    #[test]
    fn frontend_row_flows_through_task_domain() -> TestResult {
        // §9.33.4 example: "frontend reviewer for UI changes" — the UI flag is
        // catalog input; the frontend domain arrives via TaskDomainMatch.
        let profile = ChangeSetProfile::default()
            .with_ui(true)
            .with_languages([Language::TypeScript])
            .with_task_domain(DomainId::new("frontend"));
        let set = select_reviewers(&profile, &trigger_config())?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN, "frontend"]);
        assert_eq!(set.slots()[1].reason, SelectionReason::TaskDomainMatch);
        // The UI flag alone does not mint a frontend reviewer slot.
        let set = select_reviewers(
            &ChangeSetProfile::default().with_ui(true),
            &trigger_config(),
        )?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN]);
        Ok(())
    }

    #[test]
    fn coverage_or_verification_adds_testing_reviewer() -> TestResult {
        let profile = ChangeSetProfile::default()
            .with_changed_files([PathBuf::from("crates/orchestraitor-delivery/src/dag.rs")])
            .with_coverage_or_verification(true);
        let set = select_reviewers(&profile, &trigger_config())?;
        assert_eq!(
            domains_of(&set),
            vec![GENERAL_DOMAIN, TESTING_DOMAIN],
            "§9.33.4 example: testing reviewer when coverage or verification changed"
        );
        assert_eq!(set.slots()[1].reason, SelectionReason::CoverageChanged);
        Ok(())
    }

    #[test]
    fn dedup_keeps_highest_priority_reason() -> TestResult {
        // Security task domain + auth trigger: one slot, SecurityTriggered
        // (tier 2) wins over TaskDomainMatch (tier 4).
        let profile = ChangeSetProfile::default()
            .with_task_domain(DomainId::new(SECURITY_DOMAIN))
            .with_auth_permissions_scripts_execution(true);
        let set = select_reviewers(&profile, &trigger_config())?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN, SECURITY_DOMAIN]);
        assert_eq!(set.slots()[1].reason, SelectionReason::SecurityTriggered);

        // Findings plus another trigger: SecurityTriggered (tier 2) wins over
        // ArbitraitorFindings (tier 3).
        let profile = ChangeSetProfile::default()
            .with_risk(RiskClass::Critical)
            .with_arbitraitor_findings(true);
        let set = select_reviewers(&profile, &trigger_config())?;
        assert_eq!(set.slots()[1].reason, SelectionReason::SecurityTriggered);

        // A required domain already selected by a trigger keeps the trigger
        // reason and is not duplicated by RequiredByConfig.
        let profile = ChangeSetProfile::default().with_dependencies_or_config(true);
        let set = select_reviewers(&profile, &ReviewLoopConfig::default())?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN, SECURITY_DOMAIN]);
        assert_eq!(set.slots()[1].reason, SelectionReason::SecurityTriggered);
        Ok(())
    }

    #[test]
    fn required_domains_are_always_present() -> TestResult {
        let config = ReviewLoopConfig {
            required_reviewer_domains: vec![DomainId::new("qa"), DomainId::new("docs")],
            ..trigger_config()
        };
        let set = select_reviewers(&ChangeSetProfile::default(), &config)?;
        // Sorted order for deterministic config-required appends.
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN, "docs", "qa"]);
        assert!(
            set.slots()
                .iter()
                .skip(1)
                .all(|s| s.reason == SelectionReason::RequiredByConfig)
        );
        Ok(())
    }

    #[test]
    fn required_domains_exceeding_cap_are_unsatisfiable() {
        let config = ReviewLoopConfig {
            max_reviewers: 2,
            required_reviewer_domains: vec![
                DomainId::new("security"),
                DomainId::new("backend"),
                DomainId::new("qa"),
            ],
            ..trigger_config()
        };
        assert_eq!(
            select_reviewers(&ChangeSetProfile::default(), &config),
            Err(ReviewerSelectionError::ConfigUnsatisfiable {
                required: 3,
                max_reviewers: 2,
            })
        );
    }

    #[test]
    fn duplicate_required_domains_count_toward_cap_verbatim() {
        // The ConfigUnsatisfiable check uses the raw configured list length:
        // duplicate entries count toward the cap, matching the configured
        // list verbatim (per the error's documented semantics).
        let config = ReviewLoopConfig {
            max_reviewers: 2,
            required_reviewer_domains: vec![
                DomainId::new("qa"),
                DomainId::new("qa"),
                DomainId::new("qa"),
            ],
            ..trigger_config()
        };
        assert_eq!(
            select_reviewers(&ChangeSetProfile::default(), &config),
            Err(ReviewerSelectionError::ConfigUnsatisfiable {
                required: 3,
                max_reviewers: 2,
            })
        );
    }

    #[test]
    fn duplicate_required_domains_dedup_to_a_single_slot() -> TestResult {
        // Duplicates that pass the raw-length check collapse to one slot:
        // the required set is a `BTreeSet`, so only one `qa` reviewer is
        // appended for `["qa", "qa"]`.
        let config = ReviewLoopConfig {
            max_reviewers: 3,
            required_reviewer_domains: vec![DomainId::new("qa"), DomainId::new("qa")],
            ..trigger_config()
        };
        let set = select_reviewers(&ChangeSetProfile::default(), &config)?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN, "qa"]);
        assert_eq!(set.len(), 2);
        assert_eq!(set.slots()[1].reason, SelectionReason::RequiredByConfig);
        Ok(())
    }

    #[test]
    fn truncation_preserves_required_domains_first() -> TestResult {
        let config = ReviewLoopConfig {
            max_reviewers: 3,
            required_reviewer_domains: vec![DomainId::new(SECURITY_DOMAIN), DomainId::new("qa")],
            ..trigger_config()
        };
        let profile = ChangeSetProfile::default()
            .with_coverage_or_verification(true)
            .with_auth_permissions_scripts_execution(true);
        // Candidates: general, security (SecurityTriggered), testing
        // (CoverageChanged), qa (RequiredByConfig) — 4 slots, cap 3.
        let set = select_reviewers(&profile, &config)?;
        // Required (security, qa) count toward the cap; the remaining budget
        // of 1 goes to the highest-priority optional slot (general), and the
        // lowest-priority optional slot (testing) is truncated away.
        assert_eq!(
            domains_of(&set),
            vec![GENERAL_DOMAIN, SECURITY_DOMAIN, "qa"]
        );
        assert_eq!(set.slots()[1].reason, SelectionReason::SecurityTriggered);
        assert_eq!(set.slots()[2].reason, SelectionReason::RequiredByConfig);
        assert!(set.len() <= 3);
        Ok(())
    }

    #[test]
    fn general_baseline_yields_to_required_domains_under_tight_cap() -> TestResult {
        // The general baseline is always selected first, but required domains
        // take precedence under a tight cap: max=2 with two required domains
        // leaves no optional budget, so the general slot is truncated away.
        let config = ReviewLoopConfig {
            max_reviewers: 2,
            required_reviewer_domains: vec![DomainId::new("qa"), DomainId::new("docs")],
            ..trigger_config()
        };
        let set = select_reviewers(&ChangeSetProfile::default(), &config)?;
        assert_eq!(domains_of(&set), vec!["docs", "qa"]);
        assert!(!set.contains_domain(&DomainId::new(GENERAL_DOMAIN)));
        Ok(())
    }

    #[test]
    fn missing_task_domain_is_not_an_error() -> TestResult {
        // Documented decision: None skips the TaskDomainMatch row; general
        // baseline and required domains still cover the change.
        let set = select_reviewers(&ChangeSetProfile::default(), &trigger_config())?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN]);
        Ok(())
    }

    #[test]
    fn invalid_config_propagates_validation_error() {
        let config = ReviewLoopConfig {
            max_reviewers: 0,
            ..trigger_config()
        };
        assert_eq!(
            select_reviewers(&ChangeSetProfile::default(), &config),
            Err(ReviewerSelectionError::InvalidConfig(
                ReviewLoopConfigError::ZeroMaxReviewers
            ))
        );
    }

    #[test]
    fn empty_change_set_yields_general_plus_required() -> TestResult {
        let set = select_reviewers(&ChangeSetProfile::default(), &ReviewLoopConfig::default())?;
        assert_eq!(domains_of(&set), vec![GENERAL_DOMAIN, SECURITY_DOMAIN]);
        assert_eq!(set.len(), 2);
        assert!(!set.is_empty());
        Ok(())
    }

    #[test]
    fn selection_is_deterministic_for_equal_profiles() -> TestResult {
        let profile = ChangeSetProfile::default()
            .with_changed_files([PathBuf::from("a.rs"), PathBuf::from("b.md")])
            .with_languages([Language::Markdown, Language::Rust])
            .with_task_domain(DomainId::new("backend"))
            .with_coverage_or_verification(true)
            .with_arbitraitor_findings(true);
        let config = ReviewLoopConfig {
            required_reviewer_domains: vec![DomainId::new("qa"), DomainId::new(SECURITY_DOMAIN)],
            ..trigger_config()
        };
        let first = select_reviewers(&profile, &config)?;
        let second = select_reviewers(&profile.clone(), &config)?;
        assert_eq!(first, second);
        // Full sequence: general, security, backend, testing, qa.
        assert_eq!(
            domains_of(&first),
            vec![
                GENERAL_DOMAIN,
                SECURITY_DOMAIN,
                "backend",
                TESTING_DOMAIN,
                "qa"
            ]
        );
        // Round-trips through JSON without changing meaning (daemon boundary).
        let json = serde_json::to_string(&first)?;
        let back: ReviewerSet = serde_json::from_str(&json)?;
        assert_eq!(first, back);
        Ok(())
    }

    #[test]
    fn profile_round_trips_through_json() -> TestResult {
        let profile = ChangeSetProfile::default()
            .with_changed_files([PathBuf::from("src/main.rs")])
            .with_languages([Language::Rust, Language::Toml])
            .with_task_domain(DomainId::new("backend"))
            .with_risk(RiskClass::High)
            .with_dependencies_or_config(true)
            .with_auth_permissions_scripts_execution(true)
            .with_ui(true)
            .with_coverage_or_verification(true)
            .with_arbitraitor_findings(true);
        let json = serde_json::to_string_pretty(&profile)?;
        let back: ChangeSetProfile = serde_json::from_str(&json)?;
        assert_eq!(profile, back);
        // Missing keys fall back to the empty profile; drift fails visibly.
        assert_eq!(
            serde_json::from_str::<ChangeSetProfile>("{}")?,
            ChangeSetProfile::default()
        );
        assert!(serde_json::from_str::<ChangeSetProfile>(r#"{"surprise": true}"#).is_err());
        Ok(())
    }

    #[test]
    fn slot_and_reason_round_trip_through_json() -> TestResult {
        let slot = ReviewerSlot::new(
            DomainId::new(SECURITY_DOMAIN),
            SelectionReason::RequiredByConfig,
        );
        let json = serde_json::to_string(&slot)?;
        assert_eq!(
            json,
            r#"{"domain":"security","role":"reviewing","reason":"required_by_config"}"#
        );
        let back: ReviewerSlot = serde_json::from_str(&json)?;
        assert_eq!(slot, back);
        // The role defaults to "reviewing" when absent from older payloads.
        let legacy: ReviewerSlot =
            serde_json::from_str(r#"{"domain":"security","reason":"general_baseline"}"#)?;
        assert_eq!(legacy.role(), ROLE_REVIEWING);
        // Roles stay configurable per §9.33.1.
        let custom = slot.with_role("lead-reviewing");
        assert_eq!(custom.role(), "lead-reviewing");
        Ok(())
    }

    #[test]
    fn reason_serializes_as_snake_case() -> TestResult {
        for (reason, tag) in [
            (SelectionReason::GeneralBaseline, "general_baseline"),
            (SelectionReason::SecurityTriggered, "security_triggered"),
            (SelectionReason::TaskDomainMatch, "task_domain_match"),
            (SelectionReason::LanguageMatch, "language_match"),
            (SelectionReason::CoverageChanged, "coverage_changed"),
            (SelectionReason::ArbitraitorFindings, "arbitraitor_findings"),
            (SelectionReason::RequiredByConfig, "required_by_config"),
            (SelectionReason::ProjectPolicy, "project_policy"),
        ] {
            let json = serde_json::to_string(&reason)?;
            assert_eq!(json, format!("\"{tag}\""));
            let back: SelectionReason = serde_json::from_str(&json)?;
            assert_eq!(back, reason);
        }
        Ok(())
    }

    #[test]
    fn language_serializes_snake_case_and_unknown_maps_to_other() -> TestResult {
        assert_eq!(
            serde_json::to_string(&Language::TypeScript)?,
            r#""type_script""#
        );
        assert_eq!(serde_json::to_string(&Language::Rust)?, r#""rust""#);
        // Unknown tags from a newer producer degrade to Other instead of
        // failing the daemon boundary.
        let lang: Language = serde_json::from_str(r#""cobol""#)?;
        assert_eq!(lang, Language::Other);
        // BTreeSet storage stays ordered and deduplicated.
        let profile = ChangeSetProfile::default().with_languages([
            Language::Rust,
            Language::Markdown,
            Language::Rust,
        ]);
        assert_eq!(profile.languages.len(), 2);
        Ok(())
    }
}
