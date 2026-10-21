use super::*;
use nebula_core::{InboxDraft, InboxId, InboxItem, MAX_CLOUD_PROMPT_BYTES, MAX_INBOX_SOURCE_BYTES};

fn validate_text(text: &str, limit: usize) -> Result<()> {
    anyhow::ensure!(!text.trim().is_empty(), "task text cannot be empty");
    anyhow::ensure!(
        !text.contains('\0') && text.len() <= limit,
        "task text is too long or contains NUL"
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
        let mut stmt = conn.prepare("SELECT id, original, source_url, draft, archived, session_id, created_at, updated_at FROM inbox ORDER BY created_at DESC, id DESC")?;
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
                },
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (mut item, draft) = row?;
            item.draft = draft.map(|s| serde_json::from_str(&s)).transpose()?;
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
        let changed = self.conn.lock().execute(
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

    /// Only a proven spawn failure can release a launch. User deletion keeps
    /// the link as a tombstone, so an old click cannot recreate a session.
    pub(crate) fn rollback_inbox_launch(&self, id: &InboxId) -> Result<()> {
        self.conn.lock().execute("UPDATE inbox SET session_id = NULL WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM agents WHERE agents.id = inbox.session_id)", [id.as_str()])?;
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
}
