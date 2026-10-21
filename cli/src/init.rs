use anyhow::{Context, Result, bail, ensure};
use reqwest::{StatusCode, Url, blocking::Client};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

include!(concat!(env!("OUT_DIR"), "/templates.rs"));
static CANCELLED: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize)]
struct Manifest {
    track: String,
    title: String,
    bots: Vec<Bot>,
    seed: Vec<Seed>,
    next_steps: Vec<String>,
}

#[derive(Deserialize)]
struct Bot {
    name: String,
    role: Role,
    env: String,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    Researcher,
    Reviewer,
    Writer,
    Coordinator,
}

#[derive(Deserialize)]
struct Seed {
    from: String,
    to: String,
}

struct Template<'a> {
    manifest: Manifest,
    assets: Vec<(&'a str, &'a [u8])>,
}

fn relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.contains(['\\', '\0', '\r', '\n'])
            && Path::new(path)
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "Template paths must be relative and cannot contain traversal"
    );
    Ok(())
}

impl<'a> Template<'a> {
    fn load(track: &str, embedded: &[(&str, &'a str, &'a [u8])]) -> Result<Self> {
        let assets: Vec<_> = embedded
            .iter()
            .filter(|(name, _, _)| *name == track)
            .map(|(_, path, data)| (*path, *data))
            .collect();
        let manifest = assets.iter().find(|(path, _)| *path == "template.json")
            .with_context(|| format!("Template '{track}' is not embedded in this CLI. Rebuild from a checkout containing templates/{track}/template.json, or install a release containing that template."))?;
        let manifest: Manifest =
            serde_json::from_slice(manifest.1).context("Invalid embedded template manifest")?;
        ensure!(
            manifest.track == track && !manifest.title.trim().is_empty(),
            "Invalid template track or title"
        );
        ensure!(
            !manifest.bots.is_empty() && manifest.bots.len() <= 100,
            "Template must contain 1–100 bots"
        );
        ensure!(
            assets.len() < 2000,
            "Template must contain fewer than 2,000 files"
        );
        let mut variables: HashSet<&str> =
            ["ROUNDTABLE_KEY", "ROUNDTABLE_URL", "ROUNDTABLE_PROJECT"].into();
        let mut names = HashSet::new();
        for bot in &manifest.bots {
            ensure!(
                !bot.name.trim().is_empty()
                    && bot.name.chars().count() <= 80
                    && !bot.name.chars().any(char::is_control),
                "Invalid template bot name"
            );
            ensure!(names.insert(&bot.name), "Duplicate template bot name");
            let stem = bot
                .env
                .strip_suffix("_KEY")
                .context("Bot env must end in _KEY")?;
            ensure!(
                !stem.is_empty()
                    && bot
                        .env
                        .bytes()
                        .next()
                        .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
                    && bot
                        .env
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
                "Invalid bot environment variable"
            );
            ensure!(
                variables.insert(&bot.env),
                "Duplicate or reserved bot environment variable"
            );
        }
        let template = Self { manifest, assets };
        for seed in &template.manifest.seed {
            relative(&seed.from)?;
            relative(&seed.to)?;
            ensure!(
                seed.from.starts_with("seed/"),
                "Seed source must be under seed/"
            );
            let text = std::str::from_utf8(template.asset(&seed.from)?)
                .context("Seed must be UTF-8 text")?;
            ensure!(
                text.chars().count() < 200_000,
                "Seed must contain fewer than 200,000 characters"
            );
        }
        ensure!(
            template.files().next().is_some(),
            "Template is incomplete: files/ is missing or empty"
        );
        for (path, _) in template.files() {
            relative(path)?;
            ensure!(
                path != ".env"
                    && !Path::new(path)
                        .components()
                        .any(|p| p.as_os_str() == ".git"),
                "Template cannot write .env or .git"
            );
        }
        if let Some((_, data)) = template.files().find(|(path, _)| *path == ".gitignore") {
            std::str::from_utf8(data).context("Template .gitignore must be UTF-8")?;
        }
        Ok(template)
    }

