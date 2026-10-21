use super::*;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::thread::JoinHandle;
use std::time::Instant;

const FIXTURE: &[(&str, &str, &[u8])] = &[
    (
        "diligence",
        "template.json",
        include_bytes!("../../tests/fixtures/template/template.json"),
    ),
    (
        "diligence",
        "seed/deck.md",
        include_bytes!("../../tests/fixtures/template/seed/deck.md"),
    ),
    (
        "diligence",
        "files/package.json",
        include_bytes!("../../tests/fixtures/template/files/package.json"),
    ),
    (
        "diligence",
        "files/bot.mjs",
        include_bytes!("../../tests/fixtures/template/files/bot.mjs"),
    ),
    (
        "diligence",
        "files/.gitignore",
        include_bytes!("../../tests/fixtures/template/files/.gitignore"),
    ),
];

fn fixture() -> Template<'static> {
    Template::load("diligence", FIXTURE).unwrap()
}

#[test]
fn parses_manifest_and_every_supported_role() -> Result<()> {
    let template = fixture();
    assert_eq!(template.manifest.track, "diligence");
    assert_eq!(template.manifest.bots[0].env, "RED_FLAGS_KEY");
    assert_eq!(template.manifest.seed[0].to, "dataroom/acme/deck.md");
    assert_eq!(template.files().count(), 3);
    for role in ["researcher", "reviewer", "writer", "coordinator"] {
        let role: Role = serde_json::from_value(json!(role))?;
        assert!(serde_json::to_value(role)?.is_string());
    }
    Ok(())
}

#[test]
fn rejects_invalid_manifests_and_unsafe_paths() -> Result<()> {
    for (pointer, value) in [
        ("/track", json!("funds")),
        ("/bots/0/role", json!("admin")),
        ("/bots/0/env", json!("ROUNDTABLE_KEY")),
        ("/bots/0/env", json!("BOT\nKEY")),
        ("/bots/0/env", json!("BOT")),
        ("/bots/0/env", json!("1BOT_KEY")),
        ("/bots/0/name", json!("")),
        ("/seed/0/from", json!("../secret")),
        ("/seed/0/to", json!("/absolute")),
        ("/seed/0/to", json!("folder/../escape")),
    ] {
        let mut manifest: Value = serde_json::from_slice(FIXTURE[0].2)?;
        *manifest.pointer_mut(pointer).unwrap() = value;
        let bytes = serde_json::to_vec(&manifest)?;
        let mut embedded = FIXTURE.to_vec();
        embedded[0].2 = &bytes;
        assert!(Template::load("diligence", &embedded).is_err(), "{pointer}");
    }
    for path in [
        "files/../escape",
        "files/.env",
        "files/.git/config",
        "files/dir\\escape",
    ] {
        let mut embedded = FIXTURE.to_vec();
        embedded.push(("diligence", path, b""));
        assert!(Template::load("diligence", &embedded).is_err(), "{path}");
    }
    Ok(())
}

#[test]
fn missing_and_partial_templates_fail_clearly() {
    let error = Template::load("funds", FIXTURE).err().unwrap().to_string();
    assert!(error.contains("not embedded"));
    assert!(error.contains("templates/funds/template.json"));
    assert!(Template::load("diligence", &[]).is_err());
    let error = Template::load("diligence", &FIXTURE[..1])
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("missing seed/deck.md"));
    assert!(
        Template::load("diligence", &FIXTURE[..2])
            .err()
            .unwrap()
            .to_string()
            .contains("files/")
    );
}

