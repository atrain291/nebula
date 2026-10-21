//! Inbox commands serialize review/archive/launch. Launch uses the existing
//! agent creator and atomically persists its session link before spawning.
use crate::registry::{CreateAgentSpec, Daemon};
use anyhow::{ensure, Result};
use nebula_core::{AgentRole, EntityId, InboxDraft, InboxId};
use std::sync::Arc;

impl Daemon {
    pub(crate) async fn launch_inbox(
        self: &Arc<Self>,
        id: InboxId,
        draft: InboxDraft,
    ) -> Result<EntityId> {
        let _gate = self.inbox_ops.lock().await;
        let lookup = id.clone();
        let item = self.store_blocking(move |s| s.get_inbox(&lookup)).await?;
        if let Some(session) = item.session {
            // Includes deleted sessions: never silently turn a retry into new work.
            return Ok(EntityId::Agent(session));
        }
        ensure!(!item.archived, "inbox item is archived");
        ensure!(
            item.review_revision == draft.review_revision,
            "task details changed; refresh and review before launch"
        );
        let save_id = id.clone();
        let saved = draft.clone();
        self.store_blocking(move |s| s.save_inbox(&save_id, &saved))
            .await?;
        let prompt = nebula_core::inbox_launch_prompt(&draft.instructions, &item.review);
        let result = self
            .create_agent_for_inbox(
                CreateAgentSpec {
                    worktree: draft.worktree,
                    name: "inbox task".into(),
                    kind: draft.kind,
                    custom_harness: draft.custom_harness,
                    model: draft.model,
                    effort: draft.effort,
                    auto_title: true,
                    cloud_prompt: None,
                    starting_prompt: Some(prompt),
                    pr_url: None,
                    issue_url: item.source_url,
                    role: AgentRole::Worker,
                },
                Some(id.clone()),
            )
            .await;
        if result.is_err() {
            self.store_blocking(move |s| s.rollback_inbox_launch(&id))
                .await?;
        }
        result
    }
}
