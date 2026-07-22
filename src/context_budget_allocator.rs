use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
};

use openagent_protocol::{Role, SemanticAnchorKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::{
    ContextCompactionPolicy, ContextDelivery, ContextItem, ContextItemCategory, ContextItemScope,
    micro_compaction_from_metadata,
};

pub const CONTEXT_BUDGET_ALLOCATION_SCHEMA_VERSION: &str = "openagent.context_budget_allocation.v1";
pub const CONTEXT_BUDGET_POLICY_SCHEMA_VERSION: &str = "openagent.context_budget_policy.v1";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextBudgetClass {
    Instruction,
    LatestUser,
    CriticalAnchor,
    ContinuationAnchor,
    RuntimeState,
    SessionState,
    RecentConversation,
    ToolObservation,
    Attachment,
    HistoricalConversation,
    #[default]
    Extension,
}

impl ContextBudgetClass {
    pub const ALL: [Self; 11] = [
        Self::Instruction,
        Self::LatestUser,
        Self::CriticalAnchor,
        Self::ContinuationAnchor,
        Self::RuntimeState,
        Self::SessionState,
        Self::RecentConversation,
        Self::ToolObservation,
        Self::Attachment,
        Self::HistoricalConversation,
        Self::Extension,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Instruction => "instruction",
            Self::LatestUser => "latest_user",
            Self::CriticalAnchor => "critical_anchor",
            Self::ContinuationAnchor => "continuation_anchor",
            Self::RuntimeState => "runtime_state",
            Self::SessionState => "session_state",
            Self::RecentConversation => "recent_conversation",
            Self::ToolObservation => "tool_observation",
            Self::Attachment => "attachment",
            Self::HistoricalConversation => "historical_conversation",
            Self::Extension => "extension",
        }
    }

    const fn protection_rank(self) -> u8 {
        match self {
            Self::Instruction => 0,
            Self::LatestUser => 1,
            Self::CriticalAnchor => 2,
            Self::ContinuationAnchor => 3,
            Self::RuntimeState => 4,
            Self::SessionState => 5,
            Self::RecentConversation => 6,
            Self::ToolObservation => 7,
            Self::Attachment => 8,
            Self::HistoricalConversation => 9,
            Self::Extension => 10,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextBudgetRecency {
    CurrentTurn,
    Recent,
    Historical,
    Stable,
}

impl ContextBudgetRecency {
    const fn rank(self) -> u8 {
        match self {
            Self::CurrentTurn => 0,
            Self::Recent => 1,
            Self::Historical => 2,
            Self::Stable => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRecoverability {
    InlineOnly,
    SessionLedger,
    DurableReference,
    Rebuildable,
}

impl ContextRecoverability {
    const fn protection_rank(self) -> u8 {
        match self {
            Self::InlineOnly => 0,
            Self::SessionLedger => 1,
            Self::DurableReference => 2,
            Self::Rebuildable => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextBudgetAllocationPhase {
    HardReserve,
    SoftQuota,
    Borrowed,
    Dropped,
    Duplicate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextBudgetItemDecision {
    pub schema_version: String,
    pub class: ContextBudgetClass,
    pub phase: ContextBudgetAllocationPhase,
    pub recency: ContextBudgetRecency,
    pub recoverability: ContextRecoverability,
    pub hard_required: bool,
    pub group_id: String,
    pub group_tokens: u64,
    pub class_quota_tokens: u64,
    pub selection_rank: u64,
}

impl ContextBudgetItemDecision {
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.schema_version == CONTEXT_BUDGET_ALLOCATION_SCHEMA_VERSION
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextBudgetAllocationPolicy {
    pub schema_version: String,
    pub recent_user_turns: u64,
    pub soft_quota_basis_points: BTreeMap<ContextBudgetClass, u64>,
}

impl Default for ContextBudgetAllocationPolicy {
    fn default() -> Self {
        Self {
            schema_version: CONTEXT_BUDGET_POLICY_SCHEMA_VERSION.to_string(),
            recent_user_turns: 2,
            soft_quota_basis_points: BTreeMap::from([
                (ContextBudgetClass::RecentConversation, 3_600),
                (ContextBudgetClass::ToolObservation, 2_200),
                (ContextBudgetClass::SessionState, 1_200),
                (ContextBudgetClass::Attachment, 800),
                (ContextBudgetClass::HistoricalConversation, 1_600),
                (ContextBudgetClass::Extension, 600),
            ]),
        }
    }
}

impl ContextBudgetAllocationPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CONTEXT_BUDGET_POLICY_SCHEMA_VERSION {
            return Err(format!(
                "unsupported context budget policy schema: {}",
                self.schema_version
            ));
        }
        if self.recent_user_turns == 0 {
            return Err("context budget recent_user_turns must be positive".to_string());
        }
        let total = self.soft_quota_basis_points.values().sum::<u64>();
        if total != 10_000 {
            return Err(format!(
                "context budget soft quota basis points must total 10000, got {total}"
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn policy_hash(&self) -> String {
        let value = serde_json::to_value(self).expect("context budget policy serializes");
        format!("sha1:{:x}", Sha1::digest(stable_json(&value).as_bytes()))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextBudgetClassAllocation {
    pub class: ContextBudgetClass,
    pub candidate_item_count: u64,
    pub candidate_tokens: u64,
    pub hard_item_count: u64,
    pub hard_tokens: u64,
    pub quota_tokens: u64,
    pub selected_item_count: u64,
    pub selected_tokens: u64,
    pub borrowed_tokens: u64,
    pub dropped_item_count: u64,
    pub dropped_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextBudgetAllocation {
    pub schema_version: String,
    pub policy_schema_version: String,
    pub policy_hash: String,
    pub item_budget_tokens: u64,
    pub hard_required_tokens: u64,
    pub hard_selected_tokens: u64,
    pub hard_overflow_tokens: u64,
    pub soft_budget_tokens: u64,
    pub soft_quota_selected_tokens: u64,
    pub borrowed_tokens: u64,
    pub selected_tokens: u64,
    pub dropped_tokens: u64,
    pub classes: Vec<ContextBudgetClassAllocation>,
}

impl ContextBudgetAllocation {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CONTEXT_BUDGET_ALLOCATION_SCHEMA_VERSION {
            return Err(format!(
                "unsupported context budget allocation schema: {}",
                self.schema_version
            ));
        }
        if self.policy_schema_version != CONTEXT_BUDGET_POLICY_SCHEMA_VERSION {
            return Err(format!(
                "unsupported context budget policy schema: {}",
                self.policy_schema_version
            ));
        }
        if !is_sha1_hash(&self.policy_hash) {
            return Err("context budget policy hash is not canonical sha1".to_string());
        }
        if self.hard_required_tokens
            != self
                .hard_selected_tokens
                .saturating_add(self.hard_overflow_tokens)
        {
            return Err("context budget hard token accounting mismatch".to_string());
        }
        if self.selected_tokens
            != self
                .hard_selected_tokens
                .saturating_add(self.soft_quota_selected_tokens)
                .saturating_add(self.borrowed_tokens)
        {
            return Err("context budget selected token accounting mismatch".to_string());
        }
        if self.selected_tokens > self.item_budget_tokens {
            return Err("context budget allocation exceeds item budget".to_string());
        }
        if self.classes.len() != ContextBudgetClass::ALL.len()
            || self
                .classes
                .iter()
                .map(|item| item.class)
                .collect::<BTreeSet<_>>()
                .len()
                != ContextBudgetClass::ALL.len()
        {
            return Err("context budget class allocation is incomplete".to_string());
        }
        if self.classes.iter().any(|item| {
            item.candidate_item_count
                != item
                    .selected_item_count
                    .saturating_add(item.dropped_item_count)
                || item.candidate_tokens != item.selected_tokens.saturating_add(item.dropped_tokens)
                || item.hard_item_count > item.candidate_item_count
                || item.hard_tokens > item.candidate_tokens
                || item.borrowed_tokens > item.selected_tokens
        }) {
            return Err("context budget class item accounting mismatch".to_string());
        }
        let selected = self
            .classes
            .iter()
            .map(|item| item.selected_tokens)
            .sum::<u64>();
        let dropped = self
            .classes
            .iter()
            .map(|item| item.dropped_tokens)
            .sum::<u64>();
        let hard = self
            .classes
            .iter()
            .map(|item| item.hard_tokens)
            .sum::<u64>();
        let borrowed = self
            .classes
            .iter()
            .map(|item| item.borrowed_tokens)
            .sum::<u64>();
        if selected != self.selected_tokens || dropped != self.dropped_tokens {
            return Err("context budget class accounting mismatch".to_string());
        }
        if hard != self.hard_required_tokens || borrowed != self.borrowed_tokens {
            return Err("context budget class phase accounting mismatch".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ContextBudgetProjection {
    pub included: BTreeSet<String>,
    pub dropped: BTreeMap<String, String>,
    pub decisions: BTreeMap<String, ContextBudgetItemDecision>,
    pub allocation: Option<ContextBudgetAllocation>,
}

#[derive(Clone, Debug)]
struct AllocationUnit {
    member_indices: Vec<usize>,
    member_ids: Vec<String>,
    class: ContextBudgetClass,
    recency: ContextBudgetRecency,
    recoverability: ContextRecoverability,
    scope: ContextItemScope,
    hard_required: bool,
    priority: i64,
    anchor_rank: u8,
    sequence: u64,
    tokens: u64,
    group_id: String,
}

impl AllocationUnit {
    fn item_count(&self) -> u64 {
        self.member_indices.len() as u64
    }
}

pub(crate) fn allocate_context_budget(
    items: &[ContextItem],
    item_budget_tokens: Option<u64>,
    configured_policy: &ContextBudgetAllocationPolicy,
) -> ContextBudgetProjection {
    let policy = if configured_policy.validate().is_ok() {
        configured_policy.clone()
    } else {
        ContextBudgetAllocationPolicy::default()
    };
    let mut projection = ContextBudgetProjection {
        included: BTreeSet::new(),
        dropped: BTreeMap::new(),
        decisions: BTreeMap::new(),
        allocation: None,
    };
    let recent_start = recent_message_start(items, policy.recent_user_turns);

    for item in items {
        if item
            .metadata
            .get("context_semantic_duplicate_of")
            .and_then(Value::as_str)
            .is_some()
        {
            projection
                .dropped
                .insert(item.id.clone(), "semantic_duplicate".to_string());
            if item_budget_tokens.is_some() {
                projection.decisions.insert(
                    item.id.clone(),
                    item_decision(
                        item,
                        recent_start,
                        DecisionInputs {
                            phase: ContextBudgetAllocationPhase::Duplicate,
                            hard_required: item.pinned,
                            group_id: own_group_id(item),
                            group_tokens: item.token_estimate,
                            class_quota_tokens: 0,
                            selection_rank: u64::MAX,
                        },
                    ),
                );
            }
        } else if item.delivery != ContextDelivery::Message {
            projection.included.insert(item.id.clone());
        }
    }

    let Some(item_budget_tokens) = item_budget_tokens else {
        for item in items {
            if !projection.dropped.contains_key(&item.id) {
                projection.included.insert(item.id.clone());
            }
        }
        return projection;
    };

    let mut units = allocation_units(items, &projection.dropped, recent_start);
    let mut class_stats = ContextBudgetClass::ALL
        .into_iter()
        .map(|class| {
            (
                class,
                ContextBudgetClassAllocation {
                    class,
                    ..Default::default()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for unit in &units {
        let stats = class_stats
            .get_mut(&unit.class)
            .expect("known budget class");
        stats.candidate_item_count = stats.candidate_item_count.saturating_add(unit.item_count());
        stats.candidate_tokens = stats.candidate_tokens.saturating_add(unit.tokens);
        if unit.hard_required {
            stats.hard_item_count = stats.hard_item_count.saturating_add(unit.item_count());
            stats.hard_tokens = stats.hard_tokens.saturating_add(unit.tokens);
        }
    }

    let hard_required_tokens = units
        .iter()
        .filter(|unit| unit.hard_required)
        .map(|unit| unit.tokens)
        .sum::<u64>();
    let mut used = 0u64;
    let mut rank = 0u64;
    let mut selected_units = BTreeSet::new();
    let mut hard_indices = units
        .iter()
        .enumerate()
        .filter(|(_, unit)| unit.hard_required)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    hard_indices.sort_by_key(|index| unit_selection_key(&units[*index]));
    for index in hard_indices {
        let unit = &units[index];
        if used.saturating_add(unit.tokens) <= item_budget_tokens {
            used = used.saturating_add(unit.tokens);
            selected_units.insert(index);
            select_unit(
                unit,
                items,
                &mut projection,
                ContextBudgetAllocationPhase::HardReserve,
                0,
                rank,
            );
        } else {
            drop_unit(unit, items, &mut projection, true, 0, rank);
        }
        rank = rank.saturating_add(1);
    }
    let hard_selected_tokens = used;
    let soft_budget_tokens = item_budget_tokens.saturating_sub(used);

    let mut quota_remaining = BTreeMap::new();
    for class in ContextBudgetClass::ALL {
        let basis_points = policy
            .soft_quota_basis_points
            .get(&class)
            .copied()
            .unwrap_or_default();
        let quota = soft_budget_tokens.saturating_mul(basis_points) / 10_000;
        quota_remaining.insert(class, quota);
        class_stats
            .get_mut(&class)
            .expect("known budget class")
            .quota_tokens = quota;
    }

    let mut soft_quota_selected_tokens = 0u64;
    for class in ContextBudgetClass::ALL {
        let mut indices = units
            .iter()
            .enumerate()
            .filter(|(index, unit)| {
                !unit.hard_required && !selected_units.contains(index) && unit.class == class
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        indices.sort_by_key(|index| unit_selection_key(&units[*index]));
        for index in indices {
            let unit = &units[index];
            let remaining = quota_remaining.get(&class).copied().unwrap_or_default();
            if unit.tokens <= remaining && used.saturating_add(unit.tokens) <= item_budget_tokens {
                used = used.saturating_add(unit.tokens);
                soft_quota_selected_tokens = soft_quota_selected_tokens.saturating_add(unit.tokens);
                quota_remaining.insert(class, remaining.saturating_sub(unit.tokens));
                selected_units.insert(index);
                select_unit(
                    unit,
                    items,
                    &mut projection,
                    ContextBudgetAllocationPhase::SoftQuota,
                    class_stats
                        .get(&class)
                        .expect("known budget class")
                        .quota_tokens,
                    rank,
                );
                rank = rank.saturating_add(1);
            }
        }
    }

    let mut borrow_indices = units
        .iter()
        .enumerate()
        .filter(|(index, unit)| !unit.hard_required && !selected_units.contains(index))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    borrow_indices.sort_by_key(|index| unit_selection_key(&units[*index]));
    let mut borrowed_tokens = 0u64;
    for index in borrow_indices {
        let unit = &units[index];
        let quota = class_stats
            .get(&unit.class)
            .expect("known budget class")
            .quota_tokens;
        if used.saturating_add(unit.tokens) <= item_budget_tokens {
            used = used.saturating_add(unit.tokens);
            borrowed_tokens = borrowed_tokens.saturating_add(unit.tokens);
            selected_units.insert(index);
            select_unit(
                unit,
                items,
                &mut projection,
                ContextBudgetAllocationPhase::Borrowed,
                quota,
                rank,
            );
        } else {
            drop_unit(unit, items, &mut projection, false, quota, rank);
        }
        rank = rank.saturating_add(1);
    }

    for (index, unit) in units.iter_mut().enumerate() {
        let stats = class_stats
            .get_mut(&unit.class)
            .expect("known budget class");
        let selected = selected_units.contains(&index);
        if selected {
            stats.selected_item_count = stats.selected_item_count.saturating_add(unit.item_count());
            stats.selected_tokens = stats.selected_tokens.saturating_add(unit.tokens);
            if projection
                .decisions
                .get(&unit.member_ids[0])
                .is_some_and(|decision| decision.phase == ContextBudgetAllocationPhase::Borrowed)
            {
                stats.borrowed_tokens = stats.borrowed_tokens.saturating_add(unit.tokens);
            }
        } else {
            stats.dropped_item_count = stats.dropped_item_count.saturating_add(unit.item_count());
            stats.dropped_tokens = stats.dropped_tokens.saturating_add(unit.tokens);
        }
    }
    let dropped_tokens = class_stats.values().map(|item| item.dropped_tokens).sum();
    let allocation = ContextBudgetAllocation {
        schema_version: CONTEXT_BUDGET_ALLOCATION_SCHEMA_VERSION.to_string(),
        policy_schema_version: policy.schema_version.clone(),
        policy_hash: policy.policy_hash(),
        item_budget_tokens,
        hard_required_tokens,
        hard_selected_tokens,
        hard_overflow_tokens: hard_required_tokens.saturating_sub(hard_selected_tokens),
        soft_budget_tokens,
        soft_quota_selected_tokens,
        borrowed_tokens,
        selected_tokens: used,
        dropped_tokens,
        classes: ContextBudgetClass::ALL
            .into_iter()
            .filter_map(|class| class_stats.remove(&class))
            .collect(),
    };
    debug_assert!(allocation.validate().is_ok());
    projection.allocation = Some(allocation);
    projection
}

fn allocation_units(
    items: &[ContextItem],
    dropped: &BTreeMap<String, String>,
    recent_start: Option<u64>,
) -> Vec<AllocationUnit> {
    let eligible = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.delivery == ContextDelivery::Message && !dropped.contains_key(&item.id)
        })
        .map(|(index, _)| index)
        .collect::<BTreeSet<_>>();
    let mut tool_results = BTreeMap::<String, Vec<usize>>::new();
    for index in &eligible {
        let item = &items[*index];
        if item.kind == "tool_result"
            && let Some(call_id) = item.metadata.get("tool_call_id").and_then(Value::as_str)
            && !call_id.is_empty()
        {
            tool_results
                .entry(call_id.to_string())
                .or_default()
                .push(*index);
        }
    }

    let mut assigned = BTreeSet::new();
    let mut groups = Vec::new();
    for index in &eligible {
        if assigned.contains(index) {
            continue;
        }
        let item = &items[*index];
        let call_ids = assistant_tool_call_ids(item);
        if call_ids.is_empty() {
            continue;
        }
        let mut members = vec![*index];
        for call_id in call_ids {
            if let Some(result_indices) = tool_results.get(&call_id) {
                members.extend(result_indices.iter().copied());
            }
        }
        members.sort_unstable();
        members.dedup();
        for member in &members {
            assigned.insert(*member);
        }
        groups.push(members);
    }
    for index in eligible {
        if assigned.insert(index) {
            groups.push(vec![index]);
        }
    }
    groups.sort_by_key(|members| members[0]);
    groups
        .into_iter()
        .map(|members| allocation_unit(items, members, recent_start))
        .collect()
}

fn allocation_unit(
    items: &[ContextItem],
    member_indices: Vec<usize>,
    recent_start: Option<u64>,
) -> AllocationUnit {
    let member_ids = member_indices
        .iter()
        .map(|index| items[*index].id.clone())
        .collect::<Vec<_>>();
    let has_tool_result = member_indices
        .iter()
        .any(|index| items[*index].kind == "tool_result");
    let mut class = ContextBudgetClass::Extension;
    let mut recency = ContextBudgetRecency::Stable;
    let mut recoverability = ContextRecoverability::Rebuildable;
    let mut scope = ContextItemScope::Stable;
    let mut hard_required = false;
    let mut priority = i64::MIN;
    let mut anchor_rank = u8::MAX;
    let mut sequence = 0u64;
    let mut tokens = 0u64;
    for index in &member_indices {
        let item = &items[*index];
        let item_recency = item_recency(item, recent_start);
        let item_class = item_class(item, item_recency);
        if item_class.protection_rank() < class.protection_rank() {
            class = item_class;
        }
        if item_recency.rank() < recency.rank() {
            recency = item_recency;
        }
        let item_recoverability = item_recoverability(item);
        if item_recoverability.protection_rank() < recoverability.protection_rank() {
            recoverability = item_recoverability;
        }
        if scope_rank(item.taxonomy.scope) < scope_rank(scope) {
            scope = item.taxonomy.scope;
        }
        hard_required |= item.pinned;
        priority = priority.max(item.priority);
        anchor_rank = anchor_rank.min(item_anchor_rank(item));
        sequence = sequence.max(item_sequence(item).unwrap_or(*index as u64));
        tokens = tokens.saturating_add(item.token_estimate);
    }
    if has_tool_result && !hard_required {
        class = ContextBudgetClass::ToolObservation;
    }
    let group_id = group_id(&member_ids);
    AllocationUnit {
        member_indices,
        member_ids,
        class,
        recency,
        recoverability,
        scope,
        hard_required,
        priority,
        anchor_rank,
        sequence,
        tokens,
        group_id,
    }
}

fn select_unit(
    unit: &AllocationUnit,
    items: &[ContextItem],
    projection: &mut ContextBudgetProjection,
    phase: ContextBudgetAllocationPhase,
    quota: u64,
    rank: u64,
) {
    for index in &unit.member_indices {
        let item = &items[*index];
        projection.included.insert(item.id.clone());
        projection.decisions.insert(
            item.id.clone(),
            item_decision(
                item,
                None,
                DecisionInputs {
                    phase,
                    hard_required: unit.hard_required,
                    group_id: unit.group_id.clone(),
                    group_tokens: unit.tokens,
                    class_quota_tokens: quota,
                    selection_rank: rank,
                },
            )
            .with_unit(unit),
        );
    }
}

fn drop_unit(
    unit: &AllocationUnit,
    items: &[ContextItem],
    projection: &mut ContextBudgetProjection,
    hard_required: bool,
    quota: u64,
    rank: u64,
) {
    for index in &unit.member_indices {
        let item = &items[*index];
        projection.dropped.insert(
            item.id.clone(),
            if hard_required {
                "required_budget_exhausted".to_string()
            } else {
                "model_context_budget".to_string()
            },
        );
        projection.decisions.insert(
            item.id.clone(),
            item_decision(
                item,
                None,
                DecisionInputs {
                    phase: ContextBudgetAllocationPhase::Dropped,
                    hard_required: unit.hard_required,
                    group_id: unit.group_id.clone(),
                    group_tokens: unit.tokens,
                    class_quota_tokens: quota,
                    selection_rank: rank,
                },
            )
            .with_unit(unit),
        );
    }
}

struct DecisionInputs {
    phase: ContextBudgetAllocationPhase,
    hard_required: bool,
    group_id: String,
    group_tokens: u64,
    class_quota_tokens: u64,
    selection_rank: u64,
}

fn item_decision(
    item: &ContextItem,
    recent_start: Option<u64>,
    inputs: DecisionInputs,
) -> ContextBudgetItemDecision {
    let recency = item_recency(item, recent_start);
    ContextBudgetItemDecision {
        schema_version: CONTEXT_BUDGET_ALLOCATION_SCHEMA_VERSION.to_string(),
        class: item_class(item, recency),
        phase: inputs.phase,
        recency,
        recoverability: item_recoverability(item),
        hard_required: inputs.hard_required,
        group_id: inputs.group_id,
        group_tokens: inputs.group_tokens,
        class_quota_tokens: inputs.class_quota_tokens,
        selection_rank: inputs.selection_rank,
    }
}

impl ContextBudgetItemDecision {
    fn with_unit(mut self, unit: &AllocationUnit) -> Self {
        self.class = unit.class;
        self.recency = unit.recency;
        self.recoverability = unit.recoverability;
        self
    }
}

fn unit_selection_key(
    unit: &AllocationUnit,
) -> (u8, u8, u8, u8, u8, Reverse<i64>, Reverse<u64>, &str) {
    (
        unit.class.protection_rank(),
        unit.recency.rank(),
        scope_rank(unit.scope),
        unit.recoverability.protection_rank(),
        unit.anchor_rank,
        Reverse(unit.priority),
        Reverse(unit.sequence),
        unit.group_id.as_str(),
    )
}

fn item_class(item: &ContextItem, recency: ContextBudgetRecency) -> ContextBudgetClass {
    if is_latest_user(item) {
        return ContextBudgetClass::LatestUser;
    }
    match item.taxonomy.category {
        ContextItemCategory::Instruction
        | ContextItemCategory::Skill
        | ContextItemCategory::ToolManifest => ContextBudgetClass::Instruction,
        ContextItemCategory::SemanticAnchor => match semantic_anchor_kind(item) {
            Some(SemanticAnchorKind::Goal | SemanticAnchorKind::Constraint) => {
                ContextBudgetClass::CriticalAnchor
            }
            _ => ContextBudgetClass::ContinuationAnchor,
        },
        ContextItemCategory::RuntimeState => ContextBudgetClass::RuntimeState,
        ContextItemCategory::SessionState => ContextBudgetClass::SessionState,
        ContextItemCategory::ToolObservation => ContextBudgetClass::ToolObservation,
        ContextItemCategory::Attachment => ContextBudgetClass::Attachment,
        ContextItemCategory::Conversation => {
            if matches!(
                recency,
                ContextBudgetRecency::CurrentTurn | ContextBudgetRecency::Recent
            ) {
                ContextBudgetClass::RecentConversation
            } else {
                ContextBudgetClass::HistoricalConversation
            }
        }
        ContextItemCategory::Extension => ContextBudgetClass::Extension,
    }
}

fn item_recency(item: &ContextItem, recent_start: Option<u64>) -> ContextBudgetRecency {
    if is_latest_user(item) || item.taxonomy.scope == ContextItemScope::Turn {
        return ContextBudgetRecency::CurrentTurn;
    }
    if item.taxonomy.scope == ContextItemScope::Stable {
        return ContextBudgetRecency::Stable;
    }
    let sequence = item_sequence(item);
    if sequence.is_some_and(|sequence| recent_start.is_some_and(|start| sequence >= start)) {
        ContextBudgetRecency::Recent
    } else {
        ContextBudgetRecency::Historical
    }
}

fn item_recoverability(item: &ContextItem) -> ContextRecoverability {
    if item.kind == "semantic_anchor" || item.kind == "checkpoint" {
        return ContextRecoverability::DurableReference;
    }
    if micro_compaction_from_metadata(&item.metadata)
        .is_some_and(|compaction| compaction.recovery.durable)
    {
        return ContextRecoverability::DurableReference;
    }
    if matches!(item.taxonomy.compaction, ContextCompactionPolicy::Rebuild)
        || matches!(
            item.taxonomy.category,
            ContextItemCategory::Instruction | ContextItemCategory::RuntimeState
        )
    {
        return ContextRecoverability::Rebuildable;
    }
    if matches!(
        item.taxonomy.category,
        ContextItemCategory::Conversation
            | ContextItemCategory::ToolObservation
            | ContextItemCategory::Attachment
            | ContextItemCategory::SessionState
    ) {
        return ContextRecoverability::SessionLedger;
    }
    ContextRecoverability::InlineOnly
}

fn recent_message_start(items: &[ContextItem], recent_user_turns: u64) -> Option<u64> {
    let mut users = items
        .iter()
        .filter(|item| item.metadata.get("role").and_then(Value::as_str) == Some("user"))
        .filter_map(item_sequence)
        .collect::<Vec<_>>();
    users.sort_unstable();
    users.dedup();
    users
        .len()
        .checked_sub(recent_user_turns as usize)
        .and_then(|index| users.get(index).copied())
        .or_else(|| users.first().copied())
}

fn item_sequence(item: &ContextItem) -> Option<u64> {
    item.metadata
        .get("message_index")
        .or_else(|| item.metadata.get("source_message_index"))
        .and_then(Value::as_u64)
}

fn is_latest_user(item: &ContextItem) -> bool {
    item.metadata
        .get("latest_user")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || (item.pinned
            && item.metadata.get("role").and_then(Value::as_str) == Some("user")
            && item.kind == "message")
}

fn semantic_anchor_kind(item: &ContextItem) -> Option<SemanticAnchorKind> {
    item.metadata
        .get("semantic_anchor")
        .and_then(|value| value.get("kind"))
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn item_anchor_rank(item: &ContextItem) -> u8 {
    match semantic_anchor_kind(item) {
        Some(SemanticAnchorKind::Goal) => 0,
        Some(SemanticAnchorKind::Constraint) => 1,
        Some(SemanticAnchorKind::Blocker) => 2,
        Some(SemanticAnchorKind::Decision) => 3,
        Some(SemanticAnchorKind::CriticalContext) => 4,
        Some(SemanticAnchorKind::NextStep) => 5,
        Some(SemanticAnchorKind::File) => 6,
        Some(SemanticAnchorKind::RecoveryPoint) => 7,
        Some(SemanticAnchorKind::Progress) => 8,
        None => 9,
    }
}

fn assistant_tool_call_ids(item: &ContextItem) -> Vec<String> {
    if item.metadata.get("role").and_then(Value::as_str) != Some(role_name(Role::Assistant)) {
        return Vec::new();
    }
    item.metadata
        .get("message_metadata")
        .and_then(|value| value.get("tool_calls"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|call| {
            call.get("id")
                .or_else(|| call.get("call_id"))
                .or_else(|| call.get("tool_call_id"))
                .and_then(Value::as_str)
        })
        .filter(|call_id| !call_id.is_empty())
        .map(ToString::to_string)
        .collect()
}

const fn role_name(role: Role) -> &'static str {
    match role {
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::User => "user",
        Role::Tool => "tool",
    }
}

fn scope_rank(scope: ContextItemScope) -> u8 {
    match scope {
        ContextItemScope::Turn => 0,
        ContextItemScope::Session => 1,
        ContextItemScope::Stable => 2,
    }
}

fn own_group_id(item: &ContextItem) -> String {
    group_id(std::slice::from_ref(&item.id))
}

fn group_id(member_ids: &[String]) -> String {
    let mut ids = member_ids.to_vec();
    ids.sort();
    let digest = format!("{:x}", Sha1::digest(ids.join("\n").as_bytes()));
    format!("context-group:{}", &digest[..20])
}

fn stable_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            let body = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("json key serializes"),
                        stable_json(&map[key])
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(stable_json).collect::<Vec<_>>().join(",")
        ),
        _ => serde_json::to_string(value).expect("json value serializes"),
    }
}

fn is_sha1_hash(value: &str) -> bool {
    value.strip_prefix("sha1:").is_some_and(|digest| {
        digest.len() == 40 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::*;
    use crate::{CONTEXT_ITEM_TAXONOMY_SCHEMA_VERSION, ContextItemTaxonomy};

    fn message(id: &str, role: &str, index: u64, tokens: u64, pinned: bool) -> ContextItem {
        let kind = if role == "tool" {
            "tool_result"
        } else {
            "message"
        };
        let mut item = ContextItem::new(id, kind, format!("session.messages[{index}]"), id, 40);
        item.token_estimate = tokens;
        item.pinned = pinned;
        item.metadata = BTreeMap::from([
            ("role".to_string(), json!(role)),
            ("message_index".to_string(), json!(index)),
            ("latest_user".to_string(), json!(pinned && role == "user")),
            ("tool_call_id".to_string(), Value::Null),
        ]);
        item
    }

    fn extension(id: &str, tokens: u64) -> ContextItem {
        let mut item = ContextItem::new(id, "diagnostic", "fixture", id, 10);
        item.token_estimate = tokens;
        item
    }

    #[test]
    fn layered_allocation_matches_versioned_golden_and_borrows_unused_quota() {
        let mut recent = message("assistant:recent", "assistant", 4, 20, false);
        recent.priority = 45;
        let items = vec![
            message("user:old", "user", 0, 20, false),
            message("tool:recent", "tool", 2, 30, false),
            message("user:latest", "user", 3, 20, true),
            recent,
            extension("extension:turn", 10),
        ];
        let projection = allocate_context_budget(
            &items,
            Some(80),
            &ContextBudgetAllocationPolicy {
                recent_user_turns: 1,
                ..ContextBudgetAllocationPolicy::default()
            },
        );
        let actual = serde_json::to_value(projection.allocation.expect("allocation"))
            .expect("allocation serializes");
        let expected: Value = serde_json::from_str(include_str!(
            "../tests/golden/rust_rewrite/context_budget_allocation.json"
        ))
        .expect("allocation golden parses");

        assert_eq!(actual, expected);
        assert!(projection.included.contains("user:latest"));
        assert!(projection.included.contains("assistant:recent"));
        assert!(projection.included.contains("tool:recent"));
        assert!(projection.included.contains("extension:turn"));
        assert_eq!(
            projection.dropped.get("user:old").map(String::as_str),
            Some("model_context_budget")
        );
    }

    #[test]
    fn goal_constraint_and_latest_user_are_hard_required_and_overflow_is_explainable() {
        let mut goal = ContextItem::new(
            "anchor:goal:primary",
            "semantic_anchor",
            "semantic_anchor.registry",
            "goal",
            94,
        );
        goal.token_estimate = 30;
        goal.pinned = true;
        goal.metadata
            .insert("semantic_anchor".to_string(), json!({"kind": "goal"}));
        let mut constraint = ContextItem::new(
            "anchor:constraint:fixture",
            "semantic_anchor",
            "semantic_anchor.registry",
            "constraint",
            94,
        );
        constraint.token_estimate = 30;
        constraint.pinned = true;
        constraint
            .metadata
            .insert("semantic_anchor".to_string(), json!({"kind": "constraint"}));
        let latest = message("user:latest", "user", 2, 20, true);
        let old = message("user:old", "user", 0, 25, false);
        let mut runtime = ContextItem::new("runtime:large", "runtime", "runtime", "runtime", 90);
        runtime.token_estimate = 60;
        runtime.pinned = true;
        let projection = allocate_context_budget(
            &[goal, constraint, latest, old, runtime],
            Some(80),
            &ContextBudgetAllocationPolicy::default(),
        );
        let allocation = projection.allocation.expect("allocation");

        assert!(projection.included.contains("user:latest"));
        assert!(projection.included.contains("anchor:goal:primary"));
        assert!(projection.included.contains("anchor:constraint:fixture"));
        assert_eq!(allocation.hard_required_tokens, 140);
        assert_eq!(allocation.hard_selected_tokens, 80);
        assert_eq!(allocation.hard_overflow_tokens, 60);
        assert_eq!(
            projection.dropped.get("runtime:large").map(String::as_str),
            Some("required_budget_exhausted")
        );
        assert_eq!(
            projection.decisions["anchor:constraint:fixture"].class,
            ContextBudgetClass::CriticalAnchor
        );
    }

    #[test]
    fn assistant_tool_call_and_result_are_allocated_atomically() {
        let mut assistant = message("assistant:call", "assistant", 1, 20, false);
        assistant.metadata.insert(
            "message_metadata".to_string(),
            json!({"tool_calls": [{"id": "call-read"}]}),
        );
        let mut result = message("tool:call-read", "tool", 2, 30, false);
        result
            .metadata
            .insert("tool_call_id".to_string(), json!("call-read"));
        let items = vec![assistant, result];

        let dropped =
            allocate_context_budget(&items, Some(40), &ContextBudgetAllocationPolicy::default());
        assert!(dropped.included.is_empty());
        assert_eq!(dropped.dropped.len(), 2);
        assert_eq!(
            dropped.decisions["assistant:call"].group_id,
            dropped.decisions["tool:call-read"].group_id
        );

        let selected =
            allocate_context_budget(&items, Some(50), &ContextBudgetAllocationPolicy::default());
        assert!(selected.included.contains("assistant:call"));
        assert!(selected.included.contains("tool:call-read"));
    }

    #[test]
    fn invalid_policy_falls_back_to_the_versioned_default() {
        let invalid = ContextBudgetAllocationPolicy {
            schema_version: "future".to_string(),
            recent_user_turns: 0,
            soft_quota_basis_points: BTreeMap::new(),
        };
        let projection = allocate_context_budget(
            &[message("user:latest", "user", 0, 10, true)],
            Some(10),
            &invalid,
        );
        let allocation = projection.allocation.expect("allocation");
        assert_eq!(
            allocation.policy_hash,
            ContextBudgetAllocationPolicy::default().policy_hash()
        );
        assert!(allocation.validate().is_ok());
    }

    #[test]
    fn test_fixture_taxonomy_is_current() {
        let item = message("user:fixture", "user", 0, 10, false);
        assert_eq!(
            item.taxonomy,
            ContextItemTaxonomy::classify("message", "session.messages[0]")
        );
        assert_eq!(
            item.taxonomy.schema_version,
            CONTEXT_ITEM_TAXONOMY_SCHEMA_VERSION
        );
    }
}
