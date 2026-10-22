use crate::{
    credentials::Store,
    discovery,
    providers::Providers,
    state::{State, now, private_dir},
};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use reqwest::{Url, blocking::Client};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use sha2::Digest;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::{
    fs::{OpenOptionsExt, PermissionsExt},
    net::{UnixListener, UnixStream},
};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub fn request(root: &Path, value: &Value) -> Result<Value> {
    let mut stream = UnixStream::connect(root.join("agent.sock"))
        .context("Service is not running. Run roundtable daemon or roundtable service start")?;
    stream.set_read_timeout(Some(Duration::from_secs(if value["op"] == "start" {
        45
    } else {
        15
    })))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    serde_json::to_writer(&mut stream, value)?;
    stream.write_all(b"\n")?;
    let mut response = String::new();
    BufReader::new(stream)
        .take(2_000_000)
        .read_line(&mut response)?;
    let result: Value = serde_json::from_str(&response)?;
    if let Some(error) = result["error"].as_str() {
        bail!("{}", crate::safe(error));
    }
    Ok(result)
}

/// Resend an unchanged inventory at least this often so the server can trust a quiet machine.
const INVENTORY_RESEND: f64 = 900.;
/// Sync body fields beyond the original envelope. Each is sent only after the server
/// advertised it in sync_extensions, so an older server keeps accepting the legacy body.
const SYNC_EXTENSIONS: [&str; 2] = ["scanned_at", "fetch_results"];
fn negotiated(state: &State) -> Result<Vec<String>> {
    Ok(
        serde_json::from_str(&state.get("sync_extensions")?.unwrap_or_default())
            .unwrap_or_default(),
    )
}

fn inventory(state: &State) -> Result<()> {
    let sessions: Vec<_> = state
        .sessions()?
        .into_iter()
        .filter(|s| s.live || now() - s.observed_at < 86400.)
        .collect();
    let event = json!({"kind":"inventory","sessions":sessions});
    let encoded = serde_json::to_vec(&event)?;
    // An oversized inventory is a scan failure every time, never a remembered one.
    if sessions.len() > 1000 || encoded.len() > 700000 {
        state.set("scan_ok", "false")?;
        state.event(json!({"kind":"gap","text":"Inventory exceeds the supported batch size; current counts unavailable"}))?;
        return Ok(());
    }
    // Keep one heartbeat behind backfill without accumulating snapshots while offline.
    if state.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM outbox WHERE json_extract(data,'$.kind')='inventory')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    // Every inventory makes the server reconcile all of this machine's sessions, so an
    // unchanged one is only repeated as a periodic heartbeat. Liveness travels as the
    // machine-level scanned_at, so a live session's observed_at alone changes nothing.
    let mut fingerprint = event.clone();
    for session in fingerprint["sessions"].as_array_mut().into_iter().flatten() {
        session["observed_at"] = json!(0);
    }
    let digest = format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_vec(&fingerprint)?)
    );
    let sent_at = state
        .get("inventory_sent_at")?
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.);
    // A server without the scanned_at heartbeat still judges liveness from each session's
    // observed_at, so it needs the inventory every scan.
    if negotiated(state)?.iter().any(|e| e == "scanned_at")
        && state.get("inventory_digest")?.as_deref() == Some(&digest)
        && now() - sent_at < INVENTORY_RESEND
    {
        return Ok(());
    }
    state.event(event)?;
    state.set("inventory_digest", &digest)?;
    state.set("inventory_sent_at", &now().to_string())?;
    Ok(())
}

