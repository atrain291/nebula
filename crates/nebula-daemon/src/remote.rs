//! REMOTE HOST LINKS: for each ssh destination in `config.local.json`'s
//! `remote_hosts` (see `config::remote_hosts`), keep that machine's daemon
//! reachable through a local unix socket.
//!
//! Each link is two ssh calls. The first runs `nebula remote-endpoint` on
//! the remote, which starts its daemon when none is running and prints the
//! daemon's socket path. The second, `ssh -N -L <local>:<remote>`, forwards
//! a socket under `paths::remotes_dir()` to it and stays up. Nothing new
//! listens on the network: the ssh channel is the only way in, and the
//! runtime dir's 0700 guards the local end, as it guards `daemon.sock`.
//!
//! A link counts as up once the remote daemon answers a `Hello` through
//! the forward, and is checked the same way every [`HEALTH_EVERY`]. When
//! ssh exits or the remote stops answering, the link is torn down and
//! retried with backoff. The host list is reread every [`RECONCILE_EVERY`],
//! so editing `remote_hosts` needs no restart.

use crate::registry::Daemon;
use anyhow::{anyhow, bail, Context, Result};
use nebula_core::clock::now_ms;
use nebula_core::codec::{read_frame, write_frame};
use nebula_core::paths;
use nebula_core::protocol::{
    ClientRequest, RemoteHostStatus, RemoteLinkState, ServerEvent, PROTOCOL_VERSION,
};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

const RECONCILE_EVERY: Duration = Duration::from_secs(5);
const HEALTH_EVERY: Duration = Duration::from_secs(20);
/// How long the endpoint call, or the forward coming up, may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const BACKOFF_MIN: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// A link that stayed up this long starts its next retry at
/// [`BACKOFF_MIN`] again.
const STABLE_AFTER: Duration = Duration::from_secs(60);

/// Options on every ssh call. The daemon has no one to answer a prompt, so
/// `BatchMode` fails fast instead of hanging on a password or host-key
/// question; the keepalives notice a dead network inside a minute; and the
/// links skip the user's `ControlMaster`, whose mux client hands a `-N -L`
/// to the master and exits, which would read here as the link dropping.
const SSH_OPTS: &[&str] = &[
    "-T",
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=15",
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=3",
    "-o",
    "ControlMaster=no",
    "-o",
    "ControlPath=none",
];

/// Runs under `sh -c` on the remote. sshd hands it a bare PATH, so it
/// prepends the same directories `nebula ssh`'s prelude does (see
/// `install_prelude!` in the `nebula` crate). Unlike `nebula ssh` it never
/// installs anything: a background link has no one to show progress to.
/// No single quotes, backslashes or newlines, so it survives one layer of
/// '...' under any login shell.
const ENDPOINT_SCRIPT: &str = concat!(
    "export PATH=\"$HOME/.local/bin:$PATH:/opt/homebrew/bin:/usr/local/bin\"; ",
    "command -v nebula >/dev/null 2>&1 || { ",
    "echo \"nebula is not installed on this host\" >&2; exit 127; }; ",
    "exec nebula remote-endpoint"
);

/// The links' states, for `ListRemoteHosts`.
#[derive(Default)]
pub struct Remotes {
    states: Mutex<BTreeMap<String, RemoteLinkState>>,
    /// Each link's `ssh -N`, by host, while it runs — for an IN-PLACE
    /// RESTART, whose `exec` keeps the runtime and so never drops the
    /// `Child` that would kill it ([`Remotes::kill_links`]).
    forwards: Mutex<BTreeMap<String, u32>>,
}

impl Remotes {
    /// Every link, in `remote_hosts` order.
    pub fn snapshot(&self, order: &[String]) -> Vec<RemoteHostStatus> {
        let states = self.states.lock();
        order
            .iter()
            .filter_map(|host| {
                Some(RemoteHostStatus {
                    host: host.clone(),
                    socket: paths::remote_socket_path(host),
                    state: states.get(host)?.clone(),
                })
            })
            .collect()
    }

    fn set(&self, host: &str, state: RemoteLinkState) {
        self.states.lock().insert(host.to_owned(), state);
    }

    fn forget(&self, host: &str) {
        self.states.lock().remove(host);
    }

