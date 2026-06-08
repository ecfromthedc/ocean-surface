# Ocean Surface — Ops

## ocean-surface-proxy is supervised by launchd (OCEAN-161)

The **surface proxy** (`crates/ocean-surface-proxy`, built to
`target/release/ocean-surface-proxy`) serves the compiled PWA bundle from `dist/`
and reverse-proxies `/v1/*` to the Ocean daemon. It listens on
**`0.0.0.0:8790`** by default.

Previously it was hand-launched via `run-surface.sh` with **no supervision** — if
it crashed, the web surface went **silently offline**. It is now run under a
launchd **LaunchAgent** that respawns it on crash (`KeepAlive`) and starts it at
login (`RunAtLoad`).

| Thing | Value |
|---|---|
| launchd label | `dev.risingtides.ocean-surface-proxy` |
| Version-controlled plist | `deploy/dev.risingtides.ocean-surface-proxy.plist` |
| Launcher it execs | `deploy/ocean-surface-proxy.sh` |
| Installed plist path | `~/Library/LaunchAgents/dev.risingtides.ocean-surface-proxy.plist` |
| Bind address | `0.0.0.0:8790` (env `OCEAN_SURFACE_BIND`) |
| Bundle served | `dist/` (env `OCEAN_SURFACE_DIST`) |
| Daemon proxied to | `http://127.0.0.1:4780` (env `OCEAN_DAEMON_URL`) |
| Logs (stdout+stderr) | `/private/tmp/ocean-surface-proxy.log` |

> The launcher serves a **prebuilt** bundle — it does **not** run `trunk build` on
> every respawn (that's what `run-surface.sh` is for during dev). The wasm bundle
> is built once at install time. Rebuild + reinstall after UI changes by re-running
> the install script.
>
> The xAI voice key is **not** stored in the plist. The binary resolves it from
> `~/.config/ocean-surface/xai.key` (or env `XAI_API_KEY`). HTTP Basic auth is
> **on by default** (the binary's built-in operator creds); set
> `OCEAN_SURFACE_AUTH=off` in the plist's `EnvironmentVariables` only for trusted
> localhost.

### Install / enable supervision

```bash
ops/install-surface-proxy.sh
```

This builds the proxy (release), ensures a valid `dist/` bundle exists, copies the
plist into `~/Library/LaunchAgents/`, then bootstraps + enables + kickstarts the
job. Idempotent — safe to re-run after a pull/rebuild. (Equivalent manual steps:
`cp deploy/dev.risingtides.ocean-surface-proxy.plist ~/Library/LaunchAgents/` then
`launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.risingtides.ocean-surface-proxy.plist`
and `launchctl enable gui/$(id -u)/dev.risingtides.ocean-surface-proxy`.)

### Check status

```bash
# Is it listening?
lsof -nP -iTCP:8790 -sTCP:LISTEN

# launchd's view (state, pid, last exit code):
launchctl print gui/$(id -u)/dev.risingtides.ocean-surface-proxy | grep -E 'state|pid|last exit'

# Unauthenticated health endpoint:
curl -fsS http://127.0.0.1:8790/health && echo
```

### Restart / read logs

```bash
# Force a restart (e.g. after rebuilding the binary or bundle):
launchctl kickstart -k gui/$(id -u)/dev.risingtides.ocean-surface-proxy

# Tail logs:
tail -f /private/tmp/ocean-surface-proxy.log
```

### Uninstall / stop supervision

```bash
ops/uninstall-surface-proxy.sh
```

Boots the job out of launchd and removes the installed plist. The repo, the built
binary, and `dist/` are left untouched.

> **Note on the daemon:** the Ocean **daemon** (`:4780`) on this box is currently
> hand-launched and is **not** covered by this LaunchAgent — this ticket only
> supervises the **surface proxy**. Supervision state on this box drifts, so
> re-verify with `launchctl list | grep -i ocean` before assuming either process
> is supervised.