#[test]
fn writes_env_with_actual_names_private_permissions_and_no_parent_key() -> Result<()> {
    let template = fixture();
    let tmp = tempfile::tempdir()?;
    let root = destination(&tmp.path().join("project"), &template, false)?;
    let key = uuid::Uuid::new_v4().to_string();
    let server = Url::parse("https://roundtable.bot")?;
    let env = env_content(
        &server,
        "acme-team",
        &[(
            &template.manifest.bots[0],
            "red-flags-3".into(),
            key.clone(),
            "assistant-abc".into(),
        )],
    )?;
    assert_eq!(
        env,
        format!(
            "ROUNDTABLE_URL=https://roundtable.bot\nROUNDTABLE_PROJECT=acme-team\nRED_FLAGS_KEY={key}\nRED_FLAGS_NAME=red-flags-3\nRED_FLAGS_ID=assistant-abc\n"
        )
    );
    write_project(&root, &template, &env, false)?;
    assert_eq!(fs::read_to_string(root.join(".env"))?, env);
    assert!(!env.contains("ROUNDTABLE_KEY="));
    assert_eq!(
        fs::metadata(root.join(".env"))?.permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::read(root.join("bot.mjs"))?,
        template.asset("files/bot.mjs")?
    );
    assert_eq!(
        fs::read_to_string(root.join(".gitignore"))?,
        "node_modules/\n.env\n"
    );
    fs::set_permissions(root.join(".env"), fs::Permissions::from_mode(0o644))?;
    write_project(&root, &template, &env, true)?;
    assert_eq!(
        fs::metadata(root.join(".env"))?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}

#[test]
fn dotenv_values_cannot_inject_lines_or_expand_shell_content() -> Result<()> {
    assert_eq!(env_value("name # spaces $VALUE")?, "'name # spaces $VALUE'");
    for value in [
        "",
        "key\nOTHER=value",
        "key\rOTHER=value",
        "key\0",
        "has'quote",
        "back\\slash",
    ] {
        assert!(env_value(value).is_err());
    }
    Ok(())
}

#[test]
fn gitignore_preserves_existing_rules_and_overrides_negation() -> Result<()> {
    for (input, expected) in [
        ("", ".env\n"),
        ("dist", "dist\n.env\n"),
        (".env\n", ".env\n"),
        (".env\r\n", ".env\r\n"),
        (".env\n!.env\n", ".env\n!.env\n.env\n"),
        (" .env\n", " .env\n.env\n"),
    ] {
        assert_eq!(ignored_env(input.into()), expected);
    }
    let tmp = tempfile::tempdir()?;
    fs::write(tmp.path().join(".gitignore"), "keep-me\n!.env")?;
    let template = fixture();
    let root = destination(tmp.path(), &template, true)?;
    write_project(&root, &template, "", true)?;
    assert_eq!(
        fs::read_to_string(root.join(".gitignore"))?,
        "keep-me\n!.env\nnode_modules/\n.env\n"
    );
    Ok(())
}

#[test]
fn refuses_nonempty_destination_and_overwrites_only_with_force() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let template = fixture();
    fs::write(tmp.path().join("package.json"), "keep")?;
    let error = destination(tmp.path(), &template, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("--force"));
    assert_eq!(fs::read_to_string(tmp.path().join("package.json"))?, "keep");
    assert!(write_atomic(tmp.path(), "package.json", b"replace", false, 0o644).is_err());
    let root = destination(tmp.path(), &template, true)?;
    write_project(&root, &template, "", true)?;
    assert_ne!(fs::read_to_string(tmp.path().join("package.json"))?, "keep");
    Ok(())
}

#[test]
fn refuses_symlinks_even_with_force() -> Result<()> {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let template = fixture();
    for path in [".env", ".gitignore", "bot.mjs"] {
        symlink(outside.path().join("untouched"), tmp.path().join(path))?;
        assert!(destination(tmp.path(), &template, true).is_err());
        assert!(write_atomic(tmp.path(), path, b"unsafe", true, 0o600).is_err());
        fs::remove_file(tmp.path().join(path))?;
    }
    symlink(outside.path(), tmp.path().join("nested"))?;
    assert!(write_atomic(tmp.path(), "nested/escape", b"unsafe", true, 0o600).is_err());
    assert!(!outside.path().join("escape").exists());
    assert!(destination(&tmp.path().join("nested"), &template, true).is_err());
    Ok(())
}

struct Mock {
    url: Url,
    worker: JoinHandle<Result<Vec<(String, Value)>>>,
}

impl Mock {
    fn new(replies: Vec<(u16, Value)>, key: &str) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        let authorization = format!("authorization: bearer {key}");
        let worker = std::thread::spawn(move || -> Result<_> {
            let mut requests = Vec::new();
            for (status, reply) in replies {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            ensure!(Instant::now() < deadline, "Mock server timed out");
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => return Err(error.into()),
                    }
                };
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                let mut reader = BufReader::new(stream.try_clone()?);
                let mut request = String::new();
                reader.read_line(&mut request)?;
                let mut length = 0;
                let mut authorized = false;
                loop {
                    let mut line = String::new();
                    ensure!(reader.read_line(&mut line)? > 0, "Incomplete mock request");
                    if line == "\r\n" {
                        break;
                    }
                    let line = line.trim().to_ascii_lowercase();
                    authorized |= line == authorization;
                    if let Some(value) = line.strip_prefix("content-length:") {
                        length = value.trim().parse()?;
                    }
                }
                ensure!(
                    authorized && request.starts_with("POST /api/tools/"),
                    "Incorrect HTTP authorization or method"
                );
                ensure!(length < 1_000_000, "Mock request too large");
                let mut body = vec![0; length];
                reader.read_exact(&mut body)?;
                requests.push((request, serde_json::from_slice(&body)?));
                let body = reply.to_string();
                write!(
                    stream,
                    "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )?;
            }
            Ok(requests)
        });
        Ok(Self { url, worker })
    }

    fn api<'a>(&'a self, key: &'a str) -> Result<Api<'a>> {
        Ok(Api {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(3))
                .build()?,
            server: &self.url,
            key,
        })
    }

    fn finish(self) -> Result<Vec<(String, Value)>> {
        self.worker.join().unwrap()
    }
}

