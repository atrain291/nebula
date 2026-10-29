//! REMOTE HOSTS in the grid: the sessions of other machines' daemons,
//! beside this machine's.
//!
//! The local daemon keeps an ssh link to each host under `remote_hosts`
//! and forwards that host's daemon socket to a local one (the daemon's
//! `remote.rs`). The TUI asks it which links are up on a slow beat
//! (`ListRemoteHosts`), opens a connection of its own to each up link's
//! socket, and subscribes there as it does locally. That host's rows join
//! [`Tree`] with their owner recorded in [`Tree::remote`], so the grid,
//! the PROJECT TABS and the status alerts take them in like any other.
//!
//! Every request still leaves through the loop's one queue; [`route`]
//! sends each to the daemon owning the rows it names, so a remote session
//! is attached, typed into, created, renamed, archived or deleted as a
//! local one is. A request no one daemon can take — rows of two machines,
//! or something built from this machine's own disk, `gh` or inbox aimed
//! at a remote row — is refused here with an Error the loop lands like a
//! daemon's own, so a change shown on the keypress rolls back.

use crate::app::{entity_key, App, Tree, TreeRows};
use nebula_core::codec::{read_frame, write_frame};
use nebula_core::protocol::{RemoteHostStatus, RemoteLinkState, REMOTE_HOSTS_PROTOCOL};
use nebula_core::{ClientRequest, Entity, ServerEvent, SessionRef};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

/// How often the TUI asks the local daemon how its links are doing.
pub const POLL: Duration = Duration::from_secs(5);

/// What the refusal of a request aimed at a remote row says.
pub const REFUSED: &str = "not available on remote hosts";

/// News from a remote connection, tagged with its host.
#[derive(Debug)]
pub enum Msg {
    Event(String, Box<ServerEvent>),
    /// The connection ended; its rows have to go.
    Closed(String, String),
}

struct Conn {
    socket: PathBuf,
    tx: mpsc::UnboundedSender<ClientRequest>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The TUI's connections to remote daemons, owned by the event loop.
pub struct Conns {
    msg_tx: mpsc::UnboundedSender<Msg>,
    pub rx: mpsc::UnboundedReceiver<Msg>,
    conns: HashMap<String, Conn>,
    /// The req ids sent to each host and not yet answered: an Ack or Error
    /// counts only from the host it was sent to, and a connection that
    /// closes fails the ones it still owed.
    pending: HashMap<u64, String>,
}

impl Default for Conns {
    fn default() -> Self {
        let (msg_tx, rx) = mpsc::unbounded_channel();
        Self {
            msg_tx,
            rx,
            conns: HashMap::new(),
            pending: HashMap::new(),
        }
    }
}

impl Conns {
    /// Match the connections to the links the local daemon reports: one to
    /// each that is up, none to the rest. The hosts whose connection this
    /// closed are returned, for their rows to go.
    pub fn sync(&mut self, hosts: &[RemoteHostStatus]) -> Vec<String> {
        let up: HashMap<&str, &RemoteHostStatus> = hosts
            .iter()
            .filter(|h| matches!(h.state, RemoteLinkState::Connected { .. }))
            .map(|h| (h.host.as_str(), h))
            .collect();
        let closed: Vec<String> = self
            .conns
            .iter()
            .filter(|(host, conn)| {
                up.get(host.as_str())
                    .is_none_or(|h| h.socket != conn.socket)
            })
            .map(|(host, _)| host.clone())
            .collect();
        for host in &closed {
            self.conns.remove(host);
        }
        for (host, status) in up {
            if self.conns.contains_key(host) {
                continue;
            }
            let (tx, req_rx) = mpsc::unbounded_channel();
            let task = tokio::spawn(run(
                host.to_owned(),
                status.socket.clone(),
                req_rx,
                self.msg_tx.clone(),
            ));
            self.conns.insert(
                host.to_owned(),
                Conn {
                    socket: status.socket.clone(),
                    tx,
                    task,
                },
            );
        }
        closed
    }

    /// Hand `req` to `host`'s connection; false when there is none.
    pub fn send(&mut self, host: &str, req: ClientRequest) -> bool {
        let req_id = req_id_of(&req);
        let sent = self
            .conns
            .get(host)
            .is_some_and(|conn| conn.tx.send(req).is_ok());
        if let (true, Some(req_id)) = (sent, req_id) {
            self.pending.insert(req_id, host.to_owned());
        }
        sent
    }