    fn asset(&self, path: &str) -> Result<&'a [u8]> {
        self.assets.iter().find(|(name, _)| *name == path).map(|(_, data)| *data)
            .with_context(|| format!("Template is incomplete: missing {path}; rebuild with the complete templates/ directory"))
    }

    fn files(&self) -> impl Iterator<Item = (&'a str, &'a [u8])> + '_ {
        self.assets
            .iter()
            .filter_map(|(path, data)| path.strip_prefix("files/").map(|path| (path, *data)))
    }
}

// Read from the controlling terminal rather than stdin (which may be a pipe).
// The guard restores echo on errors, cancellation, and unwinding.
fn team_key() -> Result<String> {
    if let Some(key) = std::env::var_os("ROUNDTABLE_KEY") {
        let key = key
            .into_string()
            .map_err(|_| anyhow::anyhow!("ROUNDTABLE_KEY must be UTF-8"))?;
        ensure!(!key.trim().is_empty(), "ROUNDTABLE_KEY is empty");
        return Ok(key);
    }
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("No terminal for a hidden prompt; set ROUNDTABLE_KEY")?;
    struct Terminal(File, libc::termios);
    impl Drop for Terminal {
        fn drop(&mut self) {
            unsafe {
                libc::tcsetattr(self.0.as_raw_fd(), libc::TCSAFLUSH, &self.1);
            }
            let _ = self.0.write_all(b"\n");
        }
    }
    let mut original = std::mem::MaybeUninit::uninit();
    ensure!(
        unsafe { libc::tcgetattr(tty.as_raw_fd(), original.as_mut_ptr()) } == 0,
        "Cannot read terminal settings; set ROUNDTABLE_KEY"
    );
    let original = unsafe { original.assume_init() };
    let guard = Terminal(tty.try_clone()?, original);
    ctrlc::set_handler(|| CANCELLED.store(true, Ordering::SeqCst))?;
    let mut hidden = original;
    hidden.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON);
    hidden.c_cc[libc::VMIN] = 0;
    hidden.c_cc[libc::VTIME] = 1;
    ensure!(
        unsafe { libc::tcsetattr(tty.as_raw_fd(), libc::TCSAFLUSH, &hidden) } == 0,
        "Cannot hide terminal input; set ROUNDTABLE_KEY"
    );
    tty.write_all(b"Team key (hidden): ")?;
    tty.flush()?;
    let mut bytes = Vec::new();
    loop {
        ensure!(
            !CANCELLED.load(Ordering::SeqCst),
            "Team key prompt cancelled"
        );
        let mut byte = [0];
        match tty.read(&mut byte) {
            Ok(0) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
            Ok(_) => match byte[0] {
                b'\n' | b'\r' => break,
                3 | 4 => bail!("Team key prompt cancelled"),
                8 | 127 => {
                    bytes.pop();
                }
                b => bytes.push(b),
            },
        }
        ensure!(bytes.len() <= 8192, "Team key is too long");
    }
    drop(guard);
    let key = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("Team key must be UTF-8"))?;
    ensure!(!key.trim().is_empty(), "Team key is empty");
    Ok(key)
}

struct Api<'a> {
    client: Client,
    server: &'a Url,
    key: &'a str,
}