#[test]
fn provisions_with_fresh_request_ids_suffix_retries_and_create_only_seed() -> Result<()> {
    let key = uuid::Uuid::new_v4().to_string();
    let child_key = uuid::Uuid::new_v4().to_string();
    let mock = Mock::new(
        vec![
            (
                200,
                json!([{"bank":"shared-notes","access":"write"},{"bank":"acme-team","access":"write"},{"bank":"read-only","access":"read"}]),
            ),
            (409, json!({"error":"red-flags already exists"})),
            (409, json!({"error":"red-flags-2 already exists"})),
            (
                200,
                json!({"assistant":{"id":"assistant-abc","name":"red-flags-3"},"token":child_key}),
            ),
            (
                409,
                json!({"error":"That file already exists; open it to edit"}),
            ),
        ],
        &key,
    )?;
    let template = fixture();
    let tmp = tempfile::tempdir()?;
    let root = destination(tmp.path(), &template, false)?;
    assert_eq!(
        provision(&mock.api(&key)?, &root, &template, None, false)?,
        "acme-team"
    );
    let env = fs::read_to_string(root.join(".env"))?;
    assert!(env.contains(&format!(
        "RED_FLAGS_KEY={child_key}\nRED_FLAGS_NAME=red-flags-3\nRED_FLAGS_ID=assistant-abc\n"
    )));
    assert!(!env.contains(&key));
    let requests = mock.finish()?;
    assert!(requests[0].0.starts_with("POST /api/tools/list_banks "));
    assert_eq!(requests[0].1, json!({}));
    let mut ids = HashSet::new();
    for (index, (_, args)) in requests.iter().enumerate().skip(1) {
        assert!(ids.insert(args["request_id"].as_str().unwrap()));
        uuid::Uuid::parse_str(args["request_id"].as_str().unwrap())?;
        if index < 4 {
            assert_eq!(
                args["name"],
                ["red-flags", "red-flags-2", "red-flags-3"][index - 1]
            );
            assert_eq!(args["access"], json!({"acme-team":"write"}));
            assert_eq!(args["client"], "custom-agent");
            assert_eq!(args["role"], "reviewer");
        } else {
            assert_eq!(args["create_only"], true);
            assert_eq!(args["path"], "dataroom/acme/deck.md");
            assert_eq!(args["bank"], "acme-team");
            assert_eq!(
                args["content"],
                std::str::from_utf8(template.asset("seed/deck.md")?)?
            );
        }
    }
    Ok(())
}

