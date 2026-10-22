# Roundtable CLI

Connect a Mac or Linux machine to Roundtable and supervise Codex and Claude Code sessions.
The runtime is Rust. Node is only an npm launcher; background services run the native binary.
Install the npm tarball from the public GitHub release. No npm registry account is needed. The installer downloads a checksum-verified native
binary; it does not build Rust or start background services. Node 20+ is required for npm.

```sh
roundtable_package="https://github.com/roundtable-bot/roundtable-cli/releases/download/v0.2.18/roundtable-cli-0.2.18.tgz"
npm install -g --allow-remote=root --allow-scripts="$roundtable_package" "$roundtable_package"
roundtable login
roundtable setup
```

Open https://roundtable.bot/cli/sessions to view machines, sessions, activity and usage.

Create a hackathon SDK project with a template embedded in the native binary:

```sh
roundtable init acme --template diligence
# Optional: --server http://localhost:8000 --project acme-team --force
```

Choose `diligence`, `sourcing`, `portfolio`, or `funds`. The command reads a team
key from `ROUNDTABLE_KEY` or a hidden terminal prompt; it does not use or save a
machine login. Copy a team key from Agents → Connect agent. A single writable
team bank is selected automatically; use `--project BANK` when several are available.
Bot keys and their actual names are written to `.env` with mode 0600, and `.env`
is added to `.gitignore`. Seed files already in the bank are skipped. Choose an
empty destination or use `--force` to allow replacing project files.

Source builds embed available files from the repository's top-level `templates/`
directory at compile time, without downloading templates. Missing or incomplete
templates produce a runtime error explaining how to rebuild with the complete
template. Test fixtures under `cli/tests/fixtures/` are never shipped as templates.

Installation alone does not read or upload conversations. `setup` enables metadata monitoring
at user login. Enable visible prompt/response collection for selected projects and remote control:

```sh
roundtable monitor --content /absolute/project/path --allow-control
```

The service updates itself. Roundtable names the latest release and the SHA-256 of each
platform's binary; the service downloads that binary from the GitHub release, checks the digest
and version, and restarts in place once no Roundtable-managed session is running. It never
installs an older version. Use `roundtable monitor --updates manual` to update only by hand,
and `--updates auto` to turn automatic updates back on.

Grant each assistant access from the Coding sessions page. Monitor, conversation read, prompting
and interruption are separate permissions. Grants expire after 24 hours in the UI. Machine-wide
grants explicitly cover future sessions. Memory-bank permissions do not grant machine control.

```sh
roundtable sessions list --json
roundtable sessions start codex --cwd /absolute/project/path
roundtable sessions start claude --cwd /absolute/project/path
roundtable sessions prompt codex:SESSION_ID "Run the focused test suite"
roundtable sessions events codex:SESSION_ID --follow
roundtable sessions interrupt codex:SESSION_ID
roundtable commands list
roundtable commands resolve COMMAND_ID --outcome delivered
```

Managed sessions default to read-only Codex sandboxing or Claude's Read/Grep/Glob tools with
`dontAsk` permissions. Add `--workspace-write` when starting a session to allow workspace edits.
Existing Codex sessions on a shared app-server are discoverable and controllable. Standalone
Codex and existing Claude terminal sessions are observed; Roundtable never opens a second writer
against a live Claude conversation. Use `sessions start claude` for remotely promptable Claude.
Provider CLIs must already be installed and signed in. Their credentials stay on the machine.
T3 Code desktop sessions are also discovered from the running same-user host's open
local database. Active runs use the original Codex/Claude session ID, so they remain
visible after a shared provider closes the transcript file without creating duplicates.
This fallback is observation-only (`source: hook`): queued/waiting work is `waiting`,
and running work with no direct provider activity evidence is `unknown`, with a T3
status note. Both appear in Active Sessions. It expires when T3 closes, the run ends,
or the thread is archived/deleted. It never resumes, interrupts, or writes to T3.
Local prompts require an idle session. Remote queued guidance waits for the next idle turn and
expires after 60 seconds. After an uncertain delivery, inspect the provider before resolving
the command as `delivered` or `not-delivered`; resolution never sends the prompt again.

```sh
roundtable service status
roundtable service stop
roundtable service start
roundtable service uninstall
roundtable monitor --metadata-only --deny-control
roundtable logout
```

When the OS credential store is unavailable (headless Linux or a locked macOS Keychain), `roundtable login --insecure-storage` stores
the machine credential in a private 0600 file. On Linux without systemd, run `roundtable daemon`
under your own supervisor. macOS uses a user LaunchAgent; Linux uses a systemd user service.
Services start at login, not before login. Stopping the service ends Roundtable-managed provider
processes. Existing external terminal sessions are unaffected. Stop deliberately before installing
an upgraded service binary with `roundtable service install`.

Telemetry syncs every two seconds, with five-second inventory reconciliation and bounded retry
backoff. Pending uploads survive restarts and are retained until acknowledged. Transcript reading
pauses when uploads fall behind and resumes from its saved position as the queue drains.
Open sessions take priority over historical backfill, and session status continues during catch-up.
Keep provider transcripts until the backlog clears. Observed transcript collection includes
visible prompts, replies, tool inputs (including edits), and tool results.
Codex project paths and collection scope follow the working directory reported for each turn.
Deleted worktrees under an enabled root remain covered; use `monitor --metadata-only` to revoke collection.
Long text is uploaded in consecutive chunks; Claude subagents have separate histories.
Provider records larger than 16 MB are reported as coverage gaps. Usage fields
are cumulative provider reports; unavailable fields are null. No token-price estimate is invented.
Server activity history retains up to seven days and about 20,000 events per machine.
Interrupting managed Claude currently ends its process; start another session to continue.
Claude input totals include uncached and cached tokens; cached input is a subset of input.
The dashboard shows stale/offline data explicitly and excludes it from current session counts.
Every sync reports the running native daemon's version, which can differ from the npm
launcher if the service has not been reinstalled. The server retains that version with
the machine's last-seen time. Machine-wide monitors receive `cli_version`,
`latest_cli_version`, and `cli_update_available` in machine inventory; unknown versions
have a null update flag. The Machines view shows the version and links outdated clients
to upgrade instructions. Offline machines retain their last reported version.

macOS arm64/x64 and Linux glibc arm64/x64 packages are built by the release workflow. Linux x64
requires glibc 2.35+, Linux arm64 requires 2.39+. Musl/Alpine is not packaged. To build from source:

```sh
cargo install --path cli --locked
```

Download native binaries without Node from [GitHub Releases](https://github.com/roundtable-bot/roundtable-cli/releases).
All four binaries and SHA256SUMS are built by the release workflow. The source is public;
no open-source license grant has been added (package license: UNLICENSED).

Runtime and control require a matching Roundtable server. Supported external sessions expose
capabilities individually; ordinary existing Claude terminals remain observation-only.

[Verified flows and platform limits](https://github.com/roundtable-bot/roundtable-cli/blob/main/VERIFICATION.md).
