//! Manual task records. Suggestions/evaluations belong beside these records,
//! never in the immutable source or in instructions approved by the user.
use crate::{AgentId, AgentKind, InboxId, WorktreeId};
use serde::{Deserialize, Serialize};

pub const INBOX_PROTOCOL: u32 = 53;
pub const MAX_INBOX_SOURCE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxDraft {
    /// The exact composed first prompt approved in the Quick Prompt.
    pub instructions: String,
    /// Project selection is represented by this worktree's owning project.
    pub worktree: WorktreeId,
    pub kind: AgentKind,
    pub custom_harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: InboxId,
    /// Immutable capture; never rewritten by editing or by an agent.
    pub original: String,
    pub source_url: Option<String>,
    pub draft: Option<InboxDraft>,
    pub archived: bool,
    /// Kept even if the session is later deleted: retries must not launch again.
    pub session: Option<AgentId>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl InboxId {
    pub fn is_valid(&self) -> bool {
        self.as_str().parse::<ulid::Ulid>().is_ok()
    }
}
