//! Self-update. The Roundtable server names the release and each binary's SHA-256; the binary
//! itself comes from the public GitHub release, so neither source alone can change the code.
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const RELEASES: &str = "https://github.com/roundtable-bot/roundtable-cli/releases/download/";
// Kept beside the service binary, whose directory is private to the user.
const STAGED: &str = ".roundtable-update";
const PREVIOUS: &str = ".roundtable-previous";
const STARTS: &str = ".roundtable-previous-starts";

fn target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

pub fn version(v: &str) -> Option<[u64; 3]> {
    let parts: Vec<u64> = v
        .split('.')
        .map(|p| {
            (!p.is_empty() && p.len() < 10 && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse().ok())
                .flatten()
        })
        .collect::<Option<_>>()?;
    parts.try_into().ok()
}

/// The release to install: strictly newer than this binary (never a downgrade) and digested
/// for this platform.
pub fn offered(release: &Value, current: &str) -> Option<(String, String)> {
    let next = release["version"].as_str()?;
    let digest = release["sha256"][target()?].as_str()?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    (version(next)? > version(current)?).then(|| (next.into(), digest.into()))
}

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("roundtable-cli/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Download, verify, unpack and run `--version` on a release beside `exe`.
pub fn stage(client: &Client, base: &str, next: &str, digest: &str, exe: &Path) -> Result<PathBuf> {
    const LIMIT: u64 = 64 << 20;
    let url = format!(
        "{base}v{next}/roundtable-{}.gz",
        target().context("Unsupported platform")?
    );
    let mut packed = vec![];
    client
        .get(url)
        .send()?
        .error_for_status()?
        .take(LIMIT + 1)
        .read_to_end(&mut packed)?;
    if packed.len() as u64 > LIMIT {
        bail!("Release asset exceeds the size limit");
    }
    if format!("{:x}", Sha256::digest(&packed)) != digest {
        bail!("Release checksum mismatch");
    }
    let dir = exe.parent().context("Service binary has no directory")?;
    let staged = tempfile::Builder::new().prefix(STAGED).tempfile_in(dir)?;
    // ponytail: system gzip instead of a decompression crate; present on every supported OS.
    let mut gzip = Command::new("gzip")
        .arg("-dc")
        .stdin(Stdio::piped())
        .stdout(staged.reopen()?)
        .spawn()?;
    gzip.stdin.take().unwrap().write_all(&packed)?;
    if !gzip.wait()?.success() {
        bail!("Release asset is not a valid gzip file");
    }
    // Close the writable handle before executing it (Linux refuses busy text files).
    let staged = staged.into_temp_path();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o700))?;
    // Another thread's fork may briefly hold the file open for writing (ETXTBSY); retry.
    let mut attempts = 0;
    let mut child = loop {
        match Command::new(&staged)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 40 =>
            {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            result => break result?,
        }
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Downloaded release did not report its version");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output()?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != format!("roundtable {next}")
    {
        bail!("Downloaded release does not run as version {next}");
    }
    // One fixed name, so an update abandoned at shutdown never accumulates copies.
    let path = exe.with_file_name(STAGED);
    staged.persist(&path)?;
    Ok(path)
}

/// Count starts of a just-installed release; true once it has failed to sync three times.
fn rollback_due(exe: &Path) -> Result<bool> {
    let _ = fs::remove_file(exe.with_file_name(STAGED));
    if !exe.with_file_name(PREVIOUS).exists() {
        return Ok(false);
    }
    let path = exe.with_file_name(STARTS);
    let starts = fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
        + 1;
    fs::write(&path, starts.to_string())?;
    Ok(starts > 3)
}

/// At service start: restore the previous binary if the installed release keeps failing.
// ponytail: the restored release retries the same update after its six-hour back-off.
pub fn startup(exe: &Path) -> Result<()> {
    if !rollback_due(exe)? {
        return Ok(());
    }
    eprintln!(
        "The updated release failed to connect after three starts; restoring the previous one."
    );
    let _ = fs::remove_file(exe.with_file_name(STARTS));
    fs::rename(exe.with_file_name(PREVIOUS), exe)?;
    Err(Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec()
        .into())
}

