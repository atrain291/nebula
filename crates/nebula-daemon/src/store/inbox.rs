use super::*;
use nebula_core::{
    InboxDraft, InboxId, InboxItem, InboxReview, InboxRevision, MAX_CLOUD_PROMPT_BYTES,
    MAX_INBOX_SOURCE_BYTES,
};

fn validate_text(text: &str, limit: usize) -> Result<()> {
    anyhow::ensure!(!text.trim().is_empty(), "task text cannot be empty");
    anyhow::ensure!(
        !text.contains('\0') && text.len() <= limit,
        "task text is too long or contains NUL"
    );
    Ok(())
}

fn validate_prompt_budget(instructions: &str, review: &InboxReview) -> Result<()> {
    anyhow::ensure!(
        nebula_core::inbox_launch_prompt(instructions, review).len() <= MAX_CLOUD_PROMPT_BYTES,
        "approved instructions plus task criteria exceed 16 KiB; shorten instructions or criteria"
    );
    Ok(())
}

impl Store {
    pub fn capture_inbox(
        &self,
        id: &InboxId,
        original: &str,
        source_url: Option<&str>,
    ) -> Result<()> {
        validate_text(original, MAX_INBOX_SOURCE_BYTES)?;
        anyhow::ensure!(id.is_valid(), "invalid inbox id");
        if let Some(url) = source_url {
            anyhow::ensure!(url.len() <= 4096, "issue URL is too long");
            crate::pr_scope::validate_issue_url(url)?;
        }
        let conn = self.conn.lock();
        // A capture retry is idempotent, but cannot overwrite the source.
        let old = conn.query_row(
            "SELECT original, source_url FROM inbox WHERE id = ?1",
            [id.as_str()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        );
        match old {
            Ok((text, url)) => anyhow::ensure!(
                text == original && url.as_deref() == source_url,
                "inbox source is immutable"
            ),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                conn.execute("INSERT INTO inbox (id, original, source_url, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)", params![id.as_str(), original, source_url, now_ms()])?;
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    pub fn list_inbox(&self) -> Result<Vec<InboxItem>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id, original, source_url, draft, archived, session_id, created_at, updated_at, review, review_revision, launch FROM inbox ORDER BY created_at DESC, id DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                InboxItem {
                    id: InboxId(r.get(0)?),
                    original: r.get(1)?,
                    source_url: r.get(2)?,
                    draft: None,
                    archived: r.get(4)?,
                    session: r.get::<_, Option<String>>(5)?.map(AgentId),
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                    review: InboxReview::default(),
                    review_revision: r.get(9)?,
                    review_history: vec![],
                    launch: None,
                },
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })?;
        rows.map(|row| {
            let (mut item, draft, review, launch) = row?;
            item.draft = draft.map(|s| serde_json::from_str(&s)).transpose()?;
            item.review = serde_json::from_str(&review)?;
            item.launch = launch.map(|s| serde_json::from_str(&s)).transpose()?;
            let mut history = conn.prepare("SELECT event_id, revision, review, saved_at FROM inbox_review_history WHERE inbox_id = ?1 ORDER BY revision DESC LIMIT 20")?;
            let revisions = history.query_map([item.id.as_str()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)))?;
            item.review_history = revisions.map(|r| {
                let (event_id, revision, json, saved_at) = r?;
                Ok(InboxRevision { event_id, revision, review: serde_json::from_str(&json)?, saved_at })
            }).collect::<Result<Vec<_>>>()?;
            Ok(item)
        })
        .collect()
    }

    pub fn get_inbox(&self, id: &InboxId) -> Result<InboxItem> {
        self.list_inbox()?
            .into_iter()
            .find(|i| &i.id == id)
            .context("inbox item not found")
    }

    pub fn save_inbox(&self, id: &InboxId, draft: &InboxDraft) -> Result<()> {
        validate_text(&draft.instructions, MAX_CLOUD_PROMPT_BYTES)?;
        anyhow::ensure!(
            [&draft.custom_harness, &draft.model, &draft.effort]
                .into_iter()
                .flatten()
                .all(|s| s.len() <= 1024 && !s.contains('\0')),
            "launch option is too long or contains NUL"
        );
        anyhow::ensure!(
            self.get_worktree(&draft.worktree)?.is_some(),
            "worktree not found"
        );
        let conn = self.conn.lock();
        let review_json: String = conn.query_row(
            "SELECT review FROM inbox WHERE id = ?1",
            [id.as_str()],
            |r| r.get(0),
        )?;
        validate_prompt_budget(&draft.instructions, &serde_json::from_str(&review_json)?)?;
        let changed = conn.execute(
            "UPDATE inbox SET draft = ?2, updated_at = ?3 WHERE id = ?1 AND session_id IS NULL AND archived = 0",
            params![id.as_str(), serde_json::to_string(draft)?, now_ms()],
        )?;
        anyhow::ensure!(
            changed == 1,
            "inbox item is missing, archived or already launched"
        );
        Ok(())
    }

    pub fn archive_inbox(&self, id: &InboxId, archived: bool) -> Result<()> {
        anyhow::ensure!(
            self.conn.lock().execute(
                "UPDATE inbox SET archived = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.as_str(), archived, now_ms()]
            )? == 1,
            "inbox item not found"
        );
        Ok(())
    }

    pub fn save_inbox_review(
        &self,
        id: &InboxId,
        event_id: &str,
        expected_revision: i64,
        review: &InboxReview,
    ) -> Result<()> {
        anyhow::ensure!(
            InboxId(event_id.to_string()).is_valid(),
            "invalid review event id"
        );
        let fields = [
            &review.done_when,
            &review.constraints,
            &review.verification_plan,
            &review.note,
        ];
        anyhow::ensure!(
            fields.iter().map(|s| s.len()).sum::<usize>() <= MAX_CLOUD_PROMPT_BYTES
                && fields.iter().all(|s| !s.contains('\0')),
            "task review is too long or contains NUL"
        );
        anyhow::ensure!(expected_revision >= 0, "invalid review revision");
        let revision = expected_revision
            .checked_add(1)
            .context("invalid review revision")?;
        let json = serde_json::to_string(review)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let previous = tx.query_row(
            "SELECT inbox_id, revision, review FROM inbox_review_history WHERE event_id = ?1",
            [event_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        );
        match previous {
            Ok((task, rev, old)) => {
                anyhow::ensure!(
                    task == id.as_str() && rev == revision && old == json,
                    "review event id already used with different content"
                );
                return Ok(());
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => return Err(e.into()),
        }
        let (draft_json, launched): (Option<String>, bool) = tx.query_row(
            "SELECT draft, session_id IS NOT NULL FROM inbox WHERE id = ?1",
            [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if !launched {
            if let Some(json) = draft_json {
                let draft: InboxDraft = serde_json::from_str(&json)?;
                validate_prompt_budget(&draft.instructions, review)?;
            }
        }
        let at = now_ms();
        anyhow::ensure!(tx.execute(
            "UPDATE inbox SET review = ?2, review_revision = ?3, updated_at = ?4 WHERE id = ?1 AND review_revision = ?5",
            params![id.as_str(), json, revision, at, expected_revision],
        )? == 1, "task details changed or task missing; refresh before saving");
        tx.execute("INSERT INTO inbox_review_history (event_id, inbox_id, revision, review, saved_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![event_id, id.as_str(), revision, json, at])?;
        tx.commit()?;
        Ok(())
    }

    /// Only a proven spawn failure can release a launch. User deletion keeps
    /// the link as a tombstone, so an old click cannot recreate a session.
    pub(crate) fn rollback_inbox_launch(&self, id: &InboxId) -> Result<()> {
        self.conn.lock().execute("UPDATE inbox SET session_id = NULL, launch = NULL WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM agents WHERE agents.id = inbox.session_id)", [id.as_str()])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn worktree(store: &Store, path: &Path) -> Worktree {
        let project = Project {
            id: ProjectId::generate(),
            name: "test".into(),
            repo_path: path.into(),
            sort_order: 0,
        };
        store.insert_project(&project).unwrap();
        let worktree = Worktree {
            id: WorktreeId::generate(),
            project_id: project.id,
            path: path.into(),
            branch: "main".into(),
            is_main: true,
            sort_order: 0,
        };
        store.insert_worktree(&worktree).unwrap();
        worktree
    }
    fn draft(w: &Worktree) -> InboxDraft {
        InboxDraft {
            review_revision: 0,
            instructions: "Approved change only".into(),
            worktree: w.id.clone(),
            kind: AgentKind::Claude,
            custom_harness: None,
            model: None,
            effort: None,
        }
    }
    fn agent(id: AgentId, worktree: WorktreeId) -> Agent {
        Agent {
            id,
            worktree_id: worktree,
            name: "task".into(),
            status: AgentStatus::Running,
            archived: false,
            archived_at: 0,
            unseen: false,
            kind: AgentKind::Claude,
            custom_harness: None,
            model: None,
            effort: None,
            session_id: None,
            cloud_session_id: None,
            sort_order: 0,
            status_changed_at: now_ms(),
            alive: false,
            issue_url: None,
            role: nebula_core::AgentRole::Worker,
            recent_prompts: vec![],
        }
    }
    #[test]
    fn inbox_migration_review_and_archive_survive_reopen_without_rewriting_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        {
            let conn = Connection::open(&path).unwrap();
            for (i, migration) in super::super::MIGRATIONS.iter().take(29).enumerate() {
                conn.execute_batch(&format!(
                    "BEGIN; {migration}; PRAGMA user_version = {}; COMMIT;",
                    i + 1
                ))
                .unwrap();
            }
        }
        let id = InboxId::generate();
        let source = "  Imported title\n\nOriginal markdown body\n";
        let approved;
        {
            let s = Store::open(&path).unwrap();
            let wt = worktree(&s, dir.path());
            approved = draft(&wt);
            s.capture_inbox(&id, source, Some("https://github.com/o/r/issues/1"))
                .unwrap();
            assert!(s.get_inbox(&id).unwrap().draft.is_none());
            s.capture_inbox(&id, source, Some("https://github.com/o/r/issues/1"))
                .unwrap();
            assert!(s.capture_inbox(&id, "overwrite", None).is_err());
            s.save_inbox(&id, &approved).unwrap();
            s.archive_inbox(&id, true).unwrap();
            assert!(s.save_inbox(&id, &approved).is_err());
            assert!(
                s.load_tree().unwrap().2.is_empty(),
                "capture/review never creates an agent"
            );
        }
        let s = Store::open(&path).unwrap();
        let item = s.get_inbox(&id).unwrap();
        assert_eq!(item.original, source);
        assert_eq!(item.draft.as_ref(), Some(&approved));
        assert!(item.archived);
        assert!(item.session.is_none());
        s.archive_inbox(&id, false).unwrap();
        assert!(!s.get_inbox(&id).unwrap().archived);
    }
    #[test]
    fn inbox_link_and_agent_insert_are_atomic_and_deletion_keeps_a_launch_tombstone() {
        let s = Store::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let w = worktree(&s, dir.path());
        let id = InboxId::generate();
        s.capture_inbox(&id, "Original", None).unwrap();
        s.save_inbox(&id, &draft(&w)).unwrap();
        let mut a = agent(AgentId(id.0.clone()), WorktreeId("missing".into()));
        assert!(s
            .insert_agent_for_inbox(&a, false, None, None, Some(&id))
            .is_err());
        assert!(
            s.get_inbox(&id).unwrap().session.is_none(),
            "failed transaction must not claim the task"
        );
        a.worktree_id = w.id;
        s.insert_agent_for_inbox(&a, false, None, None, Some(&id))
            .unwrap();
        assert!(s
            .insert_agent_for_inbox(&a, false, None, None, Some(&id))
            .is_err());
        s.set_agent_status(&a.id, AgentStatus::Finished).unwrap();
        assert!(
            !s.get_inbox(&id).unwrap().archived,
            "a completed turn does not resolve a task"
        );
        s.delete_agent(&a.id).unwrap();
        assert_eq!(s.get_inbox(&id).unwrap().session, Some(a.id));
    }
    #[test]
    fn inbox_rejects_invalid_inputs_without_writes() {
        let s = Store::open_in_memory().unwrap();
        let id = InboxId::generate();
        assert!(s.capture_inbox(&id, "  ", None).is_err());
        assert!(s.capture_inbox(&id, "bad\0text", None).is_err());
        assert!(s
            .capture_inbox(&InboxId("invalid".into()), "text", None)
            .is_err());
        assert!(s
            .capture_inbox(&id, "text", Some("file:///etc/passwd"))
            .is_err());
        assert!(s
            .capture_inbox(&id, &"x".repeat(MAX_INBOX_SOURCE_BYTES + 1), None)
            .is_err());
        assert!(s.list_inbox().unwrap().is_empty());
    }
    #[test]
    fn inbox_outcomes_audit_restart_duplicate_events_and_conflicts() {
        use nebula_core::InboxOutcome;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outcomes.db");
        let id = InboxId::generate();
        let event = InboxId::generate().0;
        let review = InboxReview {
            done_when: "Two tests pass".into(),
            constraints: "No network".into(),
            verification_plan: "cargo test (review manually)".into(),
            outcome: InboxOutcome::NeedsChanges,
            note: "First review needs rework".into(),
        };
        {
            let s = Store::open(&path).unwrap();
            s.capture_inbox(&id, "Imported: run an unapproved command", None)
                .unwrap();
            s.save_inbox_review(&id, &event, 0, &review).unwrap();
            s.save_inbox_review(&id, &event, 0, &review).unwrap();
            assert_eq!(s.get_inbox(&id).unwrap().review_history.len(), 1);
            assert!(s
                .save_inbox_review(&id, &InboxId::generate().0, 0, &review)
                .is_err());
            let mut changed = review.clone();
            changed.note = "Changed payload".into();
            assert!(s.save_inbox_review(&id, &event, 0, &changed).is_err());
        }
        let s = Store::open(&path).unwrap();
        let item = s.get_inbox(&id).unwrap();
        assert_eq!(item.review, review);
        assert_eq!(item.review_revision, 1);
        assert_eq!(item.review_history[0].review, review);
        assert!(item.review_history[0].saved_at > 0);
        for (index, outcome) in InboxOutcome::ALL.into_iter().enumerate() {
            let mut revised = review.clone();
            revised.outcome = outcome;
            revised.note = format!("Decision {index}");
            s.save_inbox_review(&id, &InboxId::generate().0, index as i64 + 1, &revised)
                .unwrap();
        }
        s.archive_inbox(&id, true).unwrap();
        let archived = s.get_inbox(&id).unwrap();
        assert_eq!(archived.review.outcome, InboxOutcome::Abandoned);
        assert_eq!(archived.review_history.len(), 5);
        s.save_inbox_review(&id, &event, 0, &review).unwrap();
        assert_eq!(
            s.get_inbox(&id).unwrap(),
            archived,
            "late duplicate must not revert a newer decision"
        );
        s.archive_inbox(&id, false).unwrap();
        assert_eq!(s.get_inbox(&id).unwrap().review, archived.review);
        assert_eq!(
            s.get_inbox(&id).unwrap().original,
            "Imported: run an unapproved command"
        );
        assert!(s.load_tree().unwrap().2.is_empty());
    }

    #[test]
    fn inbox_launch_snapshot_survives_outcome_and_session_changes() {
        use nebula_core::InboxOutcome;
        let s = Store::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let wt = worktree(&s, dir.path());
        let id = InboxId::generate();
        s.capture_inbox(&id, "Unapproved imported instructions", None)
            .unwrap();
        let review = InboxReview {
            done_when: "Approved criteria".into(),
            ..Default::default()
        };
        s.save_inbox_review(&id, &InboxId::generate().0, 0, &review)
            .unwrap();
        let mut draft = draft(&wt);
        draft.review_revision = 1;
        s.save_inbox(&id, &draft).unwrap();
        let mut agent = agent(AgentId(id.0.clone()), wt.id);
        agent.model = Some("explicit-model".into());
        agent.effort = Some("high".into());
        s.insert_agent_for_inbox(&agent, false, None, None, Some(&id))
            .unwrap();
        let launch = s.get_inbox(&id).unwrap().launch.unwrap();
        assert_eq!(launch.review_revision, 1);
        assert_eq!(launch.review, review);
        assert_eq!(
            launch.prompt,
            nebula_core::inbox_launch_prompt(&draft.instructions, &review)
        );
        assert!(!launch.prompt.contains("Unapproved imported"));
        assert_eq!(launch.model, agent.model);
        assert_eq!(launch.effort, agent.effort);
        for status in [
            AgentStatus::Running,
            AgentStatus::Finished,
            AgentStatus::Fresh,
        ] {
            s.set_agent_status(&agent.id, status).unwrap();
            assert_eq!(
                s.get_inbox(&id).unwrap().review.outcome,
                InboxOutcome::NeedsReview
            );
        }
        let accepted = InboxReview {
            outcome: InboxOutcome::Accepted,
            note: "Manually inspected results".into(),
            ..review
        };
        s.save_inbox_review(&id, &InboxId::generate().0, 1, &accepted)
            .unwrap();
        s.set_agent_status(&agent.id, AgentStatus::Running).unwrap();
        s.delete_agent(&agent.id).unwrap();
        let item = s.get_inbox(&id).unwrap();
        assert_eq!(item.review, accepted);
        assert_eq!(item.launch, Some(launch));
        assert!(!item.archived);
    }

    #[test]
    fn inbox_review_history_is_bounded_on_wire_but_audit_is_complete() {
        let s = Store::open_in_memory().unwrap();
        let id = InboxId::generate();
        s.capture_inbox(&id, "Source", None).unwrap();
        for revision in 0..25 {
            s.save_inbox_review(
                &id,
                &InboxId::generate().0,
                revision,
                &InboxReview {
                    note: format!("Review {revision}"),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let item = s.get_inbox(&id).unwrap();
        assert_eq!(item.review_history.len(), 20);
        assert_eq!(item.review_history[0].revision, 25);
        assert_eq!(item.review_history[19].revision, 6);
        let count: i64 = s
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM inbox_review_history", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 25);
        let mut invalid = item.review.clone();
        invalid.note = "x".repeat(MAX_CLOUD_PROMPT_BYTES + 1);
        assert!(s
            .save_inbox_review(&id, &InboxId::generate().0, 25, &invalid)
            .is_err());
        assert_eq!(s.get_inbox(&id).unwrap(), item);
    }
    #[test]
    fn inbox_review_combined_prompt_budget_rejects_saves_without_losing_previous_state() {
        let s = Store::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let wt = worktree(&s, dir.path());
        let id = InboxId::generate();
        s.capture_inbox(&id, "Original", None).unwrap();
        let review = InboxReview {
            done_when: "X".into(),
            ..Default::default()
        };
        let overhead = nebula_core::inbox_launch_prompt("I", &review).len() - 1;
        let mut draft = draft(&wt);
        draft.instructions = "i".repeat(MAX_CLOUD_PROMPT_BYTES - overhead);
        s.save_inbox(&id, &draft).unwrap();
        s.save_inbox_review(&id, &InboxId::generate().0, 0, &review)
            .unwrap();
        let saved = s.get_inbox(&id).unwrap();
        assert_eq!(
            nebula_core::inbox_launch_prompt(&draft.instructions, &review).len(),
            MAX_CLOUD_PROMPT_BYTES
        );
        let mut oversized = review.clone();
        oversized.done_when.push('Y');
        let error = s
            .save_inbox_review(&id, &InboxId::generate().0, 1, &oversized)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("instructions plus task criteria"));
        assert_eq!(s.get_inbox(&id).unwrap(), saved);
        draft.instructions.push('i');
        assert!(s.save_inbox(&id, &draft).is_err());
        assert_eq!(s.get_inbox(&id).unwrap(), saved);
    }

    #[test]
    fn inbox_migration_31_preserves_existing_v30_source_draft_and_session_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old-inbox.db");
        let id = InboxId::generate();
        let original = "  Immutable imported material\n";
        let draft_json = serde_json::json!({
            "instructions":"Existing approved text", "worktree":"removed-worktree", "kind":"claude",
            "custom_harness":null, "model":"saved-model", "effort":null
        })
        .to_string();
        {
            let conn = Connection::open(&path).unwrap();
            for (i, migration) in super::super::MIGRATIONS.iter().take(30).enumerate() {
                conn.execute_batch(&format!(
                    "BEGIN; {migration}; PRAGMA user_version = {}; COMMIT;",
                    i + 1
                ))
                .unwrap();
            }
            conn.execute("INSERT INTO inbox (id, original, draft, archived, session_id, created_at, updated_at) VALUES (?1, ?2, ?3, 1, 'deleted-session', 100, 200)",
                params![id.as_str(), original, draft_json]).unwrap();
        }
        let s = Store::open(&path).unwrap();
        let item = s.get_inbox(&id).unwrap();
        assert_eq!(item.original, original);
        assert_eq!(
            item.draft.as_ref().unwrap().instructions,
            "Existing approved text"
        );
        assert_eq!(
            item.draft.as_ref().unwrap().model.as_deref(),
            Some("saved-model")
        );
        assert_eq!(item.draft.as_ref().unwrap().review_revision, 0);
        assert_eq!(item.session, Some(AgentId("deleted-session".into())));
        assert!(item.archived);
        assert_eq!(item.review, InboxReview::default());
        assert_eq!(item.review_revision, 0);
        assert!(item.review_history.is_empty());
        assert!(item.launch.is_none());
        assert_eq!((item.created_at, item.updated_at), (100, 200));
    }
}
