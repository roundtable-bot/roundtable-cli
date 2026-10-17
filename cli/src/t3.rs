use crate::discovery::valid_id;
use crate::state::{Session, State, now};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use sysinfo::System;

// A host can retain an open run after its shared provider releases the transcript.
// This is host lifecycle evidence, not proof that the model is generating output.
pub fn scan(state: &State, system: &System) -> Result<()> {
    let uid = unsafe { libc::geteuid() };
    let mut databases = HashSet::new();
    for (pid, process) in system.processes() {
        let name = process
            .exe()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str());
        if process.user_id().is_none_or(|u| **u != uid)
            || !matches!(
                name,
                Some("T3 Code" | "T3 Code (Nightly)" | "t3-code" | "t3")
            )
        {
            continue;
        }
        // Only an open database held by the running host counts, never a saved DB alone.
        for path in crate::discovery::open_files(pid.as_u32()) {
            if path.file_name().is_some_and(|n| n == "statev2.sqlite") {
                databases.insert(path);
            }
        }
    }
    reconcile(state, &databases.into_iter().collect::<Vec<_>>())
}

fn read_runs(path: &Path) -> Result<Vec<Session>> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0,
        "Unsafe T3 database ownership or permissions"
    );
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_millis(100))?;
    db.execute_batch("PRAGMA trusted_schema=OFF")?;
    let mut query = db.prepare(
        "SELECT json_extract(pt.payload_json, '$.nativeThreadRef.driver'),
                json_extract(pt.payload_json, '$.nativeThreadRef.nativeId'),
                COALESCE(NULLIF(json_extract(t.payload_json, '$.worktreePath'), ''), p.workspace_root, ''),
                CASE WHEN MAX(r.status = 'running') THEN 'unknown' ELSE 'waiting' END
         FROM orchestration_v2_projection_runs r
         JOIN orchestration_v2_projection_threads t ON t.thread_id = r.thread_id
         JOIN orchestration_v2_projection_provider_threads pt
           ON pt.provider_thread_id = r.provider_thread_id AND pt.thread_id = t.thread_id
         LEFT JOIN projection_projects p ON p.project_id = t.project_id
         WHERE r.status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
           AND r.completed_at IS NULL AND t.archived_at IS NULL AND t.deleted_at IS NULL
           AND t.active_provider_thread_id = pt.provider_thread_id
           AND r.ordinal >= COALESCE(pt.last_run_ordinal, 0)
           AND json_extract(pt.payload_json, '$.nativeThreadRef.strength') = 'strong'
         GROUP BY t.thread_id, pt.provider_thread_id LIMIT 1001",
    )?;
    let rows = query.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut sessions = vec![];
    for (index, row) in rows.enumerate() {
        ensure!(index < 1000, "Too many active T3 runs");
        let (driver, id, project, status) = row?;
        let provider = match driver.as_deref() {
            Some("codex") => "codex",
            Some("claudeAgent" | "claude") => "claude",
            _ => continue,
        };
        let Some(id) = id.filter(|id| valid_id(id)) else {
            continue;
        };
        if project.len() > 4096 || (!project.is_empty() && !Path::new(&project).is_absolute()) {
            continue;
        }
        // `hook` is the existing wire category for observation from a host lifecycle.
        let mut session = Session::new(provider, &id, &project, "hook");
        session.state = status;
        sessions.push(session);
    }
    Ok(sessions)
}