    /// End every link's `ssh -N` now. Called just before an IN-PLACE
    /// RESTART `exec`s: the new image opens links of its own, and an old
    /// forward left running would hold its connection open for good,
    /// keepalives and all, as a child nothing waits on.
    pub fn kill_links(&self) {
        for (host, pid) in std::mem::take(&mut *self.forwards.lock()) {
            let pid = nix::unistd::Pid::from_raw(pid as i32);
            if let Err(e) = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM) {
                tracing::debug!(%host, error = %e, "remote host forward already gone");
            }
        }
    }
}

/// A link's `ssh -N` in [`Remotes::forwards`] for as long as this lives.
struct Tracked<'a> {
    remotes: &'a Remotes,
    host: &'a str,
}

impl<'a> Tracked<'a> {
    fn new(remotes: &'a Remotes, host: &'a str, ssh: &Child) -> Self {
        if let Some(pid) = ssh.id() {
            remotes.forwards.lock().insert(host.to_owned(), pid);
        }
        Self { remotes, host }
    }
}

impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        self.remotes.forwards.lock().remove(self.host);
    }
}

/// Keep one link task per configured host until the daemon shuts down.
pub async fn supervise(daemon: Arc<Daemon>) {
    let mut links: BTreeMap<String, CancellationToken> = BTreeMap::new();
    let mut ignored: Vec<String> = Vec::new();
    let mut interval = tokio::time::interval(RECONCILE_EVERY);
    loop {
        tokio::select! {
            _ = daemon.shutdown.cancelled() => break,
            _ = interval.tick() => {}
        }
        let read = tokio::task::spawn_blocking(crate::config::remote_hosts)
            .await
            .unwrap_or_default();
        // Logged when the list changes, not on every reread.
        if read.ignored != ignored {
            if !read.ignored.is_empty() {
                tracing::warn!(entries = ?read.ignored, "ignoring remote_hosts entries that are not ssh destinations");
            }
            ignored = read.ignored;
        }
        let wanted = read.hosts;
        links.retain(|host, stop| {
            let keep = wanted.contains(host);
            if !keep {
                tracing::info!(%host, "remote host removed; closing its link");
                stop.cancel();
                daemon.remotes.forget(host);
            }
            keep
        });
        for host in wanted {
            if links.contains_key(&host) {
                continue;
            }
            tracing::info!(%host, "opening remote host link");
            let stop = daemon.shutdown.child_token();
            links.insert(host.clone(), stop.clone());
            daemon.remotes.set(&host, RemoteLinkState::Connecting);
            tokio::spawn(run_link(daemon.remotes.clone(), host, stop));
        }
    }
    for stop in links.values() {
        stop.cancel();
    }
}