    /// Was `req_id` sent to `host`? Forgets it: an id is answered once.
    fn answered_by(&mut self, req_id: u64, host: &str) -> bool {
        match self.pending.get(&req_id) {
            Some(h) if h == host => self.pending.remove(&req_id).is_some(),
            _ => false,
        }
    }

    /// The req ids `host` still owed, forgotten.
    fn owed_by(&mut self, host: &str) -> Vec<u64> {
        let owed: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, h)| h.as_str() == host)
            .map(|(id, _)| *id)
            .collect();
        for id in &owed {
            self.pending.remove(id);
        }
        owed
    }

    /// Forget a connection that reported itself closed.
    pub fn forget(&mut self, host: &str) {
        self.conns.remove(host);
    }
}

/// One remote connection: handshake, subscribe, then relay requests out
/// and events in until either side ends it.
async fn run(
    host: String,
    socket: PathBuf,
    mut req_rx: mpsc::UnboundedReceiver<ClientRequest>,
    msg_tx: mpsc::UnboundedSender<Msg>,
) {
    let relay = async {
        let stream = tokio::net::UnixStream::connect(&socket).await?;
        let conn = crate::ipc::handshake(stream).await?;
        if conn.protocol_version < REMOTE_HOSTS_PROTOCOL {
            anyhow::bail!(
                "remote daemon speaks protocol v{}, older than remote hosts",
                conn.protocol_version
            );
        }
        let (mut rd, mut wr) = conn.stream.into_split();
        write_frame(&mut wr, &ClientRequest::Subscribe).await?;
        let writer = async move {
            while let Some(req) = req_rx.recv().await {
                write_frame(&mut wr, &req).await?;
            }
            anyhow::Ok(())
        };
        let reader = async {
            loop {
                match read_frame::<ServerEvent, _>(&mut rd).await? {
                    Some(ev) => {
                        if msg_tx.send(Msg::Event(host.clone(), Box::new(ev))).is_err() {
                            return anyhow::Ok(());
                        }
                    }
                    None => anyhow::bail!("remote daemon closed the connection"),
                }
            }
        };
        tokio::select! {
            r = writer => r,
            r = reader => r,
        }
    };
    let why = match relay.await {
        Ok(()) => "connection closed".to_owned(),
        Err(e) => format!("{e:#}"),
    };
    let _ = msg_tx.send(Msg::Closed(host, why));
}

/// A request's `req_id`, for the ones the daemon answers with an Ack or
/// an Error. The PTY traffic carries none, and is the bulk of what a remote
/// connection sends, so it is ruled out before anything is encoded.
fn req_id_of(req: &ClientRequest) -> Option<u64> {
    use ClientRequest as R;
    match req {
        R::Input { .. } | R::Resize { .. } | R::Attach { .. } | R::Detach { .. } => None,
        // Every variant names it `req_id`; reading it off the encoding
        // keeps this from needing an arm per variant.
        other => serde_json::to_value(other)
            .ok()?
            .as_object()?
            .values()
            .next()?
            .get("req_id")?
            .as_u64(),
    }
}

/// Where one queued request goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Local,
    Host(String),
    /// Can't go to any one daemon: rows of two machines in one request, or
    /// a request built from this machine's own state aimed at a remote row.
    /// Answered with an Error when it carries a `req_id`, dropped when it
    /// doesn't (background work no one is waiting on).
    Refuse(Option<u64>),
}

