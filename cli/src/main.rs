mod credentials;
mod daemon;
mod discovery;
mod init;
mod providers;
mod service;
mod state;
mod t3;
mod update;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use credentials::{Credential, Store};
use reqwest::blocking::{Client, Response};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "roundtable",
    version,
    about = "Connect this machine to Roundtable"
)]
struct Cli {
    #[arg(long, global = true, env = "ROUNDTABLE_URL", default_value = "https://roundtable.bot", value_parser = server_url)]
    server: Url,
    #[arg(long, global = true, env = "ROUNDTABLE_CONFIG_DIR")]
    config_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create an SDK project and provision its bots using a team key
    Init {
        #[arg(default_value = ".")]
        dir: PathBuf,
        #[arg(long, value_parser = ["diligence", "sourcing", "portfolio", "funds"])]
        template: String,
        #[arg(long)]
        project: Option<String>,
        /// Allow replacing project files in a non-empty directory
        #[arg(long)]
        force: bool,
    },
    /// Inspect delivery receipts and resolve uncertain outcomes after checking the provider
    #[command(name = "commands")]
    Receipts {
        #[command(subcommand)]
        command: ReceiptCommands,
    },
    /// Run the machine service in the foreground (works without systemd)
    Daemon,
    /// Install and operate a user LaunchAgent or systemd service
    Service {
        #[arg(value_parser=["install","start","stop","status","uninstall"])]
        action: String,
    },
    /// Configure monitoring and install/start the background service
    Setup {
        /// Allow prompt/response collection only inside these project directories
        #[arg(long)]
        content: Vec<PathBuf>,
        /// Allow remote control, still requiring an explicit website assistant grant
        #[arg(long)]
        allow_control: bool,
    },
    /// Configure content collection and remote control locally
    Monitor {
        #[arg(long)]
        content: Vec<PathBuf>,
        #[arg(long, conflicts_with = "content")]
        metadata_only: bool,
        #[arg(long, conflicts_with = "deny_control")]
        allow_control: bool,
        #[arg(long)]
        deny_control: bool,
        /// Install verified releases automatically (`auto`, the default) or only by hand (`manual`)
        #[arg(long, value_parser = ["auto", "manual"])]
        updates: Option<String>,
    },
    /// Discover sessions, read activity, and prompt managed or attachable providers
    Sessions {
        #[command(subcommand)]
        command: SessionCommands,
    },
    /// Sign in through your browser and connect this machine
    Login {
        /// Suggest a computer name; you can edit it when approving in the browser
        #[arg(long)]
        name: Option<String>,
        /// Print the browser URL without trying to open it (useful over SSH)
        #[arg(long)]
        no_browser: bool,
        /// Store the credential in a private plaintext file instead of the OS keychain
        #[arg(long)]
        insecure_storage: bool,
    },
    /// Check this machine's login with the server
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Revoke this machine's credential and remove it locally
    Logout,
}

#[derive(Subcommand)]
enum ReceiptCommands {
    List,
    /// Record what you verified in the provider; never resends the prompt
    Resolve {
        id: String,
        #[arg(long,value_parser=["delivered","not-delivered"])]
        outcome: String,
    },
}