/// Answer server requests for full tool output. Each is checked against the local session
/// and content policy; the result travels in the next sync.
fn answer_fetches(state: &State, fetches: &[Value]) -> Result<()> {
    for fetch in fetches {
        let Some(id) = fetch["id"].as_str().filter(|s| discovery::valid_id(s)) else {
            continue;
        };
        if state.has_fetch_result(id)? {
            continue;
        }
        let session_id = fetch["session_id"].as_str().unwrap_or("");
        let result = match state.tool_output(session_id, fetch["seq"].as_i64().unwrap_or(0)) {
            Ok(result) => result,
            Err(error) => {
                json!({"error":crate::safe(&error.to_string()).chars().take(200).collect::<String>()})
            }
        };
        state.save_fetch_result(id, session_id, &result)?;
    }
    Ok(())
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("roundtable-cli/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn sync_once(state: &State, config: &Path, server: &Url, client: &Client) -> Result<()> {
    let store = Store::open(config.to_owned(), server.as_str())?;
    let Some((credential, token)) = store.load()? else {
        state.set("connection", "not logged in")?;
        return Ok(());
    };
    drop(store);
    if state.get("device_id")?.as_deref() != Some(&credential.device_id) {
        let tx = state.db.unchecked_transaction()?;
        if state.get("device_id")?.is_some() {
            // A new login resets sharing, but keeps the machine's update preference.
            let policy = crate::state::Policy {
                manual_updates: state.policy()?.manual_updates,
                ..Default::default()
            };
            state.set("policy", &serde_json::to_string(&policy)?)?;
            state.scrub_content()?;
            let max: i64 =
                state
                    .db
                    .query_row("SELECT coalesce(max(seq),0) FROM events", [], |r| r.get(0))?;
            state.db.execute("DELETE FROM outbox", [])?;
            state.db.execute("DELETE FROM events", [])?;
            state.db.execute("DELETE FROM commands", [])?;
            state.db.execute("DELETE FROM fetch_results", [])?;
            state.set("sync_extensions", "[]")?;
            state.set("stream_id", &uuid::Uuid::new_v4().to_string())?;
            if max > 0 {
                state.event(json!({"kind":"gap","dropped_from":1,"dropped_to":max}))?;
            }
        }
        state.set("device_id", &credential.device_id)?;
        // A new stream always starts with an inventory, even an unchanged one.
        state.set("inventory_digest", "")?;
        inventory(state)?;
        tx.commit()?;
    }
    let batch = state.batch()?;
    let extensions = negotiated(state)?;
    let uses = |name: &str| extensions.iter().any(|e| e == name);
    // Fetch answers share the batch's 900 KB budget under the server's 1 MB body limit.
    let results = if uses("fetch_results") {
        state.fetch_results(900_000usize.saturating_sub(serde_json::to_vec(&batch)?.len()))?
    } else {
        vec![]
    };
    let answered: Vec<String> = results
        .iter()
        .map(|r| r["id"].as_str().unwrap_or("").to_owned())
        .collect();
    let mut body = json!({"stream_id":state.get("stream_id")?.unwrap(),"version":env!("CARGO_PKG_VERSION"),
        "control_enabled":state.policy()?.remote_control,
        "process_count":state.get("process_count")?.and_then(|s|s.parse::<u64>().ok()).unwrap_or(0),
        "scan_ok":state.get("scan_ok")?.as_deref()==Some("true"),"events":batch});
    if !extensions.is_empty() {
        body["extensions"] = json!(extensions);
    }
    if uses("scanned_at") {
        body["scanned_at"] = json!(state.get("scanned_at")?.and_then(|s| s.parse::<f64>().ok()));
    }
    if !results.is_empty() {
        body["fetch_results"] = json!(results);
    }
    let response = client
        .post(server.join("/auth/device/sync")?)
        .bearer_auth(&token)
        .json(&body)
        .send()?;
    let status = response.status();
    if !status.is_success() {
        // A server that no longer accepts the negotiated fields gets the legacy body next.
        if status.is_client_error() && !extensions.is_empty() {
            state.set("sync_extensions", "[]")?;
        }
        state.set("connection", &format!("server HTTP {}", status.as_u16()))?;
        bail!("Server HTTP {}", status.as_u16());
    }
    let data: Value = response.json()?;
    let ack = data["ack"]
        .as_i64()
        .context("Invalid telemetry acknowledgement")?;
    let sent_max = batch.last().and_then(|v| v["seq"].as_i64()).unwrap_or(ack);
    if ack > sent_max {
        bail!("Server acknowledgement exceeds transmitted events");
    }
    state.ack(ack)?;
    state.clear_fetch_results(&answered)?;
    state.set("last_ack", &ack.to_string())?;
    state.set("connection", "online")?;
    state.set("last_sync", &now().to_string())?;
    let offered = data["sync_extensions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let agreed: Vec<&str> = SYNC_EXTENSIONS
        .into_iter()
        .filter(|e| offered.iter().any(|o| o == e))
        .collect();
    state.set("sync_extensions", &json!(agreed).to_string())?;
    if agreed.contains(&"fetch_results") {
        answer_fetches(state, data["fetches"].as_array().map_or(&[], Vec::as_slice))?;
    }
    if data["cli_release"].is_object() {
        state.set("cli_release", &data["cli_release"].to_string())?;
    }
    for command in data["uncertain"].as_array().into_iter().flatten() {
        if let Some(id) = command["id"].as_str().filter(|s| discovery::valid_id(s)) {
            state.db.execute(
                "INSERT OR IGNORE INTO commands VALUES (?,?,'uncertain','crash')",
                params![id, serde_json::to_string(command)?],
            )?;
        }
    }
    // Retry receipts, never execution. An interrupted executor becomes uncertain at restart.
    let receipts = {
        let mut stmt=state.db.prepare("SELECT id,status,code FROM commands WHERE status IN ('completed','failed','uncertain')")?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, status, code) in receipts {
        let response = client
            .post(server.join(&format!("/auth/device/commands/{id}/receipt"))?)
            .bearer_auth(&token)
            .json(&json!({"status":status,"code":code}))
            .send()?;
        if response.status().is_success() {
            state
                .db
                .execute("UPDATE commands SET status='reported' WHERE id=?", [id])?;
        }
    }
    if state.policy()?.remote_control {
        for command in data["commands"].as_array().into_iter().flatten() {
            let Some(id) = command["id"].as_str().filter(|s| discovery::valid_id(s)) else {
                continue;
            };
            if state
                .db
                .query_row("SELECT 1 FROM commands WHERE id=?", [id], |r| {
                    r.get::<_, i32>(0)
                })
                .optional()?
                .is_some()
            {
                continue;
            }
            let Ok(session) = state.session(command["session_id"].as_str().unwrap_or("")) else {
                continue;
            };
            if !session.live
                || (command["action"] == "prompt"
                    && command["timing"] != "steer"
                    && session.state != "idle")
            {
                continue;
            }
            let response = client
                .post(server.join(&format!("/auth/device/commands/{id}/claim"))?)
                .bearer_auth(&token)
                .json(&json!({}))
                .send()?;
            if !response.status().is_success() {
                continue;
            }
            let mut command: Value = response.json()?;
            // A claim is fresh authorization, not an offline command cache. Limit the local hop.
            command["authorized_until"] = json!(now() + 5.);
            state.db.execute(
                "INSERT OR IGNORE INTO commands VALUES (?,?,'executing','crash')",
                params![id, serde_json::to_string(&command)?],
            )?;
            // A failed IPC response has an uncertain outcome; never re-send the prompt.
            if request(&state.root, &json!({"op":"remote","command":command})).is_err() {
                state.db.execute("UPDATE commands SET status='uncertain',code='crash' WHERE id=? AND status='executing'",[id])?;
            }
        }
    }
    Ok(())
}

fn local_control(state: &State, providers: &mut Providers, request: &Value) -> Result<Value> {
    let mut command = request.clone();
    let id = command["session_id"]
        .as_str()
        .context("Session ID is required")?;
    let session = state.session(id)?;
    let expected = command["expected_turn_id"]
        .as_str()
        .map(str::to_owned)
        .or(session.turn_id);
    command["expected_turn_id"] = json!(expected);
    providers.control(state, &command)?;
    Ok(json!({"status":"delivered"}))
}

/// Stage the server-announced release, retrying a failed one at most every six hours.
fn prepare_update(state: &State, client: &Client, exe: &Path) -> Result<Option<PathBuf>> {
    if state.policy()?.manual_updates {
        return Ok(None);
    }
    let release =
        serde_json::from_str(&state.get("cli_release")?.unwrap_or_default()).unwrap_or(Value::Null);
    let Some((next, digest)) = crate::update::offered(&release, env!("CARGO_PKG_VERSION")) else {
        return Ok(None);
    };
    let attempt = format!("{next}:{digest}");
    let last = state
        .get("update_attempt_at")?
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.);
    if state.get("update_attempt")?.as_deref() == Some(&attempt) && now() - last < 21600. {
        return Ok(None);
    }
    state.set("update_attempt", &attempt)?;
    state.set("update_attempt_at", &now().to_string())?;
    match crate::update::stage(client, crate::update::RELEASES, &next, &digest, exe) {
        Ok(path) => {
            eprintln!("Roundtable {next} verified; installing once no managed sessions run.");
            Ok(Some(path))
        }
        Err(error) => {
            eprintln!("Roundtable {next} update failed: {error}");
            Ok(None)
        }
    }
}

fn handle(state: &State, providers: &mut Providers, value: Value) -> Result<Value> {
    match value["op"].as_str().unwrap_or("") {
        "status" => Ok(
            json!({"running":true,"pid":std::process::id(),"connection":state.get("connection")?,"last_sync":state.get("last_sync")?,"policy":state.policy()?}),
        ),
        "list" => Ok(json!({"sessions":state.sessions()?})),
        "events" => state.events(
            value["session_id"].as_str().unwrap_or(""),
            value["after"].as_i64().unwrap_or(0),
        ),
        "start" => {
            let provider = value["provider"].as_str().unwrap_or("codex");
            let cwd = Path::new(value["cwd"].as_str().context("Project is required")?);
            let session = providers.start(
                state,
                provider,
                cwd,
                value["workspace_write"].as_bool().unwrap_or(false),
            )?;
            inventory(state)?;
            Ok(json!(session))
        }
        "prompt" | "interrupt" => local_control(state, providers, &value),
        "remote" => {
            let command = &value["command"];
            let id = command["id"].as_str().context("Command ID is required")?;
            let record: Option<(String, String)> = state
                .db
                .query_row("SELECT data,status FROM commands WHERE id=?", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            let Some((saved, status)) = record else {
                bail!("Command is not authorized")
            };
            if status != "executing" || serde_json::from_str::<Value>(&saved)? != *command {
                bail!("Command was already processed");
            }
            let session_id = command["session_id"]
                .as_str()
                .context("Session is required")?;
            let epoch = command["epoch"]
                .as_u64()
                .context("Lease epoch is required")?;
            let key = format!("epoch:{session_id}");
            let last = state
                .get(&key)?
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let (status, code) = if !state.policy()?.remote_control {
                ("failed", "local_policy")
            } else if now() > command["expires_at"].as_f64().unwrap_or(0.)
                || now() > command["authorized_until"].as_f64().unwrap_or(0.)
                || epoch < last
            {
                ("failed", "expired")
            } else {
                state.set(&key, &epoch.to_string())?;
                match providers.control(state, command) {
                    Ok(()) => ("completed", "delivered"),
                    Err(e) => match e.to_string().as_str() {
                        "unsupported" => ("failed", "unsupported"),
                        "stale_turn" => ("failed", "stale_turn"),
                        _ => ("uncertain", "provider_error"),
                    },
                }
            };
            state.db.execute(
                "UPDATE commands SET status=?,code=? WHERE id=?",
                params![status, code, id],
            )?;
            Ok(json!({"status":status,"code":code}))
        }
        _ => bail!("Unknown local operation"),
    }
}

pub fn run(root: PathBuf, config: PathBuf, server: Url) -> Result<()> {
    private_dir(&root)?;
    let exe = fs::canonicalize(std::env::current_exe()?)?;
    crate::update::startup(&exe)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("Roundtable service is already running")?;
    let socket = root.join("agent.sock");
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)
        .context("Cannot create local socket (try a shorter --config-dir)")?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let state = State::open(root.clone())?;
    // Every run renegotiates sync extensions; the server may have changed meanwhile.
    state.set("sync_extensions", "[]")?;
    state.db.execute(
        "UPDATE commands SET status='uncertain',code='crash' WHERE status='executing'",
        [],
    )?;
    for mut session in state.sessions()? {
        session.live = false;
        session.state = "unknown".into();
        session.capabilities.clear();
        if session.source == "managed" {
            session.source = "transcript".into();
        }
        state.save_session(&session)?;
    }
    state.scrub_content()?;
    let running = Arc::new(AtomicBool::new(true));
    let stop = running.clone();
    ctrlc::set_handler(move || stop.store(false, Ordering::SeqCst))?;
    let sync_running = running.clone();
    let sync_root = root.clone();
    let staged: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let sync_staged = staged.clone();
    let sync_exe = exe.clone();
    let worker = std::thread::spawn(move || -> Result<()> {
        let state = State::open(sync_root)?;
        let client = client()?;
        let downloads = crate::update::client()?;
        let mut failures = 0;
        while sync_running.load(Ordering::SeqCst) {
            if let Err(error) = sync_once(&state, &config, &server, &client) {
                failures = (failures + 1).min(5);
                state.set("connection", &format!("offline; retrying: {error}"))?;
            } else {
                failures = 0;
                crate::update::confirmed(&sync_exe);
                if sync_staged.lock().unwrap().is_none()
                    && let Ok(Some(path)) = prepare_update(&state, &downloads, &sync_exe)
                {
                    *sync_staged.lock().unwrap() = Some(path);
                }
            }
            // Idle machines sync every five seconds; pending events or fetch answers go at once.
            let delay = if failures == 0 {
                if state.batch()?.is_empty() && !state.fetch_pending()? {
                    50
                } else {
                    1
                }
            } else {
                (20u64 << failures).min(300) + (std::process::id() as u64 % 17)
            };
            for _ in 0..delay {
                if !sync_running.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    });
    let mut providers = Providers::new();
    let mut system = sysinfo::System::new();
    let mut scanned = Instant::now() - Duration::from_secs(10);
    let mut updating = false;
    eprintln!(
        "Roundtable service running. Local socket: {}",
        socket.display()
    );
    while running.load(Ordering::SeqCst) {
        providers.drain(&state)?;
        if scanned.elapsed() >= Duration::from_secs(5) {
            let (count, ok) = discovery::scan(&state, &mut system)?;
            providers.inventory(&state)?;
            crate::t3::scan(&state, &system)?;
            state.set("process_count", &count.to_string())?;
            state.set("scan_ok", if ok { "true" } else { "false" })?;
            // The server treats sessions as current only while this heartbeat is fresh.
            state.set("scanned_at", &now().to_string())?;
            // A failed scan never marks missing sessions as exited remotely.
            inventory(&state)?;
            state.prune()?;
            scanned = Instant::now();
            // Restarting ends managed provider processes, so updates wait until none run.
            if staged.lock().unwrap().is_some()
                && !state.policy()?.manual_updates
                && !state
                    .sessions()?
                    .iter()
                    .any(|s| s.source == "managed" && s.live)
            {
                updating = true;
                break;
            }
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let result = read_request(&mut stream)
                    .and_then(|value| handle(&state, &mut providers, value));
                let value = match result {
                    Ok(v) => v,
                    Err(e) => json!({"error":e.to_string()}),
                };
                let _ = serde_json::to_writer(&mut stream, &value);
                let _ = stream.write_all(b"\n");
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) => return Err(e.into()),
        }
    }
    running.store(false, Ordering::SeqCst);
    drop(providers);
    let _ = worker.join();
    let _ = fs::remove_file(socket);
    if updating && let Some(path) = staged.lock().unwrap().take() {
        drop(listener);
        drop(lock);
        eprintln!("Restarting into the verified Roundtable release.");
        crate::update::apply(&path, &exe)?;
    }
    Ok(())
}

fn read_request(stream: &mut UnixStream) -> Result<Value> {
    // BSD accept inherits O_NONBLOCK from the listener. Requests can arrive in fragments.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?)
        .take(100000)
        .read_line(&mut line)?;
    if line.len() >= 100000 {
        bail!("Local request is too large");
    }
    Ok(serde_json::from_str(&line)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Session;
    #[test]
    fn backfill_does_not_starve_inventory_or_accumulate_offline_snapshots() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = State::open(tmp.path().join("state"))?;
        for _ in 0..200 {
            state.event(json!({"kind":"status"}))?;
        }
        assert!(state.backlogged()?);
        inventory(&state)?;
        inventory(&state)?;
        assert_eq!(
            state.db.query_row(
                "SELECT count(*) FROM outbox WHERE json_extract(data,'$.kind')='inventory'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            1
        );
        state.ack(201)?;
        // Without the scanned_at heartbeat the server needs every inventory for liveness.
        inventory(&state)?;
        assert_eq!(state.batch()?[0]["kind"], "inventory");
        state.ack(202)?;
        state.set("sync_extensions", r#"["scanned_at","fetch_results"]"#)?;
        // Once negotiated, an unchanged inventory waits for the periodic heartbeat.
        inventory(&state)?;
        assert!(state.batch()?.is_empty());
        state.set(
            "inventory_sent_at",
            &(now() - INVENTORY_RESEND - 1.).to_string(),
        )?;
        inventory(&state)?;
        assert_eq!(state.batch()?[0]["kind"], "inventory");
        state.ack(203)?;
        inventory(&state)?;
        assert!(state.batch()?.is_empty());
        let mut session = Session::new("codex", "fresh", "/work", "transcript");
        state.save_session(&session)?;
        inventory(&state)?;
        assert_eq!(state.batch()?[0]["sessions"][0]["id"], "codex:fresh");
        state.ack(204)?;
        session.live = true;
        state.save_session(&session)?;
        inventory(&state)?;
        assert_eq!(state.batch()?[0]["sessions"][0]["live"], true);
        state.ack(205)?;
        // A live session is re-observed every scan; that alone is not a change.
        session.observed_at = now() + 60.;
        state.save_session(&session)?;
        inventory(&state)?;
        assert!(state.batch()?.is_empty());
        Ok(())
    }

    fn content_state(tmp: &Path) -> Result<(State, Session)> {
        let state = State::open(tmp.join("state"))?;
        let project = fs::canonicalize(tmp)?;
        let policy = crate::state::Policy {
            content_roots: vec![project.clone()],
            ..Default::default()
        };
        state.set("policy", &serde_json::to_string(&policy)?)?;
        let mut session = Session::new("codex", "one", project.to_str().unwrap(), "transcript");
        session.collection = "content".into();
        state.save_session(&session)?;
        Ok((state, session))
    }

    #[test]
    fn fetched_tool_output_joins_chunks_and_refuses_unshared_or_pruned_events() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let (state, session) = content_state(tmp.path())?;
        let long = format!("head\n{}", "x".repeat(40000));
        state.event(json!({"kind":"prompt","session_id":session.id,"text":"ask"}))?;
        let seq = state.event(json!({"kind":"tool","session_id":session.id,"text":long}))?;
        let first = seq - 2;
        state.event(json!({"kind":"status","session_id":session.id}))?;
        let output = state.tool_output(&session.id, first)?;
        assert_eq!(output["text"], long);
        assert_eq!(output["truncated"], false);
        let huge = "y".repeat(70000);
        let seq = state.event(json!({"kind":"tool","session_id":session.id,"text":huge}))?;
        let output = state.tool_output(&session.id, seq - 4)?;
        assert_eq!(output["text"].as_str().unwrap().len(), 65536);
        assert_eq!(output["truncated"], true);
        // Another session's event or a pruned seq is reported, never guessed.
        assert!(state.tool_output("codex:other", first).is_err());
        state
            .db
            .execute("DELETE FROM events WHERE seq=?", [first])?;
        let error = state.tool_output(&session.id, first).unwrap_err();
        assert!(error.to_string().contains("no longer available"), "{error}");
        Ok(())
    }

    #[test]
    fn server_fetches_are_answered_in_the_following_sync() -> Result<()> {
        use std::net::TcpListener;
        let tmp = tempfile::tempdir_in("/tmp")?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        let config = tmp.path().join("config");
        let store = Store::open(config.clone(), url.as_str())?;
        store.save(
            crate::credentials::Credential {
                device_id: "test-device".into(),
                account_id: "test-account".into(),
                server: url.to_string(),
                access_token: None,
            },
            "test-only-token",
            true,
        )?;
        drop(store);
        let (state, session) = content_state(tmp.path())?;
        state.set("device_id", "test-device")?;
        let seq =
            state.event(json!({"kind":"tool","session_id":session.id,"text":"full output"}))?;
        let fetch = |id: &str| json!([{"id":id,"session_id":session.id,"seq":seq}]);
        // A current server advertises its extensions; a legacy one does not.
        let rounds = [
            (true, fetch("fetch-1"), "200 OK"),
            (false, fetch("fetch-2"), "200 OK"),
            (true, json!([]), "200 OK"),
            (true, json!([]), "422 Unprocessable Entity"),
            (true, json!([]), "200 OK"),
        ];
        let server = std::thread::spawn(move || -> Result<Vec<Value>> {
            let mut requests = vec![];
            for (advertise, fetches, status) in rounds {
                let (mut stream, _) = listener.accept()?;
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                let mut reader = BufReader::new(stream.try_clone()?);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line)?;
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse()?;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body)?;
                requests.push(serde_json::from_slice(&body)?);
                let mut response =
                    json!({"ack":seq,"commands":[],"uncertain":[],"fetches":fetches});
                if advertise {
                    response["sync_extensions"] = json!(["scanned_at", "fetch_results"]);
                }
                let response = response.to_string();
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )?;
            }
            Ok(requests)
        });
        state.set("scanned_at", "1700000000.5")?;
        sync_once(&state, &config, &url, &client()?)?;
        assert!(state.fetch_pending()?, "answer waits for the next sync");
        sync_once(&state, &config, &url, &client()?)?;
        assert!(!state.fetch_pending()?);
        // The legacy reply withdrew the extensions, so its fetch request is ignored.
        sync_once(&state, &config, &url, &client()?)?;
        assert!(!state.fetch_pending()?);
        assert_eq!(negotiated(&state)?, ["scanned_at", "fetch_results"]);
        // A rejected extended body (server rolled back) falls back to the legacy envelope.
        assert!(sync_once(&state, &config, &url, &client()?).is_err());
        assert!(negotiated(&state)?.is_empty());
        sync_once(&state, &config, &url, &client()?)?;
        let requests = server.join().unwrap()?;
        assert_eq!(
            requests[3]["extensions"],
            json!(["scanned_at", "fetch_results"])
        );
        for legacy in [&requests[0], &requests[2], &requests[4]] {
            for key in ["extensions", "scanned_at", "fetch_results"] {
                assert!(legacy.get(key).is_none(), "{key} sent before negotiation");
            }
        }
        assert_eq!(
            requests[1]["extensions"],
            json!(["scanned_at", "fetch_results"])
        );
        assert_eq!(requests[1]["scanned_at"], 1700000000.5);
        assert_eq!(
            requests[1]["fetch_results"],
            json!([{"id":"fetch-1","text":"full output","truncated":false}])
        );
        Ok(())
    }

    #[test]
    fn fetch_answers_follow_local_policy_and_travel_once() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let (state, session) = content_state(tmp.path())?;
        let seq = state.event(json!({"kind":"tool","session_id":session.id,"text":"visible"}))?;
        answer_fetches(
            &state,
            &[
                json!({"id":"f1","session_id":session.id,"seq":seq}),
                json!({"id":"f2","session_id":session.id,"seq":seq+5}),
                json!({"id":"f3","session_id":"codex:missing","seq":seq}),
                json!({"id":"bad id","session_id":session.id,"seq":seq}),
            ],
        )?;
        let results = state.fetch_results(usize::MAX)?;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["id"], "f1");
        assert_eq!(results[0]["text"], "visible");
        assert!(
            results[1]["error"]
                .as_str()
                .unwrap()
                .contains("no longer available")
        );
        // An unknown session is refused again at upload time by the policy recheck.
        assert!(results[2]["error"].as_str().unwrap().contains("not shared"));
        assert!(state.fetch_pending()?);
        // Revoking content sharing refuses later fetches and withholds answers already
        // computed for that project.
        state.set("policy", "{}")?;
        answer_fetches(
            &state,
            &[json!({"id":"f4","session_id":session.id,"seq":seq})],
        )?;
        let refused = state.fetch_results(usize::MAX)?;
        assert_eq!(refused.len(), 4);
        for item in [&refused[0], &refused[3]] {
            assert!(item.get("text").is_none(), "{item}");
            assert!(item["error"].as_str().unwrap().contains("not shared"));
        }
        // Answers already recorded are not recomputed, fit the budget one at a time, and
        // scrubbing content discards every cached answer.
        answer_fetches(
            &state,
            &[json!({"id":"f1","session_id":session.id,"seq":seq})],
        )?;
        assert_eq!(state.fetch_results(usize::MAX)?.len(), 4);
        let first = serde_json::to_vec(&refused[0])?.len();
        assert_eq!(state.fetch_results(first)?.len(), 1);
        state.scrub_content()?;
        assert!(!state.fetch_pending()?);
        Ok(())
    }

    #[test]
    fn oversized_inventory_is_a_scan_failure_every_scan() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = State::open(tmp.path().join("state"))?;
        state.set("scan_ok", "true")?;
        // An older, smaller inventory still queued must not hide the failed scan.
        inventory(&state)?;
        assert_eq!(state.batch()?[0]["kind"], "inventory");
        let healthy = state.get("inventory_digest")?;
        assert!(healthy.is_some());
        let tx = state.db.unchecked_transaction()?;
        for i in 0..1001 {
            state.save_session(&Session::new(
                "codex",
                &i.to_string(),
                "/work",
                "transcript",
            ))?;
        }
        tx.commit()?;
        for _ in 0..2 {
            state.set("scan_ok", "true")?;
            inventory(&state)?;
            assert_eq!(state.get("scan_ok")?.as_deref(), Some("false"));
            assert_eq!(state.get("inventory_digest")?, healthy);
            let batch = state.batch()?;
            assert_eq!(batch.last().unwrap()["kind"], "gap");
            state.ack(batch.last().unwrap()["seq"].as_i64().unwrap())?;
        }
        state
            .db
            .execute("DELETE FROM sessions WHERE id<>'codex:0'", [])?;
        inventory(&state)?;
        assert_eq!(state.batch()?[0]["kind"], "inventory");
        assert_ne!(state.get("inventory_digest")?, healthy);
        Ok(())
    }

    #[test]
    fn failed_upload_is_replayed_after_restart_and_only_acknowledged_events_are_removed()
    -> Result<()> {
        use std::net::TcpListener;
        let tmp = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        let config = tmp.path().join("config");
        let store = Store::open(config.clone(), url.as_str())?;
        store.save(
            crate::credentials::Credential {
                device_id: "test-device".into(),
                account_id: "test-account".into(),
                server: url.to_string(),
                access_token: None,
            },
            "test-only-token",
            true,
        )?;
        drop(store);
        let server = std::thread::spawn(move || -> Result<Vec<Value>> {
            let mut requests = vec![];
            for status in ["503 Service Unavailable", "200 OK"] {
                let (mut stream, _) = listener.accept()?;
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                let mut reader = BufReader::new(stream.try_clone()?);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line)?;
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse()?;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body)?;
                requests.push(serde_json::from_slice(&body)?);
                if status == "200 OK" {
                    // Large inventories on the real server can take longer than eight seconds.
                    std::thread::sleep(Duration::from_secs(9));
                }
                let response = "{\"ack\":1,\"commands\":[],\"uncertain\":[]}";
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )?;
            }
            Ok(requests)
        });
        let root = tmp.path().join("state");
        let state = State::open(root.clone())?;
        state.set("device_id", "test-device")?;
        state.event(json!({"kind":"status","text":"retained event"}))?;
        assert!(sync_once(&state, &config, &url, &client()?).is_err());
        assert_eq!(state.batch()?[0]["seq"], 1);
        drop(state);
        let state = State::open(root)?;
        sync_once(&state, &config, &url, &client()?)?;
        assert!(state.batch()?.is_empty());
        assert_eq!(state.get("last_ack")?.as_deref(), Some("1"));
        let requests = server.join().unwrap()?;
        assert_eq!(requests[0]["events"], requests[1]["events"]);
        assert_eq!(requests[0]["events"][0]["text"], "retained event");
        Ok(())
    }

    #[test]
    fn local_socket_accepts_delayed_fragmented_requests() -> Result<()> {
        let dir = tempfile::tempdir_in("/tmp")?;
        let path = dir.path().join("agent.sock");
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let mut client = UnixStream::connect(&path)?;
        let (mut server, _) = listener.accept()?;
        let writer = std::thread::spawn(move || -> std::io::Result<()> {
            std::thread::sleep(Duration::from_millis(30));
            client.write_all(b"{\"op\":")?;
            std::thread::sleep(Duration::from_millis(30));
            client.write_all(b"\"status\"}\n")
        });
        assert_eq!(read_request(&mut server)?, json!({"op":"status"}));
        writer.join().unwrap()?;
        Ok(())
    }
}