/// The running release synced successfully, so the previous one is no longer needed.
pub fn confirmed(exe: &Path) {
    let _ = fs::remove_file(exe.with_file_name(PREVIOUS));
    let _ = fs::remove_file(exe.with_file_name(STARTS));
}

/// Replace the running binary and restart in place with the same arguments and process ID.
pub fn apply(staged: &Path, exe: &Path) -> Result<()> {
    // ponytail: replaced in place, so a service binary keeps its installed file name
    // (e.g. roundtable-0.2.12) after updating; `service install` rewrites it when run again.
    let previous = exe.with_file_name(PREVIOUS);
    let _ = fs::remove_file(&previous);
    fs::hard_link(exe, &previous)?;
    let _ = fs::remove_file(exe.with_file_name(STARTS));
    fs::rename(staged, exe)?;
    Err(Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec()
        .into())
}

/// `roundtable update`: install the server's current release over this binary, then run the
/// background service from it.
pub fn run(
    api: &Client,
    server: &reqwest::Url,
    root: &Path,
    config: &Path,
    force: bool,
) -> Result<()> {
    let release: Value = api
        .get(server.join("/cli/release")?)
        .send()?
        .error_for_status()
        .context("The server did not describe its current CLI release")?
        .json()?;
    let exe = fs::canonicalize(std::env::current_exe()?)?;
    let installed = crate::service::installed(root)?;
    let step = |action: &str| -> Result<()> {
        // The new binary writes the service definition, so the service runs that version.
        let status = Command::new(&exe)
            .arg("--server")
            .arg(server.as_str())
            .arg("--config-dir")
            .arg(config)
            .args(["service", action])
            .status()?;
        if !status.success() {
            bail!("roundtable service {action} failed; run it by hand");
        }
        Ok(())
    };
    let message = perform(
        &client()?,
        &release,
        RELEASES,
        &exe,
        root,
        installed,
        force,
        step,
    )?;
    println!("{message}");
    Ok(())
}