#[test]
fn bank_selection_explains_zero_multiple_and_unwritable_choices() -> Result<()> {
    let key = uuid::Uuid::new_v4().to_string();
    for (banks, project, expected) in [
        (json!([]), None, "Agents → Connect agent"),
        (
            json!([{"bank":"shared-notes","access":"write"}]),
            None,
            "No writable team bank",
        ),
        (
            json!([{"bank":"one","access":"write"},{"bank":"two","access":"write"}]),
            None,
            "--project BANK",
        ),
        (
            json!([{"bank":"one","access":"read"}]),
            Some("one"),
            "not a writable bank",
        ),
    ] {
        let mock = Mock::new(vec![(200, banks)], &key)?;
        assert!(
            mock.api(&key)?
                .bank(project)
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
        mock.finish()?;
    }
    let mock = Mock::new(
        vec![(
            200,
            json!([{"bank":"one","access":"write"},{"bank":"two","access":"write"}]),
        )],
        &key,
    )?;
    assert_eq!(mock.api(&key)?.bank(Some("two"))?, "two");
    mock.finish()?;
    Ok(())
}

#[test]
fn http_errors_redact_parent_key_and_do_not_retry_forbidden_mint() -> Result<()> {
    let key = uuid::Uuid::new_v4().to_string();
    let mock = Mock::new(
        vec![(
            403,
            json!({"error":format!("Denied {key}\n"), "token":uuid::Uuid::new_v4().to_string()}),
        )],
        &key,
    )?;
    let error = mock
        .api(&key)?
        .mint(&fixture().manifest.bots[0], "acme-team")
        .unwrap_err()
        .to_string();
    assert!(error.contains("403") && error.contains("[redacted]"));
    assert!(!error.contains(&key) && !error.contains('\n'));
    assert_eq!(mock.finish()?.len(), 1);
    Ok(())
}

#[test]
fn unrelated_mint_conflicts_do_not_change_the_name() -> Result<()> {
    let key = uuid::Uuid::new_v4().to_string();
    let mock = Mock::new(
        vec![(409, json!({"error":"Another operation is in progress"}))],
        &key,
    )?;
    assert!(
        mock.api(&key)?
            .mint(&fixture().manifest.bots[0], "acme-team")
            .unwrap_err()
            .to_string()
            .contains("409")
    );
    assert_eq!(mock.finish()?.len(), 1);
    Ok(())
}

#[test]
fn hidden_prompt_never_echoes_and_restores_terminal_on_success_or_cancel() -> Result<()> {
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    if let Ok(mode) = std::env::var("ROUNDTABLE_PROMPT_TEST") {
        let result = team_key();
        if mode == "cancel" {
            assert!(result.is_err());
        } else {
            assert!(
                result.is_ok_and(|key| Some(key) == std::env::var("ROUNDTABLE_PROMPT_INPUT").ok())
            );
        }
        return Ok(());
    }
    for cancel in [false, true] {
        let mut master = -1;
        let mut slave = -1;
        ensure!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            } == 0,
            "Cannot open test terminal"
        );
        let mut master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        ensure!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == 0,
            "Cannot configure test terminal"
        );
        let key = uuid::Uuid::new_v4().to_string();
        let mut command = Command::new(std::env::current_exe()?);
        command.args(["--exact", "init::tests::hidden_prompt_never_echoes_and_restores_terminal_on_success_or_cancel", "--nocapture"])
            .env_remove("ROUNDTABLE_KEY")
            .env("ROUNDTABLE_PROMPT_TEST", if cancel { "cancel" } else { "read" })
            .env("ROUNDTABLE_PROMPT_INPUT", &key)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave.try_clone()?));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let result = (|| -> Result<()> {
            let mut buffer = [0; 4096];
            while !String::from_utf8_lossy(&output).contains("Team key (hidden):") {
                if let Ok(n) = master.read(&mut buffer) {
                    output.extend_from_slice(&buffer[..n]);
                }
                ensure!(Instant::now() < deadline, "Hidden prompt timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
            let terminal = || -> Result<libc::termios> {
                let mut state = std::mem::MaybeUninit::uninit();
                ensure!(
                    unsafe { libc::tcgetattr(slave.as_raw_fd(), state.as_mut_ptr()) } == 0,
                    "Cannot inspect test terminal"
                );
                Ok(unsafe { state.assume_init() })
            };
            assert_eq!(terminal()?.c_lflag & libc::ECHO, 0);
            if cancel {
                master.write_all(&[3])?;
            } else {
                writeln!(master, "{key}")?;
            }
            loop {
                if let Some(status) = child.try_wait()? {
                    ensure!(status.success(), "Hidden prompt child failed");
                    break;
                }
                ensure!(Instant::now() < deadline, "Hidden prompt child timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
            while let Ok(n) = master.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                output.extend_from_slice(&buffer[..n]);
            }
            assert_ne!(terminal()?.c_lflag & libc::ECHO, 0);
            assert!(!String::from_utf8_lossy(&output).contains(&key));
            Ok(())
        })();
        let _ = child.kill();
        let _ = child.wait();
        result?;
    }
    Ok(())
}

#[test]
fn seed_writes_succeed_and_unrelated_conflicts_are_not_skipped() -> Result<()> {
    let key = uuid::Uuid::new_v4().to_string();
    for (status, error, success) in [
        (200, "", true),
        (403, "Denied", false),
        (409, "Stale version", false),
    ] {
        let mock = Mock::new(vec![(status, json!({"error":error}))], &key)?;
        assert_eq!(
            mock.api(&key)?.seed(&fixture(), "acme-team").is_ok(),
            success
        );
        mock.finish()?;
    }
    Ok(())
}

#[test]
fn force_reports_bots_from_the_replaced_env() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".env"),
        "ROUNDTABLE_URL=https://roundtable.bot\nRED_FLAGS_KEY=secret\nRED_FLAGS_NAME=red-flags-2\nCOMPS_NAME=comps\nEMPTY_NAME=\n",
    )
    .unwrap();
    assert_eq!(previous_bots(dir.path()), ["red-flags-2", "comps"]);
    let empty = tempfile::tempdir().unwrap();
    assert!(previous_bots(empty.path()).is_empty());
}