/// The daemon `req` is for: the one owning every row it names.
pub fn route(tree: &Tree, req: &ClientRequest) -> Route {
    use ClientRequest as R;
    let owner = |req_id: Option<u64>, ids: &[&str]| {
        let mut hosts = ids.iter().map(|id| tree.host_of(id));
        let first = hosts.next().flatten();
        if hosts.any(|h| h != first) {
            return Route::Refuse(req_id);
        }
        first.map_or(Route::Local, |h| Route::Host(h.to_owned()))
    };
    let local_only = |req_id: u64, id: &str| match tree.host_of(id) {
        Some(_) => Route::Refuse(Some(req_id)),
        None => Route::Local,
    };
    let sref = |s: &SessionRef| match s {
        SessionRef::Agent(id) => id.0.clone(),
        SessionRef::Terminal(id) => id.0.clone(),
    };
    match req {
        R::Attach { session, .. }
        | R::Detach { session }
        | R::Input { session, .. }
        | R::Resize { session, .. }
        | R::TailOutput { session, .. } => owner(None, &[&sref(session)]),
        R::MarkAgentSeen { id } => owner(None, &[id.as_str()]),
        R::PrewarmAgent { worktree, .. } | R::PrewarmWorktreeSessions { worktree, .. } => {
            owner(None, &[worktree.as_str()])
        }

        R::RemoveProject { req_id, id } | R::RenameProject { req_id, id, .. } => {
            owner(Some(*req_id), &[id.as_str()])
        }
        R::CreateWorktree {
            req_id, project, ..
        } => owner(Some(*req_id), &[project.as_str()]),
        R::DeleteWorktree { req_id, id, .. }
        | R::CheckWorktree { req_id, id }
        | R::CleanupMergedWorktree { req_id, id, .. }
        | R::CreateAgent {
            req_id,
            worktree: id,
            ..
        }
        | R::CreateTerminal {
            req_id,
            worktree: id,
            ..
        }
        | R::StartRun {
            req_id,
            worktree: id,
        }
        | R::StopRun {
            req_id,
            worktree: id,
        } => owner(Some(*req_id), &[id.as_str()]),
        R::RenameAgent { req_id, id, .. }
        | R::AutoRenameAgent { req_id, id, .. }
        | R::EnterWorktree { req_id, id, .. }
        | R::SpawnSiblingAgent { req_id, id, .. }
        | R::OpenFiles { req_id, id, .. }
        | R::ArchiveAgent { req_id, id }
        | R::UnarchiveAgent { req_id, id }
        | R::DeleteAgent { req_id, id }
        | R::RestartAgent { req_id, id }
        | R::SendCloudMessage { req_id, id, .. } => owner(Some(*req_id), &[id.as_str()]),
        // Within one machine only: a session can't move to another's.
        R::MoveAgent {
            req_id,
            id,
            worktree,
        } => owner(Some(*req_id), &[id.as_str(), worktree.as_str()]),
        R::RenameTerminal { req_id, id, .. } | R::CloseTerminal { req_id, id } => {
            owner(Some(*req_id), &[id.as_str()])
        }
        R::UpdateLink { req_id, id, .. } | R::DeleteLink { req_id, id } => {
            owner(Some(*req_id), &[id.as_str()])
        }

        // Built from this machine: a path picked from its disk, a pull
        // request's checkout found with its `gh`, its daemon's inbox.
        R::SetProjectPath { req_id, id, .. } => local_only(*req_id, id.as_str()),
        R::CreatePrAgent {
            req_id, project, ..
        } => local_only(*req_id, project.as_str()),
        R::LaunchInbox { req_id, draft, .. } => local_only(*req_id, draft.worktree.as_str()),
        _ => Route::Local,
    }
}

/// Ask the local daemon how its links are doing, when it can answer.
pub fn request_status(app: &mut App, out: &mut Vec<ClientRequest>) {
    if app.requests.daemon_protocol < REMOTE_HOSTS_PROTOCOL {
        return;
    }
    crate::event_loop::send(app, out, |req_id| ClientRequest::ListRemoteHosts { req_id });
}

/// The local daemon's answer: remember it for the footer and match the
/// connections to it.
pub fn land_status(
    app: &mut App,
    conns: &mut Conns,
    req_id: u64,
    hosts: Vec<RemoteHostStatus>,
    out: &mut Vec<ClientRequest>,
) {
    app.requests.pending.remove(&req_id);
    for host in conns.sync(&hosts) {
        drop_host(app, &host, out);
    }
    if app.chrome.remote_hosts != hosts {
        app.chrome.remote_hosts = hosts;
        app.chrome.dirty = true;
    }
}

/// An event from a remote connection.
pub fn land(app: &mut App, conns: &mut Conns, msg: Msg, out: &mut Vec<ClientRequest>) {
    match msg {
        Msg::Event(host, event) => land_event(app, conns, &host, *event, out),
        Msg::Closed(host, why) => {
            tracing::info!(%host, %why, "remote host connection closed");
            conns.forget(&host);
            // What it never answered fails here, so a change shown on the
            // keypress rolls back rather than waiting forever.
            for req_id in conns.owed_by(&host) {
                crate::event_loop::handle_server_event(
                    app,
                    ServerEvent::Error {
                        req_id: Some(req_id),
                        message: format!("{host} disconnected before answering"),
                    },
                    out,
                );
            }
            drop_host(app, &host, out);
        }
    }
}