/// Install the offered release over `exe` and run an installed service from this version.
/// Refuses while Roundtable-managed sessions run unless forced, since restarting ends them.
#[allow(clippy::too_many_arguments)]
pub fn perform(
    download: &Client,
    release: &Value,
    base: &str,
    exe: &Path,
    root: &Path,
    installed: bool,
    force: bool,
    mut service: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let current = env!("CARGO_PKG_VERSION");
    let status = serde_json::json!({"op":"status"});
    let daemon = crate::daemon::request(root, &status).ok();
    // A service on another version (or one too old to say) is restarted from this binary.
    let stale_service = installed && daemon.as_ref().is_some_and(|d| d["version"] != current);
    let offer = offered(release, current);
    if offer.is_none() && !stale_service {
        return Ok(format!("Roundtable {current} is up to date"));
    }
    let managed_live = |running: bool| -> Result<usize> {
        if !running || force {
            return Ok(0);
        }
        let sessions = crate::daemon::request(root, &serde_json::json!({"op":"list"}))?;
        Ok(sessions["sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["source"] == "managed" && s["live"] == true)
            .count())
    };
    let managed = managed_live(daemon.is_some())?;
    if managed > 0 {
        bail!(
            "{managed} Roundtable-managed session(s) are running; updating restarts the service and ends them. Finish them first, or rerun with --force."
        );
    }
    let outcome = match &offer {
        Some((next, digest)) => {
            let staged = stage(download, base, next, digest, exe).with_context(|| {
                format!(
                    "Could not stage the update beside {}; the directory must be writable by you",
                    exe.display()
                )
            })?;
            // Keep the installed file's mode, for example a shared 0755 npm install.
            if let Ok(existing) = fs::metadata(exe) {
                fs::set_permissions(&staged, existing.permissions())?;
            }
            fs::rename(&staged, exe)?;
            format!("Updated Roundtable {current} → {next}")
        }
        None => format!("Roundtable {current} is up to date"),
    };
    if !installed {
        return Ok(format!("{outcome}. No background service is installed."));
    }
    // The download can take minutes; a session started meanwhile must not be ended.
    let running = crate::daemon::request(root, &status).is_ok();
    let managed = managed_live(running)?;
    if managed > 0 {
        bail!(
            "{outcome}, but {managed} Roundtable-managed session(s) started meanwhile, so the service still runs its previous version. Finish them and run roundtable update again, or rerun with --force."
        );
    }
    if running {
        service("stop")?;
        // The service manager returns before the daemon has exited.
        let deadline = Instant::now() + Duration::from_secs(10);
        while crate::daemon::request(root, &status).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    if let Err(error) = service("install").and_then(|()| service("start")) {
        // The previous definition and binary are still installed unless install got that far.
        let recovered = if service("start").is_ok() {
            "The previously installed service was started again."
        } else {
            "The service is stopped."
        };
        bail!(
            "{outcome}, but the service could not be reinstalled: {error:#}. {recovered} Finish by hand: roundtable service stop; roundtable service install; roundtable service start."
        );
    }
    Ok(format!("{outcome}; service restarted."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::net::TcpListener;
    use std::rc::Rc;

    fn release(next: &str, digest: &str) -> Value {
        json!({"version": next, "sha256": {target().unwrap(): digest}})
    }

    #[test]
    fn only_newer_releases_with_a_platform_digest_are_offered() {
        let digest = "a".repeat(64);
        assert_eq!(
            offered(&release("0.2.12", &digest), "0.2.11"),
            Some(("0.2.12".into(), digest.clone()))
        );
        assert_eq!(
            offered(&release("0.10.0", &digest), "0.9.9").unwrap().0,
            "0.10.0"
        );
        for (next, current) in [
            ("0.2.11", "0.2.11"),
            ("0.2.10", "0.2.11"),
            ("0.2", "0.1.0"),
            ("0.2.x", "0.1.0"),
            ("../1.0.0", "0.1.0"),
        ] {
            assert_eq!(offered(&release(next, &digest), current), None, "{next}");
        }
        assert_eq!(offered(&release("0.2.12", &"A".repeat(64)), "0.2.11"), None);
        assert_eq!(
            offered(&json!({"version": "0.2.12", "sha256": {}}), "0.2.11"),
            None
        );
    }

    fn serve(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        base
    }

    #[test]
    fn staging_verifies_digest_and_the_binary_version() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let exe = tmp.path().join("roundtable");
        let script = |v: &str| format!("#!/bin/sh\necho roundtable {v}\n");
        let gzip = |text: String| -> Result<Vec<u8>> {
            let mut child = Command::new("gzip")
                .arg("-c")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            child.stdin.take().unwrap().write_all(text.as_bytes())?;
            Ok(child.wait_with_output()?.stdout)
        };
        let local = Client::new();
        let good = gzip(script("9.9.9"))?;
        let digest = format!("{:x}", Sha256::digest(&good));

        let staged = stage(&local, &serve(good.clone()), "9.9.9", &digest, &exe)?;
        assert_eq!(staged.parent(), Some(tmp.path()));
        let error = stage(&local, &serve(good), "9.9.9", &"0".repeat(64), &exe).unwrap_err();
        assert!(error.to_string().contains("checksum"));
        let wrong = gzip(script("1.0.0"))?;
        let digest_wrong = format!("{:x}", Sha256::digest(&wrong));
        assert!(stage(&local, &serve(wrong), "9.9.9", &digest_wrong, &exe).is_err());
        // Only the verified binary is left behind.
        assert_eq!(fs::read_dir(tmp.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn a_release_that_never_syncs_is_rolled_back_on_the_fourth_start() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let exe = tmp.path().join("roundtable");
        fs::write(&exe, "new")?;
        fs::write(tmp.path().join(STAGED), "abandoned")?;
        assert!(!rollback_due(&exe)?);
        assert!(!tmp.path().join(STAGED).exists());
        fs::write(tmp.path().join(PREVIOUS), "old")?;
        assert_eq!(
            (0..4)
                .map(|_| rollback_due(&exe))
                .collect::<Result<Vec<_>>>()?,
            [false, false, false, true]
        );
        confirmed(&exe);
        assert!(!rollback_due(&exe)?);
        assert_eq!(fs::read_dir(tmp.path())?.count(), 1);
        Ok(())
    }

    fn gzip(text: String) -> Result<Vec<u8>> {
        let mut child = Command::new("gzip")
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        child.stdin.take().unwrap().write_all(text.as_bytes())?;
        Ok(child.wait_with_output()?.stdout)
    }
    fn release_of(packed: &[u8]) -> Value {
        release("9.9.9", &format!("{:x}", Sha256::digest(packed)))
    }

    #[test]
    fn update_command_reports_an_up_to_date_binary() -> Result<()> {
        let tmp = tempfile::tempdir_in("/tmp")?;
        let current = env!("CARGO_PKG_VERSION");
        let message = perform(
            &Client::new(),
            &release(current, &"a".repeat(64)),
            "http://127.0.0.1:1/",
            &tmp.path().join("roundtable"),
            &tmp.path().join("state"),
            true,
            false,
            |_| panic!("no service step for an up-to-date binary"),
        )?;
        assert_eq!(message, format!("Roundtable {current} is up to date"));
        Ok(())
    }

    #[test]
    fn update_command_replaces_the_binary_and_reinstalls_an_installed_service() -> Result<()> {
        let tmp = tempfile::tempdir_in("/tmp")?;
        let exe = tmp.path().join("roundtable");
        fs::write(&exe, "old binary")?;
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755))?;
        let packed = gzip("#!/bin/sh\necho roundtable 9.9.9\n".into())?;
        let mut steps = vec![];
        let message = perform(
            &Client::new(),
            &release_of(&packed),
            &serve(packed.clone()),
            &exe,
            &tmp.path().join("state"),
            true,
            false,
            |action| {
                steps.push(action.to_owned());
                Ok(())
            },
        )?;
        assert_eq!(
            message,
            format!(
                "Updated Roundtable {} → 9.9.9; service restarted.",
                env!("CARGO_PKG_VERSION")
            )
        );
        // Not running, so no stop; the new binary writes the service definition.
        assert_eq!(steps, ["install", "start"]);
        assert!(fs::read_to_string(&exe)?.contains("echo roundtable 9.9.9"));
        // A shared install keeps its mode instead of the staged file's private 0700.
        assert_eq!(fs::metadata(&exe)?.permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::read_dir(tmp.path())?.count(), 1);
        fs::write(&exe, "old binary")?;
        let message = perform(
            &Client::new(),
            &release_of(&packed),
            &serve(packed),
            &exe,
            &tmp.path().join("state"),
            false,
            false,
            |_| panic!("no service is installed"),
        )?;
        assert!(
            message.ends_with("No background service is installed."),
            "{message}"
        );
        Ok(())
    }

    /// A fake running daemon answering `status` and `list`; `managed` gives the live managed
    /// session count for each successive `list` call (the last value repeats).
    fn fake_daemon(root: &Path, version: Option<&str>, managed: Vec<usize>) -> Result<()> {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;
        fs::create_dir_all(root)?;
        let listener = UnixListener::bind(root.join("agent.sock"))?;
        let version = version.map(str::to_owned);
        std::thread::spawn(move || {
            let mut lists = 0;
            while let Ok((mut stream, _)) = listener.accept() {
                let mut line = String::new();
                if BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .is_err()
                {
                    break;
                }
                let request: Value = serde_json::from_str(&line).unwrap_or_default();
                let reply = if request["op"] == "list" {
                    let count = managed[lists.min(managed.len() - 1)];
                    lists += 1;
                    let sessions: Vec<Value> = (0..count)
                        .map(|i| json!({"id":format!("codex:{i}"),"source":"managed","live":true}))
                        .collect();
                    json!({"sessions":sessions})
                } else {
                    let mut status = json!({"running":true});
                    if let Some(version) = &version {
                        status["version"] = json!(version);
                    }
                    status
                };
                let _ = writeln!(stream, "{reply}");
            }
        });
        Ok(())
    }
    fn stop_removes_socket(
        root: &Path,
        steps: &Rc<RefCell<Vec<String>>>,
    ) -> impl FnMut(&str) -> Result<()> {
        let root = root.to_owned();
        let steps = steps.clone();
        move |action| {
            if action == "stop" {
                fs::remove_file(root.join("agent.sock"))?;
            }
            steps.borrow_mut().push(action.to_owned());
            Ok(())
        }
    }

    #[test]
    fn update_command_refuses_while_managed_sessions_run_unless_forced() -> Result<()> {
        let tmp = tempfile::tempdir_in("/tmp")?;
        let exe = tmp.path().join("roundtable");
        fs::write(&exe, "old binary")?;
        let root = tmp.path().join("state");
        fake_daemon(&root, Some(env!("CARGO_PKG_VERSION")), vec![1])?;
        let packed = gzip("#!/bin/sh\necho roundtable 9.9.9\n".into())?;
        let error = perform(
            &Client::new(),
            &release_of(&packed),
            "http://127.0.0.1:1/",
            &exe,
            &root,
            true,
            false,
            |_| panic!("refused before any service step"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("managed session"), "{error}");
        assert_eq!(fs::read_to_string(&exe)?, "old binary");
        let steps = Rc::new(RefCell::new(vec![]));
        let message = perform(
            &Client::new(),
            &release_of(&packed),
            &serve(packed),
            &exe,
            &root,
            true,
            true,
            stop_removes_socket(&root, &steps),
        )?;
        assert!(message.ends_with("service restarted."), "{message}");
        assert_eq!(*steps.borrow(), ["stop", "install", "start"]);
        Ok(())
    }

    #[test]
    fn a_session_started_during_the_download_keeps_the_service_running() -> Result<()> {
        let tmp = tempfile::tempdir_in("/tmp")?;
        let exe = tmp.path().join("roundtable");
        fs::write(&exe, "old binary")?;
        let root = tmp.path().join("state");
        // No managed session before the download, one afterwards.
        fake_daemon(&root, Some(env!("CARGO_PKG_VERSION")), vec![0, 1])?;
        let packed = gzip("#!/bin/sh\necho roundtable 9.9.9\n".into())?;
        let error = perform(
            &Client::new(),
            &release_of(&packed),
            &serve(packed),
            &exe,
            &root,
            true,
            false,
            |_| panic!("the service must not be stopped"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("started meanwhile"), "{error}");
        assert!(fs::read_to_string(&exe)?.contains("echo roundtable 9.9.9"));
        Ok(())
    }

    #[test]
    fn a_service_on_another_version_is_restarted_without_a_download() -> Result<()> {
        let tmp = tempfile::tempdir_in("/tmp")?;
        let exe = tmp.path().join("roundtable");
        fs::write(&exe, "current binary")?;
        let root = tmp.path().join("state");
        // An older daemon does not report its version at all.
        fake_daemon(&root, None, vec![0])?;
        let current = env!("CARGO_PKG_VERSION");
        let steps = Rc::new(RefCell::new(vec![]));
        let message = perform(
            &Client::new(),
            &release(current, &"a".repeat(64)),
            "http://127.0.0.1:1/",
            &exe,
            &root,
            true,
            false,
            stop_removes_socket(&root, &steps),
        )?;
        assert_eq!(
            message,
            format!("Roundtable {current} is up to date; service restarted.")
        );
        assert_eq!(*steps.borrow(), ["stop", "install", "start"]);
        assert_eq!(fs::read_to_string(&exe)?, "current binary");
        Ok(())
    }

    #[test]
    fn a_failed_reinstall_starts_the_previous_service_again_and_explains() -> Result<()> {
        let tmp = tempfile::tempdir_in("/tmp")?;
        let exe = tmp.path().join("roundtable");
        fs::write(&exe, "old binary")?;
        let root = tmp.path().join("state");
        fake_daemon(&root, Some(env!("CARGO_PKG_VERSION")), vec![0])?;
        let packed = gzip("#!/bin/sh\necho roundtable 9.9.9\n".into())?;
        let mut steps = vec![];
        let error = perform(
            &Client::new(),
            &release_of(&packed),
            &serve(packed),
            &exe,
            &root,
            true,
            false,
            |action| {
                if action == "stop" {
                    fs::remove_file(root.join("agent.sock"))?;
                }
                steps.push(action.to_owned());
                if action == "install" {
                    bail!("systemctl failed")
                }
                Ok(())
            },
        )
        .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("could not be reinstalled"), "{text}");
        assert!(text.contains("started again"), "{text}");
        assert!(text.contains("roundtable service install"), "{text}");
        assert_eq!(steps, ["stop", "install", "start"]);
        Ok(())
    }
}