fn reconcile(state: &State, databases: &[PathBuf]) -> Result<()> {
    let previous: HashSet<String> =
        serde_json::from_str(&state.get("t3:sessions")?.unwrap_or_else(|| "[]".into()))?;
    let policy = state.policy()?;
    let mut current = HashSet::new();
    let mut failed = false;
    for path in databases {
        let Ok(runs) = read_runs(path) else {
            failed = true;
            continue;
        };
        for host in runs {
            let mut session = state.session(&host.id).unwrap_or_else(|_| host.clone());
            if session.source == "managed"
                || (session.live
                    && session.source != "hook"
                    && matches!(session.state.as_str(), "working" | "waiting" | "unknown"))
            {
                continue;
            }
            if !current.insert(session.id.clone()) {
                continue;
            }
            session.source = "hook".into();
            session.live = true;
            session.state = host.state;
            session.project = host.project;
            session.observed_at = now();
            // The host PID and app run ID are not a provider process or controllable turn.
            session.pid = None;
            session.process_start = None;
            session.turn_id = None;
            session.capabilities.clear();
            session.collection = if policy.content(&session.project) {
                "content"
            } else {
                "metadata"
            }
            .into();
            state.save_session(&session)?;
            if !previous.contains(&session.id) && !state.backlogged()? {
                state.event(json!({"kind":"status","session_id":session.id,
                    "text":"T3 reports an open run. Provider activity is unconfirmed; this session is observation-only."}))?;
            }
        }
    }
    for id in previous.difference(&current) {
        if let Ok(mut session) = state.session(id)
            && session.source == "hook"
        {
            session.live = false;
            session.state = "unknown".into();
            session.capabilities.clear();
            state.save_session(&session)?;
        }
    }
    state.set("t3:sessions", &serde_json::to_string(&current)?)?;
    // A changed/locked optional host schema must not hide unrelated terminal sessions.
    state.set("t3:scan_ok", if failed { "false" } else { "true" })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Session;
    use rusqlite::{Connection, params};
    use serde_json::json;

    fn database(path: &std::path::Path) -> Result<Connection> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "CREATE TABLE orchestration_v2_projection_threads (
                thread_id TEXT PRIMARY KEY, project_id TEXT, active_provider_thread_id TEXT,
                archived_at TEXT, deleted_at TEXT, payload_json TEXT);
             CREATE TABLE orchestration_v2_projection_runs (
                run_id TEXT PRIMARY KEY, thread_id TEXT, provider_thread_id TEXT,
                ordinal INTEGER, status TEXT, completed_at TEXT);
             CREATE TABLE orchestration_v2_projection_provider_threads (
                provider_thread_id TEXT PRIMARY KEY, thread_id TEXT, status TEXT,
                payload_json TEXT, last_run_ordinal INTEGER);
             CREATE TABLE projection_projects (project_id TEXT PRIMARY KEY, workspace_root TEXT);
             INSERT INTO projection_projects VALUES ('project', '/project');",
        )?;
        Ok(db)
    }

    fn run(db: &Connection, id: &str, driver: &str, status: &str) -> Result<()> {
        db.execute(
            "INSERT INTO orchestration_v2_projection_threads VALUES (?, 'project', ?, NULL, NULL, ?)",
            params![id, format!("host:{id}"), json!({"worktreePath":"/project/worktree"}).to_string()],
        )?;
        db.execute(
            "INSERT INTO orchestration_v2_projection_provider_threads VALUES (?, ?, 'active', ?, 1)",
            params![
                format!("host:{id}"),
                id,
                json!({
                    "nativeThreadRef":{"driver":driver,"nativeId":id,"strength":"strong"},
                    "providerSessionId":"provider-session:shared"
                })
                .to_string()
            ],
        )?;
        db.execute(
            "INSERT INTO orchestration_v2_projection_runs VALUES (?, ?, ?, 1, ?, NULL)",
            params![format!("run:{id}"), id, format!("host:{id}"), status],
        )?;
        Ok(())
    }

    #[test]
    fn open_t3_run_survives_closed_transcript_without_claiming_generation() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("statev2.sqlite");
        let db = database(&path)?;
        run(&db, "native-id", "codex", "running")?;
        let state = State::open(tmp.path().join("collector"))?;
        let mut old = Session::new("codex", "native-id", "/project/worktree", "transcript");
        old.state = "idle".into();
        old.observed_at = 1.0;
        old.usage.input_tokens = Some(123);
        state.save_session(&old)?;
        // Multiple host processes/profiles may expose the same native session.
        reconcile(&state, &[path.clone(), path.clone()])?;
        let session = state.session("codex:native-id")?;
        assert!(session.live);
        assert_eq!(session.state, "unknown");
        assert_eq!(session.source, "hook");
        assert!(session.observed_at > old.observed_at);
        assert_eq!(session.pid, None);
        assert_eq!(session.turn_id, None);
        assert!(session.capabilities.is_empty());
        assert_eq!(session.usage.input_tokens, Some(123));
        assert_eq!(state.sessions()?.len(), 1);
        let events = state.events(&session.id, 0)?;
        assert_eq!(events["events"].as_array().unwrap().len(), 1);
        assert_eq!(events["events"][0]["kind"], "status");
        assert!(events["events"][0]["text"].as_str().unwrap().contains("T3"));
        // Repeated polling must not manufacture conversation or duplicate status events.
        reconcile(&state, &[path])?;
        assert_eq!(state.events(&session.id, 0)?, events);
        Ok(())
    }

    #[test]
    fn t3_fallback_expires_when_host_closes_or_run_finishes() -> Result<()> {
        for cause in ["host", "completed", "archived", "deleted", "replaced"] {
            let tmp = tempfile::tempdir()?;
            let path = tmp.path().join("statev2.sqlite");
            let db = database(&path)?;
            run(&db, "native-id", "codex", "running")?;
            let state = State::open(tmp.path().join("collector"))?;
            reconcile(&state, std::slice::from_ref(&path))?;
            assert!(state.session("codex:native-id")?.live);
            match cause {
                "completed" => {
                    db.execute("UPDATE orchestration_v2_projection_runs SET status='completed', completed_at='2026-10-17'", [])?;
                }
                "archived" => {
                    db.execute(
                        "UPDATE orchestration_v2_projection_threads SET archived_at='2026-10-17'",
                        [],
                    )?;
                }
                "deleted" => {
                    db.execute(
                        "UPDATE orchestration_v2_projection_threads SET deleted_at='2026-10-17'",
                        [],
                    )?;
                }
                "replaced" => {
                    db.execute("UPDATE orchestration_v2_projection_threads SET active_provider_thread_id='new-provider'", [])?;
                }
                _ => {}
            }
            if cause == "host" {
                scan(&state, &System::new())?;
            } else {
                reconcile(&state, std::slice::from_ref(&path))?;
            }
            let session = state.session("codex:native-id")?;
            assert!(!session.live, "{cause}");
            assert!(session.capabilities.is_empty());
        }
        Ok(())
    }

    #[test]
    fn claude_native_id_and_queued_work_are_observed_without_control() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("statev2.sqlite");
        let db = database(&path)?;
        run(&db, "claude-native", "claudeAgent", "queued")?;
        db.execute(
            "UPDATE orchestration_v2_projection_threads SET payload_json='{}'",
            [],
        )?;
        let state = State::open(tmp.path().join("collector"))?;
        reconcile(&state, &[path])?;
        let session = state.session("claude:claude-native")?;
        assert!(session.live);
        assert_eq!(session.state, "waiting");
        assert_eq!(session.project, "/project");
        assert_eq!(session.collection, "metadata");
        assert!(session.capabilities.is_empty());
        assert_eq!(state.sessions()?.len(), 1);
        Ok(())
    }

    #[test]
    fn t3_does_not_replace_live_provider_evidence_or_use_weak_native_ids() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("statev2.sqlite");
        let db = database(&path)?;
        let state = State::open(tmp.path().join("collector"))?;
        for source in ["managed", "app-server", "process"] {
            run(&db, source, "codex", "running")?;
            let mut session = Session::new("codex", source, "/project", source);
            session.live = true;
            session.state = "working".into();
            session.pid = Some(42);
            state.save_session(&session)?;
        }
        for id in ["weak", "unsupported", "invalid/id", "terminal"] {
            run(&db, id, "codex", "running")?;
        }
        db.execute("UPDATE orchestration_v2_projection_provider_threads SET payload_json=json_set(payload_json,'$.nativeThreadRef.strength','weak') WHERE thread_id='weak'", [])?;
        db.execute("UPDATE orchestration_v2_projection_provider_threads SET payload_json=json_set(payload_json,'$.nativeThreadRef.driver','acpRegistry') WHERE thread_id='unsupported'", [])?;
        db.execute("UPDATE orchestration_v2_projection_runs SET status='completed' WHERE thread_id='terminal'", [])?;
        let before = state.sessions()?;
        reconcile(&state, &[path])?;
        assert_eq!(state.sessions()?, before);
        Ok(())
    }

    #[test]
    fn unsupported_host_schema_expires_fallback_without_affecting_live_terminals() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("statev2.sqlite");
        let db = database(&path)?;
        run(&db, "host", "codex", "running")?;
        let state = State::open(tmp.path().join("collector"))?;
        let mut terminal = Session::new("claude", "terminal", "/project", "process");
        terminal.live = true;
        terminal.state = "working".into();
        state.save_session(&terminal)?;
        // Compare persisted state; JSON can round a fractional timestamp on storage.
        let terminal = state.session(&terminal.id)?;
        reconcile(&state, std::slice::from_ref(&path))?;
        db.execute("DROP TABLE orchestration_v2_projection_runs", [])?;
        reconcile(&state, &[path])?;
        assert!(!state.session("codex:host")?.live);
        assert_eq!(state.session("claude:terminal")?, terminal);
        assert_eq!(state.get("t3:scan_ok")?.as_deref(), Some("false"));
        Ok(())
    }

    #[test]
    fn old_queued_records_cannot_revive_a_provider_that_completed_a_later_run() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("statev2.sqlite");
        let db = database(&path)?;
        run(&db, "finished", "codex", "queued")?;
        db.execute("INSERT INTO orchestration_v2_projection_runs VALUES ('later', 'finished', 'host:finished', 2, 'completed', '2026-10-17')", [])?;
        db.execute("UPDATE orchestration_v2_projection_provider_threads SET status='idle', last_run_ordinal=2", [])?;
        let state = State::open(tmp.path().join("collector"))?;
        reconcile(&state, std::slice::from_ref(&path))?;
        assert!(state.sessions()?.is_empty());
        // Newly queued work after that completed run is still active, including while idle.
        db.execute("INSERT INTO orchestration_v2_projection_runs VALUES ('next', 'finished', 'host:finished', 3, 'queued', NULL)", [])?;
        reconcile(&state, &[path])?;
        let session = state.session("codex:finished")?;
        assert!(session.live);
        assert_eq!(session.state, "waiting");
        Ok(())
    }
}