/// One event from `host`'s daemon. It may speak only for its own rows and
/// its own requests: anything naming a row another daemon owns, or
/// answering a request it was not sent, is dropped.
fn land_event(
    app: &mut App,
    conns: &mut Conns,
    host: &str,
    event: ServerEvent,
    out: &mut Vec<ClientRequest>,
) {
    use crate::event_loop::handle_server_event;
    let owns = |app: &App, id: &str| app.tree.host_of(id) == Some(host);
    let owns_session = |app: &App, s: &SessionRef| app.tree.host_of_session(s) == Some(host);
    match event {
        ServerEvent::Snapshot {
            projects,
            worktrees,
            agents,
            terminals,
            links,
            ..
        } => {
            let before = crate::event_loop::selection_snapshot(app);
            let ids: Vec<_> = projects.iter().map(|p| p.id.clone()).collect();
            let dropped = app.tree.replace_rows(
                Some(host),
                TreeRows {
                    projects,
                    worktrees,
                    agents,
                    terminals,
                    links,
                },
            );
            if dropped > 0 {
                tracing::warn!(
                    %host,
                    dropped,
                    "remote host sent rows another daemon already has; is this machine, or one \
                     machine twice, in remote_hosts?"
                );
            }
            // A host's projects get tabs the first time it shows up in
            // this run, behind the local ones; closing one keeps it closed
            // until the next launch.
            if app.chrome.remote_tabs_opened.insert(host.to_owned()) {
                let ids: Vec<_> = ids
                    .into_iter()
                    .filter(|id| owns(app, id.as_str()))
                    .collect();
                for id in ids {
                    if !app.launcher.launcher_tabs.contains(&id) {
                        app.launcher.launcher_tabs.push(id);
                    }
                }
            }
            crate::event_loop::reconcile_selection(app, before, out);
            crate::event_loop::refresh_palette(app);
            app.chrome.dirty = true;
        }
        ServerEvent::EntityUpserted { entity } => {
            let id = match &entity {
                Entity::Project(p) => p.id.0.clone(),
                Entity::Worktree(w) => w.id.0.clone(),
                Entity::Agent(a) => a.id.0.clone(),
                Entity::Terminal(t) => t.id.0.clone(),
                Entity::Link(l) => l.id.0.clone(),
            };
            if app.tree.claimed_elsewhere(&id, host) {
                tracing::warn!(%host, %id, "remote host sent a row another daemon has; dropped");
                return;
            }
            app.tree.remote.insert(id, host.to_owned());
            handle_server_event(app, ServerEvent::EntityUpserted { entity }, out);
        }
        ServerEvent::EntityRemoved { id } => {
            let key = entity_key(&id).to_owned();
            if !owns(app, &key) {
                return;
            }
            let before = live_ids(&app.tree);
            handle_server_event(app, ServerEvent::EntityRemoved { id }, out);
            app.tree.remote.remove(&key);
            // The rows the removal cascaded away client-side.
            let after = live_ids(&app.tree);
            for gone in before.difference(&after) {
                app.tree.remote.remove(gone);
            }
        }
        // The Ack can beat the new row's upsert, which travels on the
        // daemon's broadcast instead; whatever this machine queues for the
        // new row in between must already know where it lives.
        ServerEvent::Ack {
            req_id,
            created: Some(created),
        } => {
            if !conns.answered_by(req_id, host) {
                return;
            }
            let key = entity_key(&created).to_owned();
            if !app.tree.claimed_elsewhere(&key, host) {
                app.tree.remote.insert(key, host.to_owned());
            }
            handle_server_event(
                app,
                ServerEvent::Ack {
                    req_id,
                    created: Some(created),
                },
                out,
            );
        }
        // Status and the attached pane's bytes, for its own sessions only.
        ServerEvent::StatusChanged { ref agent, .. } if !owns(app, agent.as_str()) => {}
        ServerEvent::Scrollback { ref session, .. }
        | ServerEvent::Output { ref session, .. }
        | ServerEvent::SessionExited { ref session, .. }
        | ServerEvent::KittyFlags { ref session, .. }
        | ServerEvent::AttachRefused { ref session, .. }
        | ServerEvent::OutputTail { ref session, .. }
            if !owns_session(app, session) => {}
        event @ (ServerEvent::StatusChanged { .. }
        | ServerEvent::Scrollback { .. }
        | ServerEvent::Output { .. }
        | ServerEvent::SessionExited { .. }
        | ServerEvent::KittyFlags { .. }
        | ServerEvent::AttachRefused { .. }
        | ServerEvent::OutputTail { .. }) => handle_server_event(app, event, out),
        // An Error no request owns would only put its text on screen.
        ServerEvent::Error { req_id: None, .. } => {}
        // Answers to the requests sent to it. Req ids come from the one
        // counter, so they can't be taken for another daemon's.
        ServerEvent::Ack { req_id, .. }
        | ServerEvent::Error {
            req_id: Some(req_id),
            ..
        } if !conns.answered_by(req_id, host) => {}
        event @ (ServerEvent::Ack { .. } | ServerEvent::Error { .. }) => {
            handle_server_event(app, event, out)
        }
        // Meant for that machine's own screen: a review it opened, files
        // an agent there asked for, its inbox and metrics.
        _ => {}
    }
}

