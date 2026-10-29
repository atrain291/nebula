# REMOTE HOSTS: a second nebula stands in for another machine, `fakebox`, reached through an `ssh`
# stand-in on the PATH (socat plays the `-L` forward); its sessions run `/bin/sh`, so the pane paints a
# prompt. It gets a project `api` with two sessions, started from here with `nebula spawn --host` once
# the link is up — so the local daemon starts now, before the harness's own `add`, under the same
# environment it sets.
command -v socat >/dev/null || { echo "shot: remote-hosts needs socat" >&2; exit 1; }
FAR_RT="/tmp/nfar-$$"; mkdir -p "$FAR_RT"; chmod 700 "$FAR_RT"
FAR_DATA="$WORK/far-data"; FAR_REPO="$WORK/api"; SSHBIN="$WORK/sshbin"; mkdir -p "$FAR_REPO" "$SSHBIN"
git -C "$FAR_REPO" init -q -b main
git -C "$FAR_REPO" -c user.name=shot -c user.email=shot@example.invalid commit -q --allow-empty -m "api"
cat > "$SSHBIN/ssh" <<EOF
#!/bin/sh
fwd=""
while [ \$# -gt 0 ]; do
  case "\$1" in -o) shift 2 ;; -L) fwd="\$2"; shift 2 ;; --) shift; break ;; -*) shift ;; *) break ;; esac
done
shift
[ -n "\$fwd" ] && exec socat UNIX-LISTEN:"\${fwd%%:*}",fork,unlink-early UNIX-CONNECT:"\${fwd#*:}"
export HOME='$WORK' NEBULA_RUNTIME_DIR='$FAR_RT' NEBULA_DATA_DIR='$FAR_DATA' NEBULA_AGENT_CMD=/bin/sh SHELL=/bin/sh \
  NEBULA_UPDATE_CHECK_SECS=0 PATH='$(dirname "$BIN"):/usr/bin:/bin'
exec sh -c "\$*"
EOF
chmod +x "$SSHBIN/ssh"
NEBULA_RUNTIME_DIR="$FAR_RT" NEBULA_DATA_DIR="$FAR_DATA" NEBULA_AGENT_CMD=/bin/sh SHELL=/bin/sh "$BIN" add "$FAR_REPO" >/dev/null

mkdir -p "$WORK/data"
printf '{"remote_hosts":["fakebox"]}\n' > "$WORK/data/config.local.json"
export PATH="$SSHBIN:$PATH" NEBULA_RUNTIME_DIR="$RUNTIME" NEBULA_DATA_DIR="$WORK/data" NEBULA_AGENT_CMD=/bin/cat \
       NEBULA_UPDATE_CHECK_SECS=0
(setsid "$BIN" daemon </dev/null >/dev/null 2>&1 &)
for _ in $(seq 60); do "$BIN" remotes 2>/dev/null | grep -q '^fakebox connected' && break; sleep 0.5; done
"$BIN" spawn --host fakebox --project api "profile the replay parser on the GPU box" >/dev/null
"$BIN" spawn --host fakebox --project api --worktree perf-replay "benchmark the replay parser" >/dev/null
# The far daemon outlives the harness's cleanup, which only knows the local one.
trap 'cleanup; kill "$(cat "$FAR_RT/daemon.pid" 2>/dev/null)" 2>/dev/null; rm -rf "$FAR_RT"' EXIT