impl Api<'_> {
    fn call(&self, tool: &str, args: Value) -> Result<(StatusCode, Value)> {
        ensure!(
            !CANCELLED.load(Ordering::SeqCst),
            "Initialization cancelled"
        );
        let response = self
            .client
            .post(self.server.join(&format!("/api/tools/{tool}"))?)
            .bearer_auth(self.key)
            .json(&args)
            .send()
            .map_err(|_| {
                anyhow::anyhow!("Could not call {tool}; check the server URL and connection")
            })?;
        let status = response.status();
        let mut body = Vec::new();
        response
            .take(2_000_001)
            .read_to_end(&mut body)
            .map_err(|_| anyhow::anyhow!("Could not read {tool} response"))?;
        ensure!(
            body.len() <= 2_000_000,
            "{tool} response exceeds the size limit"
        );
        let data = serde_json::from_slice(&body).map_err(|_| {
            anyhow::anyhow!("{tool} returned invalid JSON (HTTP {})", status.as_u16())
        })?;
        Ok((status, data))
    }

    fn success(&self, tool: &str, status: StatusCode, data: Value) -> Result<Value> {
        if !status.is_success() {
            // Only the error field is printable; redact even if the server echoes the key.
            let message = data["error"]
                .as_str()
                .unwrap_or("request failed")
                .replace(self.key, "[redacted]");
            bail!(
                "{tool} returned HTTP {}: {}",
                status.as_u16(),
                crate::safe(&message)
            );
        }
        Ok(data)
    }

    fn bank(&self, requested: Option<&str>) -> Result<String> {
        let (status, data) = self.call("list_banks", json!({}))?;
        let data = self.success("list_banks", status, data)?;
        let banks = data
            .as_array()
            .context("list_banks returned an invalid bank list")?;
        let writable: Vec<_> = banks
            .iter()
            .filter(|bank| bank["access"] == "write")
            .filter_map(|bank| bank["bank"].as_str())
            .collect();
        if let Some(bank) = requested {
            ensure!(
                writable.contains(&bank),
                "Project '{}' is not a writable bank. Choose a writable bank with --project, or create a team assistant in the dashboard: Agents → Connect agent → Assistant",
                crate::safe(bank)
            );
            return Ok(bank.into());
        }
        let candidates: Vec<_> = writable
            .into_iter()
            .filter(|bank| *bank != "shared-notes")
            .collect();
        match candidates.as_slice() {
            [bank] => Ok((*bank).into()),
            [] => bail!(
                "No writable team bank. Create a team assistant in the dashboard (Agents → Connect agent → Assistant; a coding agent has no bank), then use its key (or pass --project BANK for a writable bank)."
            ),
            _ => bail!(
                "Several writable team banks: {}. Pass --project BANK to choose one.",
                candidates
                    .iter()
                    .map(|bank| crate::safe(bank))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// Create one bot and return its (name, key, agent id).
    fn mint(&self, bot: &Bot, bank: &str) -> Result<(String, String, String)> {
        for attempt in 1..=100 {
            let suffix = if attempt == 1 {
                String::new()
            } else {
                format!("-{attempt}")
            };
            let name = format!(
                "{}{}",
                bot.name.chars().take(80 - suffix.len()).collect::<String>(),
                suffix
            );
            let (status, data) = self.call(
                "create_subagent",
                json!({
                    "request_id": uuid::Uuid::new_v4().to_string(), "name": name,
                    "role": bot.role, "client": "custom-agent", "access": {bank: "write"}
                }),
            )?;
            if status == StatusCode::CONFLICT
                && data["error"].as_str().is_some_and(|error| {
                    let error = error.to_ascii_lowercase();
                    error.contains("already exists")
                        || error.contains("name") && error.contains("conflict")
                })
            {
                // A fresh name is a fresh mutation, so each attempt has its own request id.
                eprintln!(
                    "Bot name '{}' conflicts; trying another suffix.",
                    crate::safe(&name)
                );
                continue;
            }
            let data = self.success("create_subagent", status, data)?;
            let key = data["token"]
                .as_str()
                .filter(|key| !key.is_empty())
                .context(
                    "create_subagent did not return a bot key; manage the created bot in Agents",
                )?;
            let actual = data["assistant"]["name"].as_str().unwrap_or(&name);
            let id = data["assistant"]["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .context(
                    "create_subagent did not return a bot id; manage the created bot in Agents",
                )?;
            if attempt > 1 {
                println!("Using bot name '{}'.", crate::safe(actual));
            }
            return Ok((actual.into(), key.into(), id.into()));
        }
        bail!("Too many bot name conflicts; rename or revoke old bots in Agents and retry")
    }

    fn seed(&self, template: &Template<'_>, bank: &str) -> Result<()> {
        for seed in &template.manifest.seed {
            let (status, data) = self.call("write_file", json!({
                "request_id": uuid::Uuid::new_v4().to_string(), "bank": bank,
                "path": seed.to, "content": std::str::from_utf8(template.asset(&seed.from)?)?,
                "message": format!("Initialize {} template", template.manifest.track), "create_only": true
            }))?;
            if status == StatusCode::CONFLICT
                && data["error"]
                    .as_str()
                    .is_some_and(|error| error.to_ascii_lowercase().contains("already exists"))
            {
                println!("Skipped existing seed file '{}'.", crate::safe(&seed.to));
            } else {
                self.success("write_file", status, data)?;
            }
        }
        Ok(())
    }
}

fn check_target(root: &Path, relative: &str) -> Result<()> {
    let mut path = root.to_path_buf();
    let parts: Vec<_> = Path::new(relative).components().collect();
    for (index, part) in parts.iter().enumerate() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) => ensure!(
                if index + 1 == parts.len() {
                    meta.is_file()
                } else {
                    meta.is_dir()
                },
                "Refusing symlink or incompatible project path: {}",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn destination(dir: &Path, template: &Template<'_>, force: bool) -> Result<PathBuf> {
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            ensure!(
                meta.is_dir(),
                "Project destination must be a directory, not a symlink"
            );
            ensure!(
                force || fs::read_dir(dir)?.next().is_none(),
                "Project directory is not empty; choose an empty directory or pass --force to replace project files"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(dir)?,
        Err(error) => return Err(error.into()),
    }
    let root = fs::canonicalize(dir)?;
    for path in template
        .files()
        .map(|(path, _)| path)
        .chain([".env", ".gitignore"])
    {
        check_target(&root, path)?;
    }
    if root.join(".gitignore").exists() {
        fs::read_to_string(root.join(".gitignore")).context("Cannot read project .gitignore")?;
    }
    Ok(root)
}

fn env_value(value: &str) -> Result<String> {
    ensure!(
        !value.is_empty() && !value.chars().any(char::is_control) && !value.contains(['\'', '\\']),
        "Server returned a value that cannot safely be stored in .env"
    );
    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_-.:/%[]".contains(&b))
    {
        Ok(value.into())
    } else {
        Ok(format!("'{value}'"))
    }
}

/// Bots are (manifest entry, actual name, key, agent id). Templates dispatch by `*_ID`, since
/// names are only unique per parent agent while discovery lists the whole account.
fn env_content(
    server: &Url,
    bank: &str,
    bots: &[(&Bot, String, String, String)],
) -> Result<String> {
    let mut output = format!(
        "ROUNDTABLE_URL={}\nROUNDTABLE_PROJECT={}\n",
        env_value(server.as_str().trim_end_matches('/'))?,
        env_value(bank)?
    );
    for (bot, name, key, id) in bots {
        let prefix = bot.env.strip_suffix("_KEY").unwrap();
        output.push_str(&format!(
            "{}={}\n{prefix}_NAME={}\n{prefix}_ID={}\n",
            bot.env,
            env_value(key)?,
            env_value(name)?,
            env_value(id)?
        ));
    }
    Ok(output)
}

fn ignored_env(mut content: String) -> String {
    // Put the rule last so an earlier negation cannot expose the generated credentials.
    if content.lines().last() != Some(".env") {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(".env\n");
    }
    content
}

fn write_atomic(root: &Path, path: &str, bytes: &[u8], force: bool, mode: u32) -> Result<()> {
    check_target(root, path)?;
    let target = root.join(path);
    fs::create_dir_all(target.parent().unwrap())?;
    let mut staged = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    if force {
        staged.persist(&target).map_err(|error| error.error)?;
    } else {
        staged
            .persist_noclobber(&target)
            .map_err(|error| error.error)?;
    }
    Ok(())
}

fn write_project(root: &Path, template: &Template<'_>, env: &str, force: bool) -> Result<()> {
    let ignore_path = root.join(".gitignore");
    let ignore_exists = ignore_path.exists();
    ensure!(
        !ignore_exists || force,
        "Project .gitignore already exists; pass --force to update project files"
    );
    let mut ignore = if ignore_exists {
        fs::read_to_string(&ignore_path)?
    } else {
        String::new()
    };
    if let Some((_, data)) = template.files().find(|(path, _)| *path == ".gitignore") {
        if !ignore.is_empty() && !ignore.ends_with('\n') {
            ignore.push('\n');
        }
        ignore.push_str(std::str::from_utf8(data).context("Template .gitignore must be UTF-8")?);
    }
    write_atomic(
        root,
        ".gitignore",
        ignored_env(ignore).as_bytes(),
        ignore_exists,
        0o644,
    )?;
    for (path, bytes) in template.files().filter(|(path, _)| *path != ".gitignore") {
        write_atomic(root, path, bytes, force, 0o644)?;
    }
    write_atomic(root, ".env", env.as_bytes(), force, 0o600)
}

fn provision(
    api: &Api<'_>,
    root: &Path,
    template: &Template<'_>,
    project: Option<&str>,
    force: bool,
) -> Result<String> {
    let bank = api.bank(project)?;
    let mut bots = Vec::new();
    let provision = (|| -> Result<()> {
        for bot in &template.manifest.bots {
            let (name, key, id) = api.mint(bot, &bank)?;
            bots.push((bot, name, key, id));
        }
        api.seed(template, &bank)?;
        ensure!(
            !CANCELLED.load(Ordering::SeqCst),
            "Initialization cancelled"
        );
        write_project(
            root,
            template,
            &env_content(api.server, &bank, &bots)?,
            force,
        )
    })();
    provision.with_context(|| format!("If any bots were created before this failure, revoke unused bots at {}#/agents before retrying", api.server))?;
    Ok(bank)
}

/// Bot names recorded in an existing project `.env`, whose keys `--force` will replace.
fn previous_bots(root: &Path) -> Vec<String> {
    let path = root.join(".env");
    if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        return Vec::new();
    }
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(name, value)| name.ends_with("_NAME") && !value.is_empty())
        .map(|(_, value)| value.to_owned())
        .collect()
}

pub fn run(
    server: &Url,
    dir: &Path,
    track: &str,
    project: Option<&str>,
    force: bool,
) -> Result<()> {
    let template = Template::load(track, EMBEDDED)?;
    let root = destination(dir, &template, force)?;
    let key = team_key()?;
    let api = Api {
        client: Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("roundtable-cli/", env!("CARGO_PKG_VERSION")))
            .build()?,
        server,
        key: &key,
    };
    let replaced = if force {
        previous_bots(&root)
    } else {
        Vec::new()
    };
    let bank = provision(&api, &root, &template, project, force)?;
    if !replaced.is_empty() {
        eprintln!(
            "Replaced the previous .env. These bots from it still have active keys; revoke any you no longer use at {server}#/agents: {}",
            replaced
                .iter()
                .map(|name| crate::safe(name))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!(
        "Created {} in {} (project {}).",
        crate::safe(&template.manifest.title),
        root.display(),
        crate::safe(&bank)
    );
    println!("Next steps:\n  cd {}", dir.display());
    for step in &template.manifest.next_steps {
        println!("  {step}");
    }
    println!("Agents: {server}#/agents");
    Ok(())
}

#[cfg(test)]
mod tests;