/// A host's connection is gone: its rows leave the tree.
fn drop_host(app: &mut App, host: &str, out: &mut Vec<ClientRequest>) {
    if !app.tree.remote.values().any(|h| h == host) {
        return;
    }
    let before = crate::event_loop::selection_snapshot(app);
    app.tree.drop_host(host);
    crate::event_loop::reconcile_selection(app, before, out);
    crate::event_loop::refresh_palette(app);
    app.chrome.dirty = true;
}

/// Every row id in the tree, owned.
fn live_ids(tree: &Tree) -> HashSet<String> {
    tree.ids().map(str::to_owned).collect()
}

/// For a tool that runs git, `gh`, an editor or a file view in a checkout
/// or repo: true, with a flash, when a REMOTE HOST owns the row `id` (a
/// worktree or a project). Its path names a directory on that machine, and
/// a same-named one here is some other checkout. Each such tool asks this
/// itself, so a menu row or a click reaches it as surely as a key.
pub fn refuse_remote(app: &mut App, id: &str) -> bool {
    let remote = app.tree.host_of(id).is_some();
    if remote {
        app.chrome.flash = Some(REFUSED.into());
        app.chrome.dirty = true;
    }
    remote
}

/// [`refuse_remote`] for the selected checkout, or with none selected,
/// the selected project.
pub fn refuse_local_view(app: &mut App) -> bool {
    let id = app
        .selected_worktree()
        .map(|w| w.id.0.clone())
        .or_else(|| app.selected_project().map(|p| p.id.0.clone()));
    id.is_some_and(|id| refuse_remote(app, &id))
}