fn receipt_command(root: &std::path::Path, command: ReceiptCommands) -> Result<()> {
    let state = state::State::open(root.to_owned())?;
    match command {
        ReceiptCommands::List => {
            let mut stmt=state.db.prepare("SELECT id,status,code,json_extract(data,'$.session_id') FROM commands ORDER BY rowid DESC LIMIT 100")?;
            let rows=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"code":r.get::<_,String>(2)?,"session_id":r.get::<_,Option<String>>(3)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        ReceiptCommands::Resolve { id, outcome } => {
            let (status, code) = if outcome == "delivered" {
                ("completed", "delivered")
            } else {
                ("failed", "provider_error")
            };
            let n=state.db.execute("UPDATE commands SET status=?,code=? WHERE id=? AND (status='uncertain' OR (status='reported' AND code IN ('crash','provider_error')))",rusqlite::params![status,code,id])?;
            if n == 0 {
                bail!("No uncertain command found. Check roundtable commands list.");
            }
            println!(
                "Outcome recorded. The service will send the receipt; the prompt was not resent."
            );
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum SessionCommands {
    List {
        #[arg(long)]
        json: bool,
    },
    Events {
        session_id: String,
        #[arg(long, default_value = "0")]
        after: i64,
        #[arg(long)]
        follow: bool,
    },
    Start {
        #[arg(value_parser=["codex","claude"])]
        provider: String,
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        #[arg(long)]
        workspace_write: bool,
    },
    Prompt {
        session_id: String,
        message: String,
        #[arg(long)]
        steer: bool,
        #[arg(long)]
        expected_turn_id: Option<String>,
    },
    Interrupt {
        session_id: String,
        #[arg(long)]
        expected_turn_id: Option<String>,
    },
}

fn configure(
    state: &state::State,
    content: Vec<PathBuf>,
    metadata_only: bool,
    allow: bool,
    deny: bool,
    updates: Option<String>,
) -> Result<()> {
    let tx = state.db.unchecked_transaction()?;
    let mut policy = state.policy()?;
    if metadata_only {
        policy.content_roots.clear();
    }
    for path in content {
        let path = std::fs::canonicalize(path).context("Content project must exist")?;
        if !path.is_dir() {
            bail!("Content project must be a directory");
        }
        if !policy.content_roots.contains(&path) {
            policy.content_roots.push(path);
        }
    }
    if allow {
        policy.remote_control = true;
    }
    if deny {
        policy.remote_control = false;
    }
    if let Some(updates) = updates {
        policy.manual_updates = updates == "manual";
    }
    state.set("policy", &serde_json::to_string(&policy)?)?;
    state.scrub_content()?;
    tx.commit()?;
    println!("{}", serde_json::to_string_pretty(&policy)?);
    Ok(())
}

fn session_command(root: &std::path::Path, command: SessionCommands) -> Result<()> {
    let value = match command {
        SessionCommands::List { json: as_json } => {
            let result = match daemon::request(root, &json!({"op":"list"})) {
                Ok(v) => v,
                Err(_) => {
                    let state = state::State::open(root.to_owned())?;
                    discovery::scan(&state, &mut sysinfo::System::new())?;
                    json!({"sessions":state.sessions()?})
                }
            };
            if as_json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                for session in result["sessions"].as_array().into_iter().flatten() {
                    println!(
                        "{}  {}  {}  {}",
                        safe(session["id"].as_str().unwrap_or("")),
                        if session["live"] == true {
                            "live"
                        } else {
                            "history"
                        },
                        safe(session["state"].as_str().unwrap_or("unknown")),
                        safe(session["project"].as_str().unwrap_or(""))
                    );
                }
            }
            return Ok(());
        }
        SessionCommands::Events {
            session_id,
            mut after,
            follow,
        } => loop {
            let state = state::State::open(root.to_owned())?;
            let result = state.events(&session_id, after)?;
            after = result["cursor"].as_i64().unwrap_or(after);
            println!("{}", serde_json::to_string(&result)?);
            if !follow {
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(1));
        },
        SessionCommands::Start {
            provider,
            cwd,
            workspace_write,
        } => {
            json!({"op":"start","provider":provider,"cwd":std::fs::canonicalize(cwd)?,"workspace_write":workspace_write})
        }
        SessionCommands::Prompt {
            session_id,
            message,
            steer,
            expected_turn_id,
        } => {
            if message.is_empty() || message.len() > 16000 {
                bail!("Prompt must contain 1–16000 bytes");
            }
            json!({"op":"prompt","action":"prompt","session_id":session_id,"message":message,"timing":if steer{"steer"}else{"queue"},"expected_turn_id":expected_turn_id})
        }
        SessionCommands::Interrupt {
            session_id,
            expected_turn_id,
        } => {
            json!({"op":"interrupt","action":"interrupt","session_id":session_id,"expected_turn_id":expected_turn_id})
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&daemon::request(root, &value)?)?
    );
    Ok(())
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri_complete: Url,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct Grant {
    access_token: String,
    device_id: String,
    device_name: String,
    account_id: String,
}

fn server_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "Use an absolute HTTPS server URL")?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.host_str().is_none()
        || !(url.scheme() == "https" || url.scheme() == "http" && local)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("Use an HTTPS origin without credentials, a path, query, or fragment (HTTP is allowed only on loopback)".into());
    }
    Ok(url)
}

fn response(response: Response) -> Result<Value> {
    let status = response.status();
    let data: Value = response
        .json()
        .context("Server returned an invalid response")?;
    if !status.is_success() {
        // Never print response bodies: token responses contain secrets.
        bail!(
            "Roundtable returned HTTP {}: {}",
            status.as_u16(),
            safe(
                data.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("request failed")
            )
        );
    }
    Ok(data)
}

fn safe(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect()
}

fn endpoint(server: &Url, path: &str) -> Url {
    server.join(path).expect("constant absolute endpoint")
}

fn open_browser(url: &Url) -> Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = Command::new(program)
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn login(
    client: &Client,
    server: &Url,
    store: &Store,
    name: Option<String>,
    no_browser: bool,
    file_storage: bool,
) -> Result<()> {
    if let Some((credential, token)) = store.load()? {
        let existing = client
            .get(endpoint(server, "/auth/device/session"))
            .bearer_auth(&token)
            .send()
            .context("Could not verify the existing login; it was kept")?;
        if existing.status() == StatusCode::UNAUTHORIZED {
            let revoked = client
                .post(endpoint(server, "/auth/device/logout"))
                .bearer_auth(&token)
                .send()
                .context("Could not revoke the old login; it was kept")?;
            if revoked.status() != StatusCode::UNAUTHORIZED {
                response(revoked)?;
            }
            store.remove(&credential)?;
        } else {
            response(existing)?;
            bail!(
                "Already logged in. Use roundtable status, or roundtable logout to switch accounts."
            );
        }
    }
    let name = name.unwrap_or_else(|| {
        hostname::get()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "My machine".into())
    });
    let platform = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let code: DeviceCode = serde_json::from_value(response(
        client
            .post(endpoint(server, "/auth/device/code"))
            .json(&json!({"name": name, "platform": platform}))
            .send()?,
    )?)?;
    // The server cannot redirect a user or credential to a different origin.
    if code.verification_uri_complete.origin() != server.origin()
        || code.verification_uri_complete.path() != "/cli/authorize"
        || !code.verification_uri_complete.username().is_empty()
        || code.verification_uri_complete.password().is_some()
        || code.expires_in > 600
        || code.expires_in == 0
        || code.interval == 0
        || code.interval > 60
    {
        bail!("Server returned an invalid device authorization URL or expiry");
    }
    println!(
        "Your code: {}\nOpen {}\nApprove this code in the browser. Waiting for login…",
        safe(&code.user_code),
        code.verification_uri_complete
    );
    if !no_browser && open_browser(&code.verification_uri_complete).is_err() {
        eprintln!("Could not open a browser. Open the URL above on any device.");
    }
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = code.interval.max(5);
    loop {
        if Instant::now() + Duration::from_secs(interval) >= deadline {
            bail!("Login expired. Run roundtable login again.");
        }
        std::thread::sleep(Duration::from_secs(interval));
        let poll = match client
            .post(endpoint(server, "/auth/device/token"))
            .json(&json!({"device_code": code.device_code}))
            .send()
        {
            Ok(response) => response,
            Err(error) if error.is_timeout() || error.is_connect() || error.is_request() => {
                interval = (interval * 2).min(60);
                eprintln!("Connection interrupted; retrying login…");
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let status = poll.status();
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            interval = (interval * 2).min(60);
            continue;
        }
        let data: Value = poll
            .json()
            .context("Invalid login response; run roundtable login again")?;
        if status.is_success() {
            let grant: Grant = serde_json::from_value(data)?;
            let saved = store.save(
                Credential {
                    device_id: grant.device_id.clone(),
                    account_id: grant.account_id,
                    server: server.to_string(),
                    access_token: None,
                },
                &grant.access_token,
                file_storage,
            );
            if let Err(error) = saved {
                let revoked = client
                    .post(endpoint(server, "/auth/device/logout"))
                    .bearer_auth(&grant.access_token)
                    .send();
                if !revoked.is_ok_and(|r| r.status().is_success()) {
                    eprintln!(
                        "Credential could not be saved or revoked. Revoke this machine at {server}cli/devices."
                    );
                }
                return Err(error);
            }
            println!(
                "Logged in. Connected \"{}\" to {server}\nMachine ID: {}",
                safe(&grant.device_name),
                safe(&grant.device_id)
            );
            if file_storage {
                println!("Credential stored in a private plaintext file (--insecure-storage).");
            }
            return Ok(());
        }
        match data.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => (),
            Some("slow_down") => interval += 5,
            Some("access_denied") => bail!("Login was denied in the browser."),
            Some("expired_token") => {
                bail!("Login expired or was already completed. Run roundtable login again.")
            }
            _ => bail!(
                "Login failed (HTTP {}). Run roundtable login again.",
                status.as_u16()
            ),
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Commands::Init {
        dir,
        template,
        project,
        force,
    } = &cli.command
    {
        return init::run(&cli.server, dir, template, project.as_deref(), *force);
    }
    let root = cli
        .config_dir
        .or_else(|| directories::BaseDirs::new().map(|d| d.config_dir().join("roundtable")))
        .context("Cannot find a config directory; supply --config-dir")?;
    state::private_dir(&root)?;
    let agent_root = state::agent_root(&root, cli.server.as_str());
    match cli.command {
        Commands::Receipts { command } => return receipt_command(&agent_root, command),
        Commands::Daemon => return daemon::run(agent_root, root, cli.server),
        Commands::Service { action } => {
            return service::manage(&action, &agent_root, &root, &cli.server);
        }
        Commands::Setup {
            content,
            allow_control,
        } => {
            configure(
                &state::State::open(agent_root.clone())?,
                content,
                false,
                allow_control,
                false,
                None,
            )?;
            service::manage("install", &agent_root, &root, &cli.server)?;
            return service::manage("start", &agent_root, &root, &cli.server);
        }
        Commands::Monitor {
            content,
            metadata_only,
            allow_control,
            deny_control,
            updates,
        } => {
            return configure(
                &state::State::open(agent_root)?,
                content,
                metadata_only,
                allow_control,
                deny_control,
                updates,
            );
        }
        Commands::Sessions { command } => return session_command(&agent_root, command),
        _ => {}
    }
    let store = Store::open(root, cli.server.as_str())?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .pool_idle_timeout(Duration::from_secs(3))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("roundtable-cli/", env!("CARGO_PKG_VERSION")))
        .build()?;
    match cli.command {
        Commands::Login {
            name,
            no_browser,
            insecure_storage,
        } => login(
            &client,
            &cli.server,
            &store,
            name,
            no_browser,
            insecure_storage,
        ),
        Commands::Status { json: as_json } => {
            let (_, token) = store
                .load()?
                .context("Not logged in. Run roundtable login.")?;
            let info = response(
                client
                    .get(endpoint(&cli.server, "/auth/device/session"))
                    .bearer_auth(token)
                    .send()?,
            )?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!(
                    "Connected to {}\nMachine: {}\nMachine ID: {}\nAccount: {}",
                    cli.server,
                    safe(info["name"].as_str().unwrap_or("unknown")),
                    safe(info["id"].as_str().unwrap_or("unknown")),
                    safe(info["account_id"].as_str().unwrap_or("unknown"))
                );
            }
            Ok(())
        }
        Commands::Logout => {
            let Some((credential, token)) = store.load()? else {
                println!("Already logged out.");
                return Ok(());
            };
            let result = client
                .post(endpoint(&cli.server, "/auth/device/logout"))
                .bearer_auth(token)
                .send()
                .context("Could not revoke login; credentials were kept so you can retry logout")?;
            if result.status() != StatusCode::UNAUTHORIZED {
                response(result)?;
            }
            store.remove(&credential)?;
            println!("Logged out. Machine access revoked.");
            Ok(())
        }
        _ => unreachable!("Service commands are handled before opening credentials"),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_only_travel_to_explicit_secure_origins() {
        for valid in [
            "https://roundtable.bot",
            "http://localhost:8000",
            "http://127.0.0.1:8000",
            "http://[::1]:8000",
        ] {
            assert!(server_url(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "http://roundtable.bot",
            "https://user:secret@roundtable.bot",
            "https://roundtable.bot/path",
            "https://roundtable.bot?redirect=evil",
            "file:///tmp/a",
        ] {
            assert!(server_url(invalid).is_err(), "{invalid}");
        }
    }
}
