use serde::{Deserialize, Serialize};

pub const CONTEXT_ITEM_TAXONOMY_SCHEMA_VERSION: &str = "openagent.context_item_taxonomy.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextItemCategory {
    Instruction,
    Conversation,
    ToolObservation,
    Attachment,
    Skill,
    ToolManifest,
    RuntimeState,
    SessionState,
    Extension,
}

impl ContextItemCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Instruction => "instruction",
            Self::Conversation => "conversation",
            Self::ToolObservation => "tool_observation",
            Self::Attachment => "attachment",
            Self::Skill => "skill",
            Self::ToolManifest => "tool_manifest",
            Self::RuntimeState => "runtime_state",
            Self::SessionState => "session_state",
            Self::Extension => "extension",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextItemOrigin {
    AgentProfile,
    InstructionFile,
    LegacySystem,
    SystemAssembly,
    SessionMessage,
    TurnAttachment,
    SkillDocument,
    SkillCatalog,
    ToolRegistry,
    Runtime,
    Sandbox,
    WorkState,
    Todo,
    Checkpoint,
    Extension,
}

impl ContextItemOrigin {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentProfile => "agent_profile",
            Self::InstructionFile => "instruction_file",
            Self::LegacySystem => "legacy_system",
            Self::SystemAssembly => "system_assembly",
            Self::SessionMessage => "session_message",
            Self::TurnAttachment => "turn_attachment",
            Self::SkillDocument => "skill_document",
            Self::SkillCatalog => "skill_catalog",
            Self::ToolRegistry => "tool_registry",
            Self::Runtime => "runtime",
            Self::Sandbox => "sandbox",
            Self::WorkState => "work_state",
            Self::Todo => "todo",
            Self::Checkpoint => "checkpoint",
            Self::Extension => "extension",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextItemScope {
    Stable,
    Session,
    Turn,
}

impl ContextItemScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Session => "session",
            Self::Turn => "turn",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompactionPolicy {
    Preserve,
    Summarize,
    Truncate,
    Rebuild,
    Drop,
}

impl ContextCompactionPolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::Summarize => "summarize",
            Self::Truncate => "truncate",
            Self::Rebuild => "rebuild",
            Self::Drop => "drop",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextItemTaxonomy {
    pub schema_version: String,
    pub category: ContextItemCategory,
    pub origin: ContextItemOrigin,
    pub scope: ContextItemScope,
    pub compaction: ContextCompactionPolicy,
}

impl Default for ContextItemTaxonomy {
    fn default() -> Self {
        Self {
            schema_version: String::new(),
            category: ContextItemCategory::Extension,
            origin: ContextItemOrigin::Extension,
            scope: ContextItemScope::Turn,
            compaction: ContextCompactionPolicy::Drop,
        }
    }
}

impl ContextItemTaxonomy {
    #[must_use]
    pub fn classify(kind: &str, source: &str) -> Self {
        use ContextCompactionPolicy::{Drop, Preserve, Rebuild, Summarize, Truncate};
        use ContextItemCategory::{
            Attachment, Conversation, Extension, Instruction, RuntimeState, SessionState, Skill,
            ToolManifest, ToolObservation,
        };
        use ContextItemOrigin::{
            AgentProfile, Checkpoint, InstructionFile, LegacySystem, Runtime, Sandbox,
            SessionMessage, SkillCatalog, SkillDocument, SystemAssembly, Todo, ToolRegistry,
            TurnAttachment, WorkState,
        };
        use ContextItemScope::{Session, Stable, Turn};

        let (category, origin, scope, compaction) = match kind {
            "profile_prompt" => (Instruction, AgentProfile, Stable, Preserve),
            "instruction" => (Instruction, InstructionFile, Stable, Preserve),
            "legacy_system" => (Instruction, LegacySystem, Stable, Preserve),
            "message" if source == "context.system_sources" => {
                (Instruction, SystemAssembly, Stable, Preserve)
            }
            "message" => (Conversation, SessionMessage, Session, Summarize),
            "tool_result" => (ToolObservation, SessionMessage, Session, Summarize),
            "skill_preloaded" => (Skill, SkillDocument, Stable, Preserve),
            "skill_available" => (Skill, SkillCatalog, Stable, Rebuild),
            "mcp_tool_manifest" | "tool_manifest" => (ToolManifest, ToolRegistry, Stable, Rebuild),
            "runtime" => (RuntimeState, Runtime, Turn, Preserve),
            "sandbox" => (RuntimeState, Sandbox, Session, Preserve),
            "work_state" => (SessionState, WorkState, Session, Preserve),
            "todo" => (SessionState, Todo, Session, Preserve),
            "checkpoint" => (SessionState, Checkpoint, Session, Summarize),
            value if value.starts_with("attachment_") => {
                (Attachment, TurnAttachment, Session, Truncate)
            }
            _ => (Extension, ContextItemOrigin::Extension, Turn, Drop),
        };
        Self {
            schema_version: CONTEXT_ITEM_TAXONOMY_SCHEMA_VERSION.to_string(),
            category,
            origin,
            scope,
            compaction,
        }
    }

    #[must_use]
    pub fn is_current(&self) -> bool {
        self.schema_version == CONTEXT_ITEM_TAXONOMY_SCHEMA_VERSION
    }
}