/// The footer's word on each link: `desktop ✓`, `server ↻`.
pub fn footer_label(hosts: &[RemoteHostStatus]) -> Option<String> {
    if hosts.is_empty() {
        return None;
    }
    let parts: Vec<String> = hosts
        .iter()
        .map(|h| {
            let mark = match h.state {
                RemoteLinkState::Connected { .. } => "✓",
                RemoteLinkState::Connecting => "…",
                RemoteLinkState::Retrying { .. } => "↻",
            };
            format!("{} {mark}", h.host)
        })
        .collect();
    Some(parts.join("  "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nebula_core::{
        Agent, AgentId, AgentKind, AgentStatus, EntityId, Project, ProjectId, Worktree, WorktreeId,
    };

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: ProjectId(id.into()),
            name: name.into(),
            repo_path: format!("/tmp/{name}").into(),
            sort_order: 0,
        }
    }

    fn worktree(id: &str, project: &str) -> Worktree {
        Worktree {
            id: WorktreeId(id.into()),
            project_id: ProjectId(project.into()),
            path: format!("/tmp/{id}").into(),
            branch: "main".into(),
            is_main: true,
            sort_order: 0,
        }
    }

    fn agent(id: &str, worktree: &str) -> Agent {
        Agent {
            id: AgentId(id.into()),
            worktree_id: WorktreeId(worktree.into()),
            name: id.into(),
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
            status_changed_at: 0,
            alive: true,
            issue_url: None,
            role: nebula_core::AgentRole::Worker,
            recent_prompts: Vec::new(),
        }
    }

    fn rows(project_id: &str, name: &str, wt: &str, agents: &[&str]) -> TreeRows {
        TreeRows {
            projects: vec![project(project_id, name)],
            worktrees: vec![worktree(wt, project_id)],
            agents: agents.iter().map(|a| agent(a, wt)).collect(),
            terminals: Vec::new(),
            links: Vec::new(),
        }
    }

    fn snapshot(rows: TreeRows) -> ServerEvent {
        ServerEvent::Snapshot {
            projects: rows.projects,
            worktrees: rows.worktrees,
            agents: rows.agents,
            terminals: rows.terminals,
            links: rows.links,
            pr_seen: Vec::new(),
            ui_state: None,
        }
    }

    /// The local project `api` (w1, a1) and `web` on host `desk` (w9, a9).
    fn app() -> App {
        let mut app = App::new();
        app.tree
            .replace_rows(None, rows("p1", "api", "w1", &["a1"]));
        land_event(
            &mut app,
            &mut Conns::default(),
            "desk",
            snapshot(rows("p9", "web", "w9", &["a9"])),
            &mut Vec::new(),
        );
        app
    }

    fn agent_ids(app: &App) -> Vec<&str> {
        app.tree.agents.iter().map(|a| a.id.as_str()).collect()
    }

    #[test]
    fn each_daemon_replaces_only_its_own_rows() {
        let mut app = app();
        assert_eq!(agent_ids(&app), ["a1", "a9"]);
        assert_eq!(app.tree.host_of("a9"), Some("desk"));
        assert_eq!(app.tree.host_of("a1"), None);

        // A local resync keeps the remote rows, local ones first.
        app.tree
            .replace_rows(None, rows("p1", "api", "w1", &["a1", "a2"]));
        assert_eq!(agent_ids(&app), ["a1", "a2", "a9"]);

        // The host's resync keeps the local rows.
        land_event(
            &mut app,
            &mut Conns::default(),
            "desk",
            snapshot(rows("p9", "web", "w9", &["a8"])),
            &mut Vec::new(),
        );
        assert_eq!(agent_ids(&app), ["a1", "a2", "a8"]);
        assert_eq!(
            app.tree.host_of("a9"),
            None,
            "the old row's owner goes with it"
        );

        app.tree.drop_host("desk");
        assert_eq!(agent_ids(&app), ["a1", "a2"]);
        assert!(app.tree.remote.is_empty());
        assert_eq!(app.tree.projects.len(), 1);
    }

    #[test]
    fn a_host_gets_tabs_once_and_a_label() {
        let mut app = app();
        assert!(app.launcher.launcher_tabs.contains(&ProjectId("p9".into())));
        let web = app
            .tree
            .projects
            .iter()
            .find(|p| p.id.as_str() == "p9")
            .unwrap();
        assert_eq!(app.tree.project_label(web), "web @desk");

        // Closed by the user, it stays closed through a resync.
        app.launcher.launcher_tabs.retain(|id| id.as_str() != "p9");
        land_event(
            &mut app,
            &mut Conns::default(),
            "desk",
            snapshot(rows("p9", "web", "w9", &["a9"])),
            &mut Vec::new(),
        );
        assert!(!app.launcher.launcher_tabs.contains(&ProjectId("p9".into())));
    }

    #[test]
    fn upserts_and_removals_keep_owners_in_step() {
        let mut app = app();
        let mut out = Vec::new();
        land_event(
            &mut app,
            &mut Conns::default(),
            "desk",
            ServerEvent::EntityUpserted {
                entity: Entity::Agent(agent("a7", "w9")),
            },
            &mut out,
        );
        assert_eq!(app.tree.host_of("a7"), Some("desk"));
        // Removing the worktree cascades its sessions away, owners too.
        land_event(
            &mut app,
            &mut Conns::default(),
            "desk",
            ServerEvent::EntityRemoved {
                id: EntityId::Worktree(WorktreeId("w9".into())),
            },
            &mut out,
        );
        assert_eq!(agent_ids(&app), ["a1"]);
        assert_eq!(app.tree.host_of("a7"), None);
        assert_eq!(
            app.tree.host_of("p9"),
            Some("desk"),
            "the project is still there"
        );
    }

    /// The Ack for a create can land before the new row does; whatever is
    /// queued for that row in between must already go to its host.
    #[test]
    fn an_ack_tells_where_the_created_row_lives() {
        let mut app = app();
        let mut conns = Conns::default();
        conns.pending.insert(42, "desk".into());
        land_event(
            &mut app,
            &mut conns,
            "desk",
            ServerEvent::Ack {
                req_id: 42,
                created: Some(EntityId::Agent(AgentId("a5".into()))),
            },
            &mut Vec::new(),
        );
        assert_eq!(
            route(
                &app.tree,
                &ClientRequest::Attach {
                    session: SessionRef::Agent(AgentId("a5".into())),
                    from_seq: None,
                    cols: 80,
                    rows: 24
                }
            ),
            Route::Host("desk".into())
        );
        // An unrelated removal leaves that owner alone.
        land_event(
            &mut app,
            &mut Conns::default(),
            "desk",
            ServerEvent::EntityRemoved {
                id: EntityId::Agent(AgentId("a9".into())),
            },
            &mut Vec::new(),
        );
        assert_eq!(app.tree.host_of("a5"), Some("desk"));
    }

    #[test]
    fn a_closed_connection_takes_its_rows() {
        let mut app = app();
        let mut conns = Conns::default();
        land(
            &mut app,
            &mut conns,
            Msg::Closed("desk".into(), "gone".into()),
            &mut Vec::new(),
        );
        assert_eq!(agent_ids(&app), ["a1"]);
    }

    #[test]
    fn requests_go_to_the_daemon_owning_their_rows() {
        let app = app();
        let tree = &app.tree;
        let remote = SessionRef::Agent(AgentId("a9".into()));
        let local = SessionRef::Agent(AgentId("a1".into()));
        let host = |r| route(tree, &r);

        assert_eq!(
            host(ClientRequest::Attach {
                session: remote.clone(),
                from_seq: None,
                cols: 80,
                rows: 24
            }),
            Route::Host("desk".into())
        );
        assert_eq!(
            host(ClientRequest::Input {
                session: remote,
                data: b"y".to_vec()
            }),
            Route::Host("desk".into())
        );
        assert_eq!(
            host(ClientRequest::Input {
                session: local,
                data: b"y".to_vec()
            }),
            Route::Local
        );
        assert_eq!(
            host(ClientRequest::MarkAgentSeen {
                id: AgentId("a9".into())
            }),
            Route::Host("desk".into())
        );
        // A remote row's own daemon takes the user's edits of it.
        assert_eq!(
            host(ClientRequest::RenameAgent {
                req_id: 7,
                id: AgentId("a9".into()),
                name: "x".into()
            }),
            Route::Host("desk".into())
        );
        assert_eq!(
            host(ClientRequest::PrewarmWorktreeSessions {
                worktree: WorktreeId("w9".into()),
                cols: 80,
                rows: 24
            }),
            Route::Host("desk".into())
        );
        assert_eq!(
            host(ClientRequest::CreateTerminal {
                req_id: 3,
                worktree: WorktreeId("w9".into()),
                name: None
            }),
            Route::Host("desk".into())
        );
        // Moving a session onto another machine is refused.
        assert_eq!(
            host(ClientRequest::MoveAgent {
                req_id: 8,
                id: AgentId("a1".into()),
                worktree: WorktreeId("w9".into())
            }),
            Route::Refuse(Some(8))
        );
        // A path picked from this machine's disk means nothing there.
        assert_eq!(
            host(ClientRequest::SetProjectPath {
                req_id: 4,
                id: ProjectId("p9".into()),
                path: "/tmp/x".into()
            }),
            Route::Refuse(Some(4))
        );
        assert_eq!(
            host(ClientRequest::RenameAgent {
                req_id: 9,
                id: AgentId("a1".into()),
                name: "x".into()
            }),
            Route::Local
        );
        assert_eq!(host(ClientRequest::GetMetrics { req_id: 1 }), Route::Local);
    }

    #[test]
    fn checkout_tools_refuse_a_remote_worktree() {
        let app = app();
        assert!(app.tree.is_remote_worktree(&WorktreeId("w9".into())));
        assert!(!app.tree.is_remote_worktree(&WorktreeId("w1".into())));
    }

    /// This machine in `remote_hosts`, or one machine under two names: the
    /// same daemon answers twice with the same ids. The second copy is
    /// dropped, and the rows stay with whoever had them.
    #[test]
    fn a_host_cannot_take_over_rows_another_daemon_has() {
        let mut app = app();
        // "self" answers with the local daemon's own ids.
        land_event(
            &mut app,
            &mut Conns::default(),
            "self",
            snapshot(rows("p1", "api", "w1", &["a1"])),
            &mut Vec::new(),
        );
        // ... and "desk2" with desk's.
        land_event(
            &mut app,
            &mut Conns::default(),
            "desk2",
            snapshot(rows("p9", "web", "w9", &["a9"])),
            &mut Vec::new(),
        );
        assert_eq!(agent_ids(&app), ["a1", "a9"], "no row twice");
        assert_eq!(app.tree.host_of("a1"), None, "local stays local");
        assert_eq!(app.tree.host_of("a9"), Some("desk"));
        // Nor one row at a time.
        land_event(
            &mut app,
            &mut Conns::default(),
            "self",
            ServerEvent::EntityUpserted {
                entity: Entity::Agent(agent("a1", "w1")),
            },
            &mut Vec::new(),
        );
        assert_eq!(app.tree.host_of("a1"), None);
        // A remote can't remove, or speak for, a row it doesn't own.
        land_event(
            &mut app,
            &mut Conns::default(),
            "desk",
            ServerEvent::EntityRemoved {
                id: EntityId::Agent(AgentId("a1".into())),
            },
            &mut Vec::new(),
        );
        assert_eq!(agent_ids(&app), ["a1", "a9"]);
        // The local daemon wins a clash it resyncs into.
        app.tree
            .replace_rows(None, rows("p1", "api", "w1", &["a1", "a9"]));
        assert_eq!(agent_ids(&app), ["a1", "a9"]);
        assert_eq!(app.tree.host_of("a9"), None);
    }

    /// Every way into a checkout-local tool refuses a remote worktree — a
    /// menu row as surely as a key — and runs nothing here.
    #[test]
    fn checkout_tools_refuse_a_remote_worktree_from_any_entry() {
        use crate::app::MenuAction;
        let mut app = app();
        let w9 = WorktreeId("w9".into());
        for action in [
            MenuAction::SwitchBranch(w9.clone()),
            MenuAction::OpenWorktree(w9.clone()),
        ] {
            app.chrome.flash = None;
            crate::event_loop::run_menu_action(&mut app, action, &mut Vec::new());
            assert_eq!(app.chrome.flash.as_deref(), Some(REFUSED));
            assert!(app.modals.overlay.is_none(), "nothing opened");
        }
        // And the local checkout's menu still opens its switcher.
        app.chrome.flash = None;
        crate::event_loop::run_menu_action(
            &mut app,
            MenuAction::SwitchBranch(WorktreeId("w1".into())),
            &mut Vec::new(),
        );
        assert_ne!(app.chrome.flash.as_deref(), Some(REFUSED));
    }

    #[test]
    fn answers_count_only_from_the_host_asked() {
        let mut app = app();
        let mut conns = Conns::default();
        conns.pending.insert(7, "desk".into());
        // An answer from another host is not desk's to give.
        land_event(
            &mut app,
            &mut conns,
            "srv",
            ServerEvent::Ack {
                req_id: 7,
                created: Some(EntityId::Agent(AgentId("a5".into()))),
            },
            &mut Vec::new(),
        );
        assert_eq!(app.tree.host_of("a5"), None);
        assert!(conns.pending.contains_key(&7));
        // A closed connection fails what it owed.
        app.requests
            .pending
            .insert(7, crate::app::PendingIntent::None);
        land(
            &mut app,
            &mut conns,
            Msg::Closed("desk".into(), "gone".into()),
            &mut Vec::new(),
        );
        assert!(!app.requests.pending.contains_key(&7));
        assert!(conns.pending.is_empty());
    }

    #[test]
    fn the_req_id_is_read_off_any_answered_request() {
        assert_eq!(
            req_id_of(&ClientRequest::RenameAgent {
                req_id: 11,
                id: AgentId("a".into()),
                name: "x".into()
            }),
            Some(11)
        );
        assert_eq!(req_id_of(&ClientRequest::GetMetrics { req_id: 3 }), Some(3));
        assert_eq!(
            req_id_of(&ClientRequest::MarkAgentSeen {
                id: AgentId("a".into())
            }),
            None
        );
        assert_eq!(
            req_id_of(&ClientRequest::Input {
                session: SessionRef::Agent(AgentId("a".into())),
                data: Vec::new()
            }),
            None
        );
    }

    #[test]
    fn the_footer_names_each_link() {
        let status = |host: &str, state| RemoteHostStatus {
            host: host.into(),
            socket: PathBuf::from("/x"),
            state,
        };
        assert_eq!(footer_label(&[]), None);
        assert_eq!(
            footer_label(&[
                status(
                    "desk",
                    RemoteLinkState::Connected {
                        protocol_version: 55,
                        remote_socket: PathBuf::from("/r"),
                        since_ms: 0
                    }
                ),
                status(
                    "srv",
                    RemoteLinkState::Retrying {
                        error: "x".into(),
                        retry_at_ms: 0
                    }
                ),
            ])
            .as_deref(),
            Some("desk ✓  srv ↻")
        );
    }
}