/// One host's link, reopened with backoff until `stop`.
async fn run_link(remotes: Arc<Remotes>, host: String, stop: CancellationToken) {
    let socket = paths::remote_socket_path(&host);
    let mut backoff = BACKOFF_MIN;
    while !stop.is_cancelled() {
        remotes.set(&host, RemoteLinkState::Connecting);
        let opened = tokio::time::Instant::now();
        let error = match hold_link(&remotes, &host, &socket, &stop).await {
            Ok(()) => break,
            Err(e) => format!("{e:#}"),
        };
        if stop.is_cancelled() {
            break;
        }
        if opened.elapsed() >= STABLE_AFTER {
            backoff = BACKOFF_MIN;
        }
        tracing::warn!(%host, %error, retry_in = ?backoff, "remote host link down");
        remotes.set(
            &host,
            RemoteLinkState::Retrying {
                error,
                retry_at_ms: now_ms() + backoff.as_millis() as i64,
            },
        );
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
    let _ = std::fs::remove_file(&socket);
}

/// Bring the link up and hold it. `Ok` only when `stop` ends it; any other
/// way it ends is the error the next retry reports.
async fn hold_link(
    remotes: &Remotes,
    host: &str,
    socket: &Path,
    stop: &CancellationToken,
) -> Result<()> {
    let remote_socket = tokio::select! {
        _ = stop.cancelled() => return Ok(()),
        r = remote_endpoint(host) => r?,
    };
    ensure_remotes_dir()?;
    let (mut ssh, last_err) = spawn_forward(host, socket, &remote_socket)?;
    let _tracked = Tracked::new(remotes, host, &ssh);
    let protocol_version = tokio::select! {
        _ = stop.cancelled() => return Ok(()),
        r = await_forward(&mut ssh, &last_err, socket) => r?,
    };
    tracing::info!(%host, protocol_version, "remote host link up");
    remotes.set(
        host,
        RemoteLinkState::Connected {
            protocol_version,
            remote_socket,
            since_ms: now_ms(),
        },
    );
    let mut health = tokio::time::interval(HEALTH_EVERY);
    health.tick().await;
    loop {
        tokio::select! {
            _ = stop.cancelled() => return Ok(()),
            status = ssh.wait() => {
                let status = status.context("wait for ssh")?;
                bail!("ssh exited ({status}){}", stderr_tail(&last_err).await);
            }
            _ = health.tick() => {
                if let Err(e) = hello(socket).await {
                    return Err(e.context("remote daemon stopped answering"));
                }
            }
        }
    }
}

/// Run [`ENDPOINT_SCRIPT`] on `host`: its daemon is running once this
/// returns, at the socket path returned.
async fn remote_endpoint(host: &str) -> Result<PathBuf> {
    let output = Command::new("ssh")
        .args(SSH_OPTS)
        .arg("--")
        .arg(host)
        .arg(format!("sh -c '{ENDPOINT_SCRIPT}' nebula-remote"))
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(CONNECT_TIMEOUT, output)
        .await
        .map_err(|_| anyhow!("ssh {host} timed out"))?
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => anyhow!("ssh not found on PATH"),
            _ => anyhow!(e).context("run ssh"),
        })?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        bail!(endpoint_failure(output.status.code(), &stderr));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let path = stdout.lines().map(str::trim).rfind(|l| !l.is_empty());
    match path {
        Some(path) if path.starts_with('/') => Ok(PathBuf::from(path)),
        _ => bail!("the remote nebula printed no daemon socket"),
    }
}

/// What a failed endpoint call means, worded for `nebula remotes`.
fn endpoint_failure(code: Option<i32>, stderr: &str) -> String {
    // nebula's own failures print `Error: <what>` and then its causes;
    // the `Error:` line says what went wrong, a cause only how.
    let line = stderr
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("Error: "))
        .or_else(|| stderr.lines().map(str::trim).rfind(|l| !l.is_empty()));
    match (code, line) {
        (Some(127), _) => {
            "nebula is not installed there; run `nebula ssh <host>` once to install it".into()
        }
        // A nebula from before REMOTE HOST LINKS: clap refuses the
        // subcommand, or, where a bare word is a folder to open, nebula
        // looks for a folder named after it.
        _ if stderr.contains("unrecognized subcommand")
            || stderr.contains("remote-endpoint does not exist") =>
        {
            "the nebula there is too old for remote links; run `nebula upgrade` there".into()
        }
        // ssh's own failures (unreachable, auth, host key) exit 255.
        (Some(255), Some(line)) if !line.starts_with("ssh:") => format!("ssh: {line}"),
        (_, Some(line)) => line.to_owned(),
        (code, None) => format!("remote command failed (exit {code:?})"),
    }
}

fn ensure_remotes_dir() -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = paths::remotes_dir();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("create {}", dir.display()))
}

/// The last line a forward's ssh wrote to stderr.
type LastLine = std::sync::Arc<Mutex<String>>;

fn spawn_forward(host: &str, socket: &Path, remote_socket: &Path) -> Result<(Child, LastLine)> {
    // `-L` splits its argument at `:`, so a path holding one can't be
    // forwarded at all; say so rather than let ssh misread it.
    for path in [socket, remote_socket] {
        if path.to_string_lossy().contains(':') {
            bail!(
                "can't forward {}: ssh's -L can't take a path with ':' in it",
                path.display()
            );
        }
    }
    // StreamLocalBindUnlink clears a socket file a previous link left
    // behind; without it the bind fails and ExitOnForwardFailure ends ssh.
    let mut ssh = Command::new("ssh")
        .args(SSH_OPTS)
        .args([
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "StreamLocalBindUnlink=yes",
            "-L",
        ])
        .arg(format!("{}:{}", socket.display(), remote_socket.display()))
        .arg("--")
        .arg(host)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn ssh")?;
    // Read as it is written: OpenSSH logs a line for every channel the far
    // end refuses, and a pipe left unread fills and stalls ssh on the next
    // one. Only the last line is kept, for the error when ssh exits (the
    // same reason `nebula tunnel` drains its own).
    let last = LastLine::default();
    if let Some(stderr) = ssh.stderr.take() {
        let last = last.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if !line.is_empty() {
                    *last.lock() = line.to_owned();
                }
            }
        });
    }
    Ok((ssh, last))
}

