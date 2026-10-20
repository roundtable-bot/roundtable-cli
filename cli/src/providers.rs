use crate::discovery::valid_id;
use crate::state::{Session, State, now};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::{
    fs::{FileTypeExt, MetadataExt},
    net::UnixStream,
};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// Bound subprocess time/output, capture rather than logging provider errors or credentials.
pub fn bounded_output(command: &mut Command, seconds: u64) -> Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut data = vec![];
        stdout
            .by_ref()
            .take(2_000_001)
            .read_to_end(&mut data)
            .map(|_| data)
    });
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(status) = child.try_wait()? {
            let data = reader
                .join()
                .map_err(|_| anyhow::anyhow!("Provider output reader failed"))??;
            if !status.success() || data.len() > 2_000_000 {
                bail!("Provider command failed");
            }
            return Ok(data);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Provider command timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct JsonProcess {
    child: Child,
    input: ChildStdin,
    output: Receiver<Value>,
}

impl JsonProcess {
    fn spawn(command: &mut Command) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Provider CLI is unavailable")?;
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::sync_channel(256);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = vec![];
                let Ok(n) = reader.by_ref().take(4_000_001).read_until(b'\n', &mut line) else {
                    break;
                };
                if n == 0 || n > 4_000_000 {
                    break;
                }
                if let Ok(value) = serde_json::from_slice(&line)
                    && tx.send(value).is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input,
            output: rx,
        })
    }
    fn send(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.input, value)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        Ok(())
    }
}
impl Drop for JsonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum RpcTransport {
    Process(JsonProcess),
    Socket(Box<tungstenite::WebSocket<UnixStream>>),
}
impl RpcTransport {
    fn send(&mut self, value: &Value) -> Result<()> {
        match self {
            Self::Process(process) => process.send(value),
            Self::Socket(socket) => {
                socket
                    .get_mut()
                    .set_nonblocking(false)
                    .context("Set RPC write mode")?;
                socket
                    .get_mut()
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .context("Set RPC write timeout")?;
                socket.send(tungstenite::Message::Text(value.to_string().into()))?;
                Ok(())
            }
        }
    }
    fn receive(&mut self, timeout: Option<Duration>) -> Result<Option<Value>> {
        match self {
            Self::Process(process) => match timeout {
                Some(wait) => match process.output.recv_timeout(wait) {
                    Ok(value) => Ok(Some(value)),
                    Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                    Err(_) => bail!("Provider connection closed"),
                },
                None => Ok(process.output.try_recv().ok()),
            },
            Self::Socket(socket) => {
                socket
                    .get_mut()
                    .set_nonblocking(timeout.is_none())
                    .context("Set RPC read mode")?;
                if let Some(wait) = timeout {
                    socket
                        .get_mut()
                        .set_read_timeout(Some(wait.max(Duration::from_millis(1))))
                        .context("Set RPC read timeout")?;
                }
                match socket.read() {
                    Ok(tungstenite::Message::Text(text)) => Ok(Some(serde_json::from_str(&text)?)),
                    Ok(tungstenite::Message::Close(_)) => bail!("Shared Codex connection closed"),
                    Ok(_) => Ok(None),
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        Ok(None)
                    }
                    Err(e) => Err(e.into()),
                }
            }
        }
    }
    fn pid(&self) -> Option<u32> {
        match self {
            Self::Process(p) => Some(p.child.id()),
            Self::Socket(_) => None,
        }
    }
    fn exited(&mut self) -> Result<bool> {
        match self {
            Self::Process(p) => Ok(p.child.try_wait()?.is_some()),
            Self::Socket(_) => Ok(false),
        }
    }
}
struct Rpc {
    transport: RpcTransport,
    serial: u64,
    notifications: VecDeque<Value>,
    streamed_items: HashSet<String>,
}
impl Rpc {
    fn new(command: &mut Command) -> Result<Self> {
        Self::initialize(RpcTransport::Process(JsonProcess::spawn(command)?))
    }
    fn shared(path: &Path) -> Result<Self> {
        let owner = unsafe { libc::geteuid() };
        let link = std::fs::symlink_metadata(path).context("Inspect shared socket")?;
        let parent = std::fs::metadata(path.parent().context("Shared socket has no parent")?)?;
        if link.uid() != owner
            || !parent.is_dir()
            || parent.uid() != owner
            || parent.mode() & 0o077 != 0
        {
            bail!("Shared Codex socket must be in a private directory owned by this user");
        }
        // Codex publishes a symlink to a short socket path under its private runtime directory.
        // Connect to the validated canonical target, not through a mutable symlink again.
        let target = std::fs::canonicalize(path).context("Resolve shared socket")?;
        let meta = std::fs::symlink_metadata(&target)?;
        let parent = std::fs::metadata(target.parent().context("Shared target has no parent")?)?;
        if !meta.file_type().is_socket()
            || meta.uid() != owner
            || meta.mode() & 0o022 != 0
            || !parent.is_dir()
            || parent.uid() != owner
            || parent.mode() & 0o077 != 0
        {
            bail!("Shared Codex socket target must be private and owned by this user");
        }
        let stream = UnixStream::connect(&target).context("Connect shared socket")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .context("Set shared read timeout")?;
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .context("Set shared write timeout")?;
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(4_000_000))
            .max_frame_size(Some(4_000_000));
        let (socket, _) =
            tungstenite::client::client_with_config("ws://localhost/", stream, Some(config))
                .context("Shared WebSocket handshake")?;
        Self::initialize(RpcTransport::Socket(Box::new(socket))).context("Initialize shared RPC")
    }
    fn initialize(transport: RpcTransport) -> Result<Self> {
        let mut rpc = Self {
            transport,
            serial: 0,
            notifications: VecDeque::new(),
            streamed_items: HashSet::new(),
        };
        rpc.call("initialize",json!({"clientInfo":{"name":"roundtable","title":"Roundtable","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        rpc.transport.send(&json!({"method":"initialized"}))?;
        Ok(rpc)
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.serial += 1;
        let id = self.serial;
        self.transport
            .send(&json!({"id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if Instant::now() >= deadline {
                bail!("Provider response timed out; delivery may be uncertain");
            }
            let Some(value) = self
                .transport
                .receive(Some(deadline.saturating_duration_since(Instant::now())))?
            else {
                continue;
            };
            if value["id"] == id {
                if value.get("error").is_some() {
                    bail!("Provider rejected this operation");
                }
                return Ok(value["result"].clone());
            }
            if value.get("method").is_some() {
                // No approval is granted implicitly. Managed sessions deny approval-required work.
                if value.get("id").is_some() {
                    self.transport.send(&json!({"id":value["id"],"error":{"code":-32601,"message":"Roundtable does not approve provider permission requests"}}))?;
                } else {
                    if self.notifications.len() >= 1000 {
                        self.notifications.pop_front();
                    }
                    self.notifications.push_back(value);
                }
            }
        }
    }
    fn drain(&mut self) -> Vec<Value> {
        let mut result: Vec<_> = self.notifications.drain(..).collect();
        for _ in 0..500 {
            match self.transport.receive(None) {
                Ok(Some(v)) => result.push(v),
                _ => break,
            }
        }
        result
    }
}

enum Managed {
    Codex(Rpc),
    Claude(JsonProcess),
}

pub struct Providers {
    proxy: Option<Rpc>,
    managed: HashMap<String, Managed>,
    last_proxy_attempt: Instant,
}
impl Providers {
    pub fn new() -> Self {
        Self {
            proxy: None,
            managed: HashMap::new(),
            last_proxy_attempt: Instant::now() - Duration::from_secs(60),
        }
    }
    pub fn start(
        &mut self,
        state: &State,
        provider: &str,
        cwd: &Path,
        workspace_write: bool,
    ) -> Result<Session> {
        let cwd = std::fs::canonicalize(cwd)?;
        if !cwd.is_dir() {
            bail!("Project must be a directory");
        }
        let project = cwd.to_string_lossy();
        let (mut session, managed) = match provider {
            "codex" => {
                let mut rpc = Rpc::new(
                    Command::new("codex")
                        .args(["app-server", "--stdio"])
                        .current_dir(&cwd),
                )?;
                let response=rpc.call("thread/start",json!({"cwd":project,"approvalPolicy":"never","sandbox":if workspace_write{"workspace-write"}else{"read-only"},"serviceName":"roundtable"}))?;
                let id = response["thread"]["id"]
                    .as_str()
                    .filter(|s| valid_id(s))
                    .context("Provider did not return a session ID")?;
                let mut s = Session::new("codex", id, &project, "managed");
                s.pid = rpc.transport.pid();
                s.capabilities = vec!["prompt".into(), "steer".into(), "interrupt".into()];
                (s, Managed::Codex(rpc))
            }
            "claude" => {
                let id = uuid::Uuid::new_v4().to_string();
                let mut command = Command::new("claude");
                command.current_dir(&cwd).args([
                    "-p",
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--session-id",
                    &id,
                ]);
                if workspace_write {
                    command.args(["--permission-mode", "acceptEdits"]);
                } else {
                    command.args(["--permission-mode", "dontAsk", "--tools", "Read,Grep,Glob"]);
                }
                let process = JsonProcess::spawn(&mut command)?;
                let mut s = Session::new("claude", &id, &project, "managed");
                s.pid = Some(process.child.id());
                s.capabilities = vec!["prompt".into(), "interrupt".into()];
                (s, Managed::Claude(process))
            }
            _ => bail!("Provider must be codex or claude"),
        };
        session.live = true;
        session.state = "idle".into();
        session.collection = if state.policy()?.content(&project) {
            "content"
        } else {
            "metadata"
        }
        .into();
        state.save_session(&session)?;
        self.managed.insert(session.id.clone(), managed);
        Ok(session)
    }
    pub fn inventory(&mut self, state: &State) -> Result<()> {
        if self.proxy.is_none() && self.last_proxy_attempt.elapsed() > Duration::from_secs(30) {
            self.last_proxy_attempt = Instant::now();
            // The control socket carries WebSocket frames, not the stdio JSONL transport.
            // Connect only to the existing same-user daemon; never resume its threads.
            self.proxy = Rpc::shared(
                &crate::discovery::home("codex").join("app-server-control/app-server-control.sock"),
            )
            .ok();
        }
        let mut loaded = vec![];
        if let Some(proxy) = self.proxy.as_mut() {
            let result = (|| -> Result<()> {
                let mut cursor = Value::Null;
                for _ in 0..10 {
                    let page =
                        proxy.call("thread/loaded/list", json!({"limit":100,"cursor":cursor}))?;
                    for id in page["data"]
                        .as_array()
                        .context("Invalid thread inventory")?
                    {
                        let Some(id) = id.as_str().filter(|s| valid_id(s)) else {
                            continue;
                        };
                        let response = proxy
                            .call("thread/read", json!({"threadId":id,"includeTurns":false}))?;
                        let thread = &response["thread"];
                        let mut s = state.session(&format!("codex:{id}")).unwrap_or_else(|_| {
                            Session::new(
                                "codex",
                                id,
                                thread["cwd"].as_str().unwrap_or(""),
                                "app-server",
                            )
                        });
                        if s.source == "managed" {
                            continue;
                        }
                        s.source = "app-server".into();
                        s.live = thread["status"]["type"] != "notLoaded";
                        s.observed_at = now();
                        s.state = match thread["status"]["type"].as_str() {
                            Some("idle") => "idle",
                            Some("active") => {
                                if thread["status"]["activeFlags"].as_array().is_some_and(|a| {
                                    a.iter().any(|v| {
                                        v == "waitingOnApproval" || v == "waitingOnUserInput"
                                    })
                                }) {
                                    "waiting"
                                } else {
                                    "working"
                                }
                            }
                            _ => "unknown",
                        }
                        .into();
                        s.capabilities = if s.live {
                            vec!["prompt".into(), "steer".into(), "interrupt".into()]
                        } else {
                            vec![]
                        };
                        if s.state == "idle" {
                            s.turn_id = None;
                        }
                        s.collection = if state.policy()?.content(&s.project) {
                            "content"
                        } else {
                            "metadata"
                        }
                        .into();
                        // Read only. Never resume a thread owned by another client.
                        // A local transcript includes all turns and tool types, unlike the recent-turn API.
                        if !state
                            .get(&format!("transcript:{}", s.id))?
                            .is_some_and(|path| Path::new(&path).is_file())
                            && !state.backlogged()?
                        {
                            let history = external_history(proxy, state, &mut s);
                            state.set(
                                &format!("history:{}", s.id),
                                if history.is_ok() { "true" } else { "false" },
                            )?;
                        }
                        state.save_session(&s)?;
                        loaded.push(s.id);
                    }
                    cursor = page["nextCursor"].clone();
                    if cursor.is_null() {
                        break;
                    }
                }
                Ok(())
            })();
            if result.is_err() {
                self.proxy = None;
                loaded.clear();
            }
        }
        for mut s in state.sessions()? {
            if s.source == "app-server" && !loaded.contains(&s.id) {
                s.live = false;
                s.state = "unknown".into();
                s.capabilities.clear();
                state.save_session(&s)?;
            }
        }
        Ok(())
    }
    pub fn drain(&mut self, state: &State) -> Result<()> {
        let mut exited = vec![];
        for (id, managed) in &mut self.managed {
            let mut session = state.session(id)?;
            let previous = session.clone();
            session.collection = if state.policy()?.content(&session.project) {
                "content"
            } else {
                "metadata"
            }
            .into();
            match managed {
                Managed::Codex(rpc) => {
                    for value in rpc.drain() {
                        codex_event(state, &mut session, &value, &mut rpc.streamed_items)?;
                    }
                    if rpc.transport.exited()? {
                        exited.push(id.clone());
                    }
                }
                Managed::Claude(process) => {
                    for _ in 0..500 {
                        let Ok(value) = process.output.try_recv() else {
                            break;
                        };
                        claude_completion(state, &session, &value)?;
                        crate::discovery::parse(state, &mut session, &value)?;
                        if value["type"] == "result" {
                            session.state = "idle".into();
                            session.turn_id = None;
                            if value["is_error"] == true {
                                state.event(json!({"kind":"status","session_id":id,"text":"Provider reported a failed turn"}))?;
                            }
                        }
                    }
                    if process.child.try_wait()?.is_some() {
                        exited.push(id.clone());
                    }
                }
            }
            if session != previous || now() - session.observed_at >= 1. {
                session.observed_at = now();
                state.save_session(&session)?;
            }
        }
        // Existing app-server sessions may publish live turn events through the proxy.
        // History polling itself never manufactures a completion signal.
        if let Some(proxy) = self.proxy.as_mut() {
            for value in proxy.drain() {
                if !matches!(
                    value["method"].as_str(),
                    Some("turn/started" | "turn/completed")
                ) {
                    continue;
                }
                let Some(thread) = value["params"]["threadId"].as_str() else {
                    continue;
                };
                let Ok(mut session) = state.session(&format!("codex:{thread}")) else {
                    continue;
                };
                if session.source != "app-server" {
                    continue;
                }
                codex_event(state, &mut session, &value, &mut HashSet::new())?;
                session.observed_at = now();
                state.save_session(&session)?;
            }
        }
        for id in exited {
            self.managed.remove(&id);
            let mut session = state.session(&id)?;
            session.live = false;
            session.state = "exited".into();
            session.capabilities.clear();
            state.save_session(&session)?;
        }
        Ok(())
    }
    pub fn ready(&self, session: &Session, command: &Value) -> bool {
        command["action"] == "interrupt" || command["timing"] == "steer" || session.state == "idle"
    }
    pub fn control(&mut self, state: &State, command: &Value) -> Result<()> {
        let id = command["session_id"]
            .as_str()
            .context("Session ID is required")?;
        let mut session = state.session(id)?;
        let action = command["action"].as_str().unwrap_or("prompt");
        let steer = command["timing"] == "steer";
        if !session.live
            || !session
                .capabilities
                .iter()
                .any(|c| c == if steer { "steer" } else { action })
        {
            bail!("unsupported");
        }
        if (action == "interrupt" || steer)
            && (session.turn_id.is_none()
                || command["expected_turn_id"].as_str() != session.turn_id.as_deref())
        {
            bail!("stale_turn");
        }
        if !self.ready(&session, command) {
            bail!("Session is busy; queued commands wait for the next idle turn");
        }
        let input = json!([{"type":"text","text":command["message"].as_str().unwrap_or(""),"text_elements":[]}]);
        let method = if action == "interrupt" {
            "turn/interrupt"
        } else if steer {
            "turn/steer"
        } else {
            "turn/start"
        };
        let params = if action == "interrupt" {
            json!({"threadId":session.provider_session_id,"turnId":session.turn_id})
        } else if steer {
            json!({"threadId":session.provider_session_id,"expectedTurnId":session.turn_id,"input":input})
        } else {
            json!({"threadId":session.provider_session_id,"input":input})
        };
        if let Some(managed) = self.managed.get_mut(id) {
            match managed {
                Managed::Codex(rpc) => {
                    let result = rpc.call(method, params)?;
                    if action == "prompt" {
                        session.turn_id = result["turn"]["id"]
                            .as_str()
                            .or(result["turnId"].as_str())
                            .map(str::to_owned);
                    }
                }
                Managed::Claude(process) => {
                    if action == "interrupt" {
                        if unsafe { libc::kill(process.child.id() as i32, libc::SIGINT) } != 0 {
                            bail!("provider_error");
                        }
                    } else {
                        process.send(&json!({"type":"user","message":{"role":"user","content":command["message"]},"session_id":session.provider_session_id,"parent_tool_use_id":null}))?;
                        session.turn_id = Some(uuid::Uuid::new_v4().to_string());
                    }
                }
            }
        } else if session.provider == "codex" && session.source == "app-server" {
            let rpc = self.proxy.as_mut().context("unsupported")?;
            // Verify the thread is still loaded on this exact connection before mutating it.
            let current = rpc.call(
                "thread/read",
                json!({"threadId":session.provider_session_id,"includeTurns":false}),
            )?;
            let status = current["thread"]["status"]["type"]
                .as_str()
                .unwrap_or("notLoaded");
            if status == "notLoaded" || (!steer && action == "prompt" && status != "idle") {
                bail!("stale_turn");
            }
            let result = rpc.call(method, params)?;
            if action == "prompt" {
                session.turn_id = result["turn"]["id"]
                    .as_str()
                    .or(result["turnId"].as_str())
                    .map(str::to_owned);
            }
        } else {
            bail!("unsupported");
        }
        if action == "prompt" {
            session.state = "working".into();
            visible_event(
                state,
                &session,
                "prompt",
                command["message"].as_str().unwrap_or(""),
                false,
            )?;
        }
        session.observed_at = now();
        state.save_session(&session)?;
        Ok(())
    }
}

fn external_history(rpc: &mut Rpc, state: &State, session: &mut Session) -> Result<()> {
    let content = state.policy()?.content(&session.project);
    let page = rpc.call(
        "thread/turns/list",
        json!({"threadId":session.provider_session_id,
        "limit":5,"sortDirection":"desc","itemsView":if content {"full"} else {"notLoaded"}}),
    )?;
    let turns = page["data"].as_array().context("Invalid turn history")?;
    if let Some(turn) = turns.iter().find(|t| t["status"] == "inProgress") {
        session.turn_id = turn["id"].as_str().map(str::to_owned);
    }
    if content {
        for turn in turns.iter().rev() {
            for item in turn["items"].as_array().into_iter().flatten() {
                record_external_item(state, session, item)?;
            }
        }
    }
    Ok(())
}

fn record_external_item(state: &State, session: &Session, item: &Value) -> Result<()> {
    let Some(id) = item["id"].as_str() else {
        return Ok(());
    };
    let (kind, text) = match item["type"].as_str() {
        Some("userMessage") => ("prompt", crate::discovery::text_content(&item["content"])),
        Some("agentMessage") => (
            "response",
            item["text"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(16000)
                .collect(),
        ),
        Some("commandExecution") => (
            "tool",
            item["aggregatedOutput"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(16000)
                .collect(),
        ),
        _ => return Ok(()), // Do not collect reasoning or opaque provider payloads.
    };
    if text.is_empty() {
        return Ok(());
    }
    let tx = state.db.unchecked_transaction()?;
    use rusqlite::OptionalExtension;
    let previous: Option<(usize, String)> = state
        .db
        .query_row(
            "SELECT length,hash FROM provider_items WHERE session=? AND item=?",
            rusqlite::params![session.id, id],
            |r| Ok((r.get::<_, i64>(0)? as usize, r.get(1)?)),
        )
        .optional()?;
    let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
    if previous.as_ref().is_some_and(|(_, h)| h == &hash) {
        return Ok(());
    }
    let start = previous
        .filter(|(n, h)| {
            text.get(..*n)
                .is_some_and(|s| format!("{:x}", Sha256::digest(s.as_bytes())) == *h)
        })
        .map(|(n, _)| n)
        .unwrap_or(0);
    visible_event(state, session, kind, &text[start..], start > 0)?;
    state.db.execute("INSERT INTO provider_items VALUES (?,?,?,?,?) ON CONFLICT(session,item) DO UPDATE SET length=excluded.length,hash=excluded.hash,seen=excluded.seen",
        rusqlite::params![session.id,id,text.len() as i64,hash,now()])?;
    state.db.execute("DELETE FROM provider_items WHERE rowid IN (SELECT rowid FROM provider_items WHERE session=? ORDER BY seen DESC LIMIT -1 OFFSET 1000)",[&session.id])?;
    tx.commit()?;
    Ok(())
}

fn visible_event(
    state: &State,
    session: &Session,
    kind: &str,
    text: &str,
    append: bool,
) -> Result<()> {
    let mut event = json!({"kind":kind,"session_id":session.id,"session":session,"append":append});
    if state.policy()?.content(&session.project) {
        event["text"] = json!(text);
    }
    state.event(event)?;
    Ok(())
}

fn claude_completion(state: &State, session: &Session, value: &Value) -> Result<()> {
    // Only live provider results; transcript discovery must never call this helper.
    if value["type"] == "result"
        && value["is_error"] == false
        && value["subtype"] == "success"
        && let Some(turn) = value["uuid"].as_str().or(session.turn_id.as_deref())
    {
        state.event(
            json!({"kind":"status","session_id":session.id,"session":session,
            "completed_turn_id":turn,"text":"Turn completed"}),
        )?;
    }
    Ok(())
}

fn codex_event(
    state: &State,
    session: &mut Session,
    v: &Value,
    streamed: &mut HashSet<String>,
) -> Result<()> {
    let p = &v["params"];
    if p["threadId"]
        .as_str()
        .is_some_and(|id| id != session.provider_session_id)
    {
        return Ok(());
    }
    match v["method"].as_str().unwrap_or("") {
        "item/agentMessage/delta" => {
            if let Some(id) = p["itemId"].as_str() {
                streamed.insert(id.to_owned());
            }
            visible_event(
                state,
                session,
                "response",
                p["delta"].as_str().unwrap_or(""),
                true,
            )?;
        }
        "item/completed" => {
            let item = &p["item"];
            if item["type"] == "agentMessage"
                && !streamed.contains(item["id"].as_str().unwrap_or(""))
            {
                visible_event(
                    state,
                    session,
                    "response",
                    item["text"].as_str().unwrap_or(""),
                    false,
                )?;
            }
            if item["type"] == "commandExecution" {
                visible_event(
                    state,
                    session,
                    "tool",
                    item["aggregatedOutput"]
                        .as_str()
                        .unwrap_or("Command completed"),
                    false,
                )?;
            }
        }
        "turn/started" => {
            session.state = "working".into();
            session.turn_id = p["turn"]["id"].as_str().map(str::to_owned);
        }
        "turn/completed" => {
            streamed.clear();
            if p["turn"]["status"] == "completed" && p["turn"]["id"].as_str().is_some() {
                state.event(
                    json!({"kind":"status","session_id":session.id,"session":session,
                    "completed_turn_id":p["turn"]["id"],"text":"Turn completed"}),
                )?;
            }
            session.state = "idle".into();
            session.turn_id = None;
            state.event(json!({"kind":"status","session_id":session.id,"text":format!("Turn {}",p["turn"]["status"].as_str().unwrap_or("unknown"))}))?;
        }
        "thread/tokenUsage/updated" => {
            let total = &p["tokenUsage"]["total"];
            session.usage.input_tokens = total["inputTokens"].as_u64();
            session.usage.output_tokens = total["outputTokens"].as_u64();
            session.usage.cached_input_tokens = total["cachedInputTokens"].as_u64();
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_daemon_uses_websocket_frames_and_preserves_notifications() -> Result<()> {
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        let tmp = tempfile::tempdir_in("/tmp")?;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700))?;
        let path = tmp.path().join("control.sock");
        let listener = UnixListener::bind(&path).context("Bind test socket")?;
        let link_dir = tmp.path().join("control");
        std::fs::create_dir(&link_dir)?;
        std::fs::set_permissions(&link_dir, std::fs::Permissions::from_mode(0o700))?;
        let link = link_dir.join("published.sock");
        std::os::unix::fs::symlink(&path, &link)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777))
            .context("Make test socket shared")?;
        assert!(Rpc::shared(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("Make test socket private")?;
        let (done, finish) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let init: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(init["method"], "initialize");
            socket
                .send(tungstenite::Message::Text(
                    json!({"id":init["id"],"result":{}}).to_string().into(),
                ))
                .unwrap();
            let initialized: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(initialized["method"], "initialized");
            let request: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            socket
                .send(tungstenite::Message::Ping(vec![1, 2].into()))
                .unwrap();
            socket
                .send(tungstenite::Message::Text(
                    json!({"method":"turn/started","params":{"threadId":"existing"}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            socket
                .send(tungstenite::Message::Text(
                    json!({"id":request["id"],"result":{"data":["existing"]}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            finish.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        std::fs::set_permissions(&link_dir, std::fs::Permissions::from_mode(0o777))?;
        assert!(Rpc::shared(&link).is_err());
        std::fs::set_permissions(&link_dir, std::fs::Permissions::from_mode(0o700))?;
        let mut rpc = Rpc::shared(&link)?;
        assert_eq!(
            rpc.call("thread/loaded/list", json!({}))?["data"][0],
            "existing"
        );
        assert_eq!(rpc.drain()[0]["method"], "turn/started");
        done.send(())?;
        server.join().unwrap();
        Ok(())
    }
    #[test]
    fn only_successful_live_results_emit_completion_metadata() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = State::open(tmp.path().join("state"))?;
        let mut codex = Session::new("codex", "one", "/work/private-project", "managed");
        codex.live = true;
        let mut streamed = HashSet::new();
        for status in ["failed", "interrupted", "completed"] {
            codex_event(
                &state,
                &mut codex,
                &json!({"method":"turn/completed",
                "params":{"threadId":"one","turn":{"id":status,"status":status}}}),
                &mut streamed,
            )?;
        }
        codex_event(
            &state,
            &mut codex,
            &json!({"method":"turn/completed",
            "params":{"threadId":"someone-else","turn":{"id":"wrong","status":"completed"}}}),
            &mut streamed,
        )?;
        let events = state.events(&codex.id, 0)?;
        let completions: Vec<_> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e.get("completed_turn_id").is_some())
            .collect();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0]["completed_turn_id"], "completed");

        let mut claude = Session::new("claude", "two", "/work/private-project", "managed");
        claude.live = true;
        for (subtype, error) in [
            ("success", true),
            ("error_max_turns", false),
            ("success", false),
        ] {
            claude_completion(
                &state,
                &claude,
                &json!({"type":"result", "subtype":subtype,
                "is_error":error,"uuid":"result-one","result":"private answer"}),
            )?;
        }
        let events = state.events(&claude.id, 0)?;
        assert_eq!(events["events"].as_array().unwrap().len(), 1);
        assert_eq!(events["events"][0]["completed_turn_id"], "result-one");
        assert!(!events.to_string().contains("private answer"));
        Ok(())
    }

    #[test]
    fn existing_session_history_is_deduplicated_across_restarts() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = State::open(tmp.path().join("state"))?;
        state.set(
            "policy",
            &json!({"content_roots":[std::fs::canonicalize(tmp.path())?],"remote_control":false})
                .to_string(),
        )?;
        let mut session = Session::new(
            "codex",
            "existing",
            tmp.path().to_str().unwrap(),
            "app-server",
        );
        session.collection = "content".into();
        state.save_session(&session)?;
        record_external_item(
            &state,
            &session,
            &json!({"id":"u1","type":"userMessage","content":[{"type":"text","text":"Review code"}]}),
        )?;
        record_external_item(
            &state,
            &session,
            &json!({"id":"r1","type":"agentMessage","text":"Checking"}),
        )?;
        drop(state);
        let state = State::open(tmp.path().join("state"))?;
        record_external_item(
            &state,
            &session,
            &json!({"id":"r1","type":"agentMessage","text":"Checking"}),
        )?;
        record_external_item(
            &state,
            &session,
            &json!({"id":"r1","type":"agentMessage","text":"Checking files"}),
        )?;
        record_external_item(
            &state,
            &session,
            &json!({"id":"private","type":"reasoning","text":"do not collect"}),
        )?;
        let events = state.events(&session.id, 0)?;
        assert_eq!(events["events"].as_array().unwrap().len(), 3);
        assert_eq!(events["events"][0]["text"], "Review code");
        assert_eq!(events["events"][2]["text"], " files");
        assert_eq!(events["events"][2]["append"], true);
        Ok(())
    }
}
