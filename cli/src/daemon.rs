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
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::{
    fs::{OpenOptionsExt, PermissionsExt},
    net::{UnixListener, UnixStream},
};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
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

fn inventory(state: &State) -> Result<()> {
    // Keep one heartbeat behind backfill without accumulating snapshots while offline.
    if state.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM outbox WHERE json_extract(data,'$.kind')='inventory')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    let sessions: Vec<_> = state
        .sessions()?
        .into_iter()
        .filter(|s| s.live || now() - s.observed_at < 86400.)
        .collect();
    let event = json!({"kind":"inventory","sessions":sessions});
    if sessions.len() > 1000 || serde_json::to_vec(&event)?.len() > 700000 {
        state.set("scan_ok", "false")?;
        state.event(json!({"kind":"gap","text":"Inventory exceeds the supported batch size; current counts unavailable"}))?;
    } else {
        state.event(event)?;
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
            state.set(
                "policy",
                &serde_json::to_string(&crate::state::Policy::default())?,
            )?;
            state.scrub_content()?;
            let max: i64 =
                state
                    .db
                    .query_row("SELECT coalesce(max(seq),0) FROM events", [], |r| r.get(0))?;
            state.db.execute("DELETE FROM outbox", [])?;
            state.db.execute("DELETE FROM events", [])?;
            state.db.execute("DELETE FROM commands", [])?;
            state.set("stream_id", &uuid::Uuid::new_v4().to_string())?;
            if max > 0 {
                state.event(json!({"kind":"gap","dropped_from":1,"dropped_to":max}))?;
            }
        }
        state.set("device_id", &credential.device_id)?;
        inventory(state)?;
        tx.commit()?;
    }
    let batch = state.batch()?;
    let body = json!({"stream_id":state.get("stream_id")?.unwrap(),"version":env!("CARGO_PKG_VERSION"),
        "control_enabled":state.policy()?.remote_control,
        "process_count":state.get("process_count")?.and_then(|s|s.parse::<u64>().ok()).unwrap_or(0),
        "scan_ok":state.get("scan_ok")?.as_deref()==Some("true"),"events":batch});
    let response = client
        .post(server.join("/auth/device/sync")?)
        .bearer_auth(&token)
        .json(&body)
        .send()?;
    let status = response.status();
    if !status.is_success() {
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
    state.set("last_ack", &ack.to_string())?;
    state.set("connection", "online")?;
    state.set("last_sync", &now().to_string())?;
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
    let worker = std::thread::spawn(move || -> Result<()> {
        let state = State::open(sync_root)?;
        let client = client()?;
        let mut failures = 0;
        while sync_running.load(Ordering::SeqCst) {
            if let Err(error) = sync_once(&state, &config, &server, &client) {
                failures = (failures + 1).min(5);
                state.set("connection", &format!("offline; retrying: {error}"))?;
            } else {
                failures = 0;
            }
            let delay = if failures == 0 {
                if state.batch()?.is_empty() { 20 } else { 1 }
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
            // A failed scan never marks missing sessions as exited remotely.
            inventory(&state)?;
            state.prune()?;
            scanned = Instant::now();
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
    drop(providers);
    let _ = worker.join();
    let _ = fs::remove_file(socket);
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
        inventory(&state)?;
        assert_eq!(state.batch()?[0]["kind"], "inventory");
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