/// Poll the forward until the remote daemon answers through it, or ssh
/// gives up. The socket file appears as soon as ssh binds it, before the
/// session is fully up, so only a handshake proves the link.
async fn await_forward(ssh: &mut Child, last_err: &LastLine, socket: &Path) -> Result<u32> {
    let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
    let mut last_error = anyhow!("forward did not come up");
    while tokio::time::Instant::now() < deadline {
        if let Some(status) = ssh.try_wait().context("wait for ssh")? {
            bail!("ssh exited ({status}){}", stderr_tail(last_err).await);
        }
        match hello(socket).await {
            Ok(version) => return Ok(version),
            // An incompatible daemon will not become compatible by waiting.
            Err(e) if e.downcast_ref::<Incompatible>().is_some() => return Err(e),
            Err(e) => last_error = e,
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(last_error.context("timed out waiting for the forward"))
}

#[derive(Debug)]
struct Incompatible(u32);

impl std::fmt::Display for Incompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "remote daemon speaks protocol v{}, this one v{PROTOCOL_VERSION}; upgrade the older side",
            self.0
        )
    }
}

impl std::error::Error for Incompatible {}

/// Handshake with the daemon behind `socket`, then hang up: the protocol
/// version it answered with.
async fn hello(socket: &Path) -> Result<u32> {
    let exchange = async {
        let mut stream = UnixStream::connect(socket).await?;
        write_frame(
            &mut stream,
            &ClientRequest::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await?;
        match read_frame::<ServerEvent, _>(&mut stream).await? {
            Some(ServerEvent::HelloOk {
                protocol_version, ..
            }) if nebula_core::protocol_compatible_with(protocol_version) => Ok(protocol_version),
            Some(ServerEvent::HelloOk {
                protocol_version, ..
            }) => Err(Incompatible(protocol_version).into()),
            Some(ServerEvent::Incompatible {
                daemon_protocol_version,
            }) => Err(Incompatible(daemon_protocol_version).into()),
            // ssh accepts on the forward even when nothing is listening at
            // the far end, then hangs up.
            None => Err(anyhow!("no daemon is listening on the remote socket")),
            other => Err(anyhow!("unexpected handshake reply: {other:?}")),
        }
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, exchange)
        .await
        .map_err(|_| anyhow!("handshake timed out"))?
}

/// `": <last stderr line>"` from an exited ssh, or nothing. Waits a beat
/// first: the line that says why ssh quit can trail its exit.
async fn stderr_tail(last: &LastLine) -> String {
    tokio::time::sleep(Duration::from_millis(100)).await;
    let line = last.lock().clone();
    if line.is_empty() {
        String::new()
    } else {
        format!(": {line}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_survives_single_quoting() {
        assert!(!ENDPOINT_SCRIPT.contains('\''));
        assert!(!ENDPOINT_SCRIPT.contains('\\'));
        assert!(!ENDPOINT_SCRIPT.contains('\n'));
    }

    /// The script run the way sshd runs it: a stub `nebula` on the PATH is
    /// exec'd with `remote-endpoint`, and a host without one says so.
    #[test]
    fn the_script_execs_the_endpoint_or_reports_no_nebula() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".local/bin");
        let run = || {
            std::process::Command::new("sh")
                .args(["-c", ENDPOINT_SCRIPT, "nebula-remote"])
                .env("HOME", home.path())
                .env("PATH", "/usr/bin:/bin")
                .output()
                .expect("sh")
        };
        let missing = run();
        assert_eq!(missing.status.code(), Some(127));
        assert!(String::from_utf8_lossy(&missing.stderr).contains("not installed"));

        std::fs::create_dir_all(&bin).unwrap();
        let nebula = bin.join("nebula");
        std::fs::write(&nebula, "#!/bin/sh\necho \"/run/sock $*\"\n").unwrap();
        std::fs::set_permissions(&nebula, std::fs::Permissions::from_mode(0o755)).unwrap();
        let found = run();
        assert!(found.status.success());
        assert_eq!(
            String::from_utf8_lossy(&found.stdout).trim(),
            "/run/sock remote-endpoint"
        );
    }

    #[test]
    fn endpoint_failures_read_as_what_to_do() {
        assert!(endpoint_failure(Some(127), "").contains("not installed"));
        assert!(endpoint_failure(
            Some(2),
            "error: unrecognized subcommand 'remote-endpoint'\n\nUsage: nebula"
        )
        .contains("too old"));
        assert_eq!(
            endpoint_failure(
                Some(255),
                "ssh: connect to host box port 22: No route to host\n"
            ),
            "ssh: connect to host box port 22: No route to host"
        );
        assert_eq!(
            endpoint_failure(Some(255), "Permission denied (publickey).\n"),
            "ssh: Permission denied (publickey)."
        );
        assert!(endpoint_failure(
            Some(1),
            "Error: remote-endpoint does not exist\n\nCaused by:\n    No such file or directory (os error 2)\n"
        )
        .contains("too old"));
        assert_eq!(
            endpoint_failure(Some(1), "Error: disk full\n\nCaused by:\n    os error 28\n"),
            "disk full"
        );
        assert_eq!(endpoint_failure(Some(1), "boom\n\n"), "boom");
    }

    /// A fake daemon on a socket answers the handshake; `hello` reads its
    /// version, and refuses one too old to talk to.
    #[tokio::test]
    async fn hello_reads_the_remote_version() {
        let dir = tempfile::tempdir().unwrap();
        for (version, ok) in [(PROTOCOL_VERSION, true), (1, false)] {
            let sock = dir.path().join(format!("{version}.sock"));
            let listener = tokio::net::UnixListener::bind(&sock).unwrap();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let _: Option<ClientRequest> = read_frame(&mut stream).await.unwrap();
                write_frame(
                    &mut stream,
                    &ServerEvent::HelloOk {
                        protocol_version: version,
                        daemon_pid: 1,
                    },
                )
                .await
                .unwrap();
            });
            let result = hello(&sock).await;
            assert_eq!(result.is_ok(), ok, "{version}: {result:?}");
            if !ok {
                assert!(result.unwrap_err().downcast_ref::<Incompatible>().is_some());
            }
        }
    }

    /// An IN-PLACE RESTART's `exec` drops no `Child`, so a tracked forward
    /// has to be ended by pid; one no longer tracked is left alone.
    #[tokio::test]
    async fn kill_links_ends_every_tracked_forward() {
        let remotes = Remotes::default();
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let tracked = Tracked::new(&remotes, "desk", &child);
        remotes.kill_links();
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("the forward ends")
            .unwrap();
        assert!(!status.success());
        drop(tracked);
        assert!(remotes.forwards.lock().is_empty());

        let mut other = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        drop(Tracked::new(&remotes, "srv", &other));
        remotes.kill_links();
        assert!(
            other.try_wait().unwrap().is_none(),
            "untracked keeps running"
        );
    }

    #[tokio::test]
    async fn a_path_with_a_colon_is_refused_before_ssh_runs() {
        let err = spawn_forward("desk", Path::new("/run/a:b.sock"), Path::new("/run/d.sock"))
            .expect_err("refused");
        assert!(
            format!("{err:#}").contains("can't take a path with ':'"),
            "{err:#}"
        );
    }

    #[test]
    fn snapshot_keeps_config_order_and_skips_unknown_hosts() {
        let remotes = Remotes::default();
        remotes.set("b", RemoteLinkState::Connecting);
        remotes.set("a", RemoteLinkState::Connecting);
        let order = ["b".to_owned(), "nope".to_owned(), "a".to_owned()];
        let hosts: Vec<_> = remotes
            .snapshot(&order)
            .into_iter()
            .map(|s| s.host)
            .collect();
        assert_eq!(hosts, ["b", "a"]);
    }
}
