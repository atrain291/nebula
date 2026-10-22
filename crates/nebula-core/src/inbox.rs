//! Manual task records. Suggestions/evaluations belong beside these records,
//! never in the immutable source or in instructions approved by the user.
use crate::{AgentId, AgentKind, InboxId, WorktreeId};
use serde::{Deserialize, Serialize};

pub const INBOX_PROTOCOL: u32 = 54;
pub const MAX_INBOX_SOURCE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InboxOutcome {
    #[default]
    NeedsReview,
    Accepted,
    NeedsChanges,
    Abandoned,
}

impl InboxOutcome {
    pub const ALL: [Self; 4] = [
        Self::NeedsReview,
        Self::Accepted,
        Self::NeedsChanges,
        Self::Abandoned,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsReview => "Needs review",
            Self::Accepted => "Accepted",
            Self::NeedsChanges => "Needs changes/rework",
            Self::Abandoned => "Abandoned",
        }
    }
}

/// Only explicitly edited user review; never populated from imported material.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InboxReview {
    pub done_when: String,
    pub constraints: String,
    /// Descriptive text, never an instruction to Nebula to execute a shell.
    pub verification_plan: String,
    pub outcome: InboxOutcome,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxRevision {
    pub event_id: String,
    pub revision: i64,
    pub review: InboxReview,
    pub saved_at: i64,
}

/// Recorded atomically with the session link; None options mean no CLI flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxLaunch {
    pub review_revision: i64,
    pub review: InboxReview,
    pub prompt: String,
    pub kind: AgentKind,
    pub custom_harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub launched_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxDraft {
    #[serde(default)]
    pub review_revision: i64,
    /// Instructions composed in Quick Prompt; the daemon appends reviewed criteria once.
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
    pub review: InboxReview,
    pub review_revision: i64,
    /// Most recent 20 snapshots. The complete audit remains in SQLite.
    pub review_history: Vec<InboxRevision>,
    pub launch: Option<InboxLaunch>,
}

/// Compose from approved fields only, once at the daemon launch boundary.
/// Original imported content and outcome opinions are deliberately absent.
pub fn inbox_launch_prompt(instructions: &str, review: &InboxReview) -> String {
    let mut prompt = instructions.to_string();
    for (heading, text) in [
        ("Done when (user reviewed)", &review.done_when),
        ("Constraints (user reviewed)", &review.constraints),
        (
            "Verification plan (user reviewed; descriptive only)",
            &review.verification_plan,
        ),
    ] {
        if !text.trim().is_empty() {
            prompt.push_str(&format!("\n\n## {heading}\n{text}"));
        }
    }
    prompt.trim().to_string()
}

impl InboxId {
    pub fn is_valid(&self) -> bool {
        self.as_str().parse::<ulid::Ulid>().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inbox_prompt_includes_only_approved_fields_once() {
        let review = InboxReview {
            done_when: "criteria-marker".into(),
            constraints: "constraint-marker".into(),
            verification_plan: "plan-marker: cargo test".into(),
            outcome: InboxOutcome::Accepted,
            note: "opinion-marker".into(),
        };
        let prompt = inbox_launch_prompt("instruction-marker", &review);
        for marker in [
            "instruction-marker",
            "criteria-marker",
            "constraint-marker",
            "plan-marker",
        ] {
            assert_eq!(prompt.matches(marker).count(), 1);
        }
        assert!(!prompt.contains("opinion-marker"));
        assert!(!prompt.contains("Accepted"));
        assert_eq!(
            inbox_launch_prompt("  approved instruction  ", &InboxReview::default()),
            "approved instruction"
        );
        assert!(prompt.contains("descriptive only"));
        assert_eq!(
            inbox_launch_prompt("exact old instructions", &InboxReview::default()),
            "exact old instructions"
        );
    }
    #[test]
    fn inbox_legacy_json_draft_and_review_receive_safe_defaults() {
        let draft: InboxDraft = serde_json::from_value(serde_json::json!({
            "instructions":"Original approved prompt", "worktree":"wt", "kind":"claude",
            "custom_harness":null, "model":null, "effort":null
        }))
        .unwrap();
        assert_eq!(draft.review_revision, 0);
        assert_eq!(
            serde_json::from_str::<InboxReview>("{}").unwrap(),
            InboxReview::default()
        );
    }
}
