use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, DirBuilder, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn private_dir(path: &Path) -> Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        bail!("Agent directory must be owned by you, not a symlink, and mode 0700");
    }
    Ok(())
}

pub fn agent_root(root: &Path, server: &str) -> PathBuf {
    root.join(&format!("agent-{:x}", Sha256::digest(server.as_bytes()))[..22])
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: String,
    pub provider: String,
    pub provider_session_id: String,
    pub project: String,
    pub state: String,
    pub live: bool,
    pub pid: Option<u32>,
    pub process_start: Option<u64>,
    pub parent_id: Option<String>,
    pub turn_id: Option<String>,
    pub source: String,
    pub observed_at: f64,
    pub capabilities: Vec<String>,
    pub usage: Usage,
    pub collection: String,
}

impl Session {
    pub fn new(provider: &str, id: &str, project: &str, source: &str) -> Self {
        Self {
            id: format!("{provider}:{id}"),
            provider: provider.into(),
            provider_session_id: id.into(),
            project: project.into(),
            state: "unknown".into(),
            live: false,
            pid: None,
            process_start: None,
            parent_id: None,
            turn_id: None,
            source: source.into(),
            observed_at: now(),
            capabilities: vec![],
            usage: Usage::default(),
            collection: "metadata".into(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Policy {
    /// Canonical directories whose visible conversation content may leave this machine.
    pub content_roots: Vec<PathBuf>,
    /// Remote prompts are disabled until explicitly enabled on the machine.
    pub remote_control: bool,
    /// The service installs verified releases unless updates were made manual.
    #[serde(default)]
    pub manual_updates: bool,
}

impl Policy {
    pub fn content(&self, project: &str) -> bool {
        let project = Path::new(project);
        for ancestor in project.ancestors() {
            match fs::canonicalize(ancestor) {
                Ok(base) => {
                    let path = base.join(project.strip_prefix(ancestor).unwrap());
                    return self.content_roots.iter().any(|root| path.starts_with(root));
                }
                Err(error) => {
                    // Deleted worktrees retain their scope; unresolved symlinks and traversal do not.
                    if error.kind() != std::io::ErrorKind::NotFound
                        || !project.is_absolute()
                        || project
                            .components()
                            .any(|part| part == std::path::Component::ParentDir)
                        || fs::symlink_metadata(ancestor).is_ok_and(|m| m.file_type().is_symlink())
                    {
                        return false;
                    }
                }
            }
        }
        false
    }
}

pub struct State {
    pub db: Connection,
    pub root: PathBuf,
}

impl State {
    pub fn open(root: PathBuf) -> Result<Self> {
        private_dir(&root)?;
        let path = root.join("state.sqlite");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
            bail!("Unsafe state database permissions");
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA journal_size_limit=33554432; PRAGMA secure_delete=ON;
            CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS outbox(seq INTEGER PRIMARY KEY,data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS cursors(path TEXT PRIMARY KEY,inode TEXT NOT NULL,offset INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS usage_items(session TEXT,item TEXT,data TEXT,PRIMARY KEY(session,item));
            CREATE TABLE IF NOT EXISTS provider_items(session TEXT,item TEXT,length INTEGER,hash TEXT,seen REAL,PRIMARY KEY(session,item));
            CREATE TABLE IF NOT EXISTS commands(id TEXT PRIMARY KEY,data TEXT NOT NULL,status TEXT NOT NULL,code TEXT NOT NULL);")?;
        let state = Self { db, root };
        if state.get("stream_id")?.is_none() {
            state.set("stream_id", &uuid::Uuid::new_v4().to_string())?;
        }
        Ok(state)
    }
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM settings WHERE key=?", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO settings VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [key, value],
        )?;
        Ok(())
    }
    pub fn policy(&self) -> Result<Policy> {
        Ok(
            serde_json::from_str(&self.get("policy")?.unwrap_or_else(|| "{}".into()))
                .unwrap_or_default(),
        )
    }
    pub fn sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.db.prepare("SELECT data FROM sessions ORDER BY id")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn session(&self, id: &str) -> Result<Session> {
        let data: String = self
            .db
            .query_row("SELECT data FROM sessions WHERE id=?", [id], |r| r.get(0))
            .context("Session not found")?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn save_session(&self, session: &Session) -> Result<()> {
        self.db.execute(
            "INSERT INTO sessions VALUES (?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![session.id, serde_json::to_string(session)?],
        )?;
        Ok(())
    }
    pub fn event(&self, mut value: Value) -> Result<i64> {
        let transaction = if self.db.is_autocommit() {
            Some(self.db.unchecked_transaction()?)
        } else {
            None
        };
        value["at"] = json!(now());
        // Bound each wire event without losing the remainder of visible output.
        let text = value["text"].as_str().map(str::to_owned);
        let mut remaining = text.as_deref().unwrap_or("").chars().peekable();
        let seq = loop {
            if text.is_some() {
                value["text"] = json!(remaining.by_ref().take(16000).collect::<String>());
            }
            self.db
                .execute("INSERT INTO events(data) VALUES ('{}')", [])?;
            let seq = self.db.last_insert_rowid();
            value["seq"] = json!(seq);
            let data = serde_json::to_string(&value)?;
            let mut local = value.clone();
            if let Some(object) = local.as_object_mut() {
                object.remove("sessions");
                object.remove("session");
            }
            self.db.execute(
                "UPDATE events SET data=? WHERE seq=?",
                params![serde_json::to_string(&local)?, seq],
            )?;
            self.db
                .execute("INSERT INTO outbox VALUES (?,?)", params![seq, data])?;
            if remaining.peek().is_none() {
                break seq;
            }
            value["append"] = json!(true);
        };
        if let Some(tx) = transaction {
            tx.commit()?;
        }
        Ok(seq)
    }
    pub fn batch(&self) -> Result<Vec<Value>> {
        let mut stmt = self
            .db
            .prepare("SELECT data FROM outbox ORDER BY seq LIMIT 100")?;
        let mut size = 0;
        let mut items = vec![];
        for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
            let data = row?;
            if size + data.len() > 900000 {
                break;
            }
            size += data.len();
            items.push(serde_json::from_str(&data)?);
        }
        Ok(items)
    }
    pub fn backlogged(&self) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT count(*)>=200 FROM (SELECT 1 FROM outbox LIMIT 200)",
            [],
            |r| r.get(0),
        )?)
    }
    pub fn ack(&self, seq: i64) -> Result<()> {
        self.db.execute("DELETE FROM outbox WHERE seq<=?", [seq])?;
        Ok(())
    }
    pub fn events(&self, id: &str, after: i64) -> Result<Value> {
        let mut stmt = self.db.prepare("SELECT data FROM events WHERE seq>? AND json_extract(data,'$.session_id')=? ORDER BY seq LIMIT 200")?;
        let values: Vec<Value> = stmt
            .query_map(params![after, id], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?;
        Ok(
            json!({"cursor":values.last().and_then(|v|v["seq"].as_i64()).unwrap_or(after),"events":values}),
        )
    }
    pub fn prune(&self) -> Result<()> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let max: i64 = self
            .db
            .query_row("SELECT coalesce(max(seq),0) FROM events", [], |r| r.get(0))?;
        self.db
            .execute("DELETE FROM events WHERE seq<?", [max - 10000])?;
        self.db.execute("DELETE FROM events WHERE seq < coalesce((SELECT min(seq) FROM (SELECT seq,sum(length(data)) OVER (ORDER BY seq DESC) AS bytes FROM events) WHERE bytes<=33554432),?)",[max])?;
        transaction.commit()?;
        Ok(())
    }
    pub fn scrub_content(&self) -> Result<()> {
        let policy = self.policy()?;
        for mut session in self.sessions()? {
            if !policy.content(&session.project) {
                self.db.execute(
                    "DELETE FROM settings WHERE key=?",
                    [format!("transcript-content:{}", session.id)],
                )?;
                session.collection = "metadata".into();
                self.save_session(&session)?;
                for table in ["events", "outbox"] {
                    self.db.execute(&format!("UPDATE {table} SET data=json_remove(data,'$.text') WHERE json_extract(data,'$.session_id')=?"), [&session.id])?;
                }
            }
        }
        // Old inventory policy must never precede newly scrubbed events on replay.
        for table in ["events", "outbox"] {
            let rows = {
                let mut stmt = self.db.prepare(&format!(
                    "SELECT seq,data FROM {table} WHERE json_extract(data,'$.kind')='inventory'"
                ))?;
                stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            for (seq, data) in rows {
                let mut value: Value = serde_json::from_str(&data)?;
                if let Some(sessions) = value["sessions"].as_array_mut() {
                    for s in sessions {
                        if !policy.content(s["project"].as_str().unwrap_or("")) {
                            s["collection"] = json!("metadata");
                        }
                    }
                }
                self.db.execute(
                    &format!("UPDATE {table} SET data=? WHERE seq=?"),
                    params![serde_json::to_string(&value)?, seq],
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deleted_workspaces_remain_scoped_without_following_escaping_symlinks() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let allowed = fs::canonicalize(tmp.path())?.join("allowed");
        let outside = fs::canonicalize(tmp.path())?.join("outside");
        fs::create_dir(&allowed)?;
        fs::create_dir(&outside)?;
        let policy = Policy {
            content_roots: vec![allowed.clone()],
            ..Policy::default()
        };
        assert!(policy.content(allowed.join("removed/project").to_str().unwrap()));
        assert!(!policy.content(outside.join("removed/project").to_str().unwrap()));
        assert!(!policy.content(allowed.join("missing/../../outside").to_str().unwrap()));
        std::os::unix::fs::symlink(&outside, allowed.join("escape"))?;
        assert!(!policy.content(allowed.join("escape/missing").to_str().unwrap()));
        std::os::unix::fs::symlink(outside.join("missing"), allowed.join("dangling"))?;
        assert!(!policy.content(allowed.join("dangling/project").to_str().unwrap()));
        Ok(())
    }

    #[test]
    fn pruning_local_history_never_discards_unacknowledged_events() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = State::open(tmp.path().join("state"))?;
        let tx = state.db.unchecked_transaction()?;
        for _ in 0..10001 {
            state.event(json!({"kind":"status"}))?;
        }
        tx.commit()?;
        state.prune()?;
        let batch = state.batch()?;
        assert_eq!(batch.len(), 100);
        assert_eq!(batch[0]["kind"], "status");
        assert_eq!(batch[0]["seq"], 1);
        assert_eq!(
            state
                .db
                .query_row("SELECT count(*) FROM outbox", [], |r| r.get::<_, i64>(0))?,
            10001
        );
        state.event(json!({"kind":"status"}))?;
        state.ack(10001)?;
        assert_eq!(state.batch()?[0]["seq"], 10002);
        Ok(())
    }
    #[test]
    fn outbox_survives_restart_and_content_revocation() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = State::open(tmp.path().join("state"))?;
        let session = Session::new("codex", "test", tmp.path().to_str().unwrap(), "transcript");
        state.save_session(&session)?;
        let seq = state
            .event(json!({"kind":"prompt","session_id":session.id,"text":"private canary"}))?;
        let stream = state.get("stream_id")?;
        drop(state);
        let state = State::open(tmp.path().join("state"))?;
        assert_eq!(state.get("stream_id")?, stream);
        assert_eq!(state.batch()?[0]["seq"], seq);
        state.scrub_content()?;
        assert!(state.batch()?[0].get("text").is_none());
        state.ack(seq)?;
        assert!(state.batch()?.is_empty());
        Ok(())
    }
}
