use crate::state::private_dir;
use anyhow::{Context, Result, bail};
use reqwest::Url;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn systemd(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

fn label(root: &Path) -> String {
    format!(
        "bot.roundtable.{}",
        &format!("{:x}", Sha256::digest(root.as_os_str().as_encoded_bytes()))[..16]
    )
}
fn service_file(root: &Path) -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new().context("Cannot locate user home")?;
    Ok(if cfg!(target_os = "macos") {
        dirs.home_dir()
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", label(root)))
    } else {
        dirs.config_dir()
            .join("systemd/user")
            .join(format!("{}.service", label(root)))
    })
}
fn run(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        bail!(
            "Service manager failed. On headless Linux without systemd, run roundtable daemon in a supervisor."
        );
    }
    Ok(())
}

pub fn definition(root: &Path, config: &Path, server: &Url, binary: &Path, mac: bool) -> String {
    let args = [
        binary.to_string_lossy().into_owned(),
        "--config-dir".into(),
        config.to_string_lossy().into_owned(),
        "--server".into(),
        server.to_string(),
        "daemon".into(),
    ];
    let env: [(&str, String); 3] = [
        (
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
        ),
        (
            "CODEX_HOME",
            crate::discovery::home("codex").to_string_lossy().into(),
        ),
        (
            "CLAUDE_CONFIG_DIR",
            crate::discovery::home("claude").to_string_lossy().into(),
        ),
    ];
    if mac {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{}</array><key>EnvironmentVariables</key><dict>{}</dict><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>10</integer><key>Umask</key><integer>63</integer><key>StandardOutPath</key><string>/dev/null</string><key>StandardErrorPath</key><string>{}</string></dict></plist>\n",
            xml(&label(root)),
            args.iter()
                .map(|a| format!("<string>{}</string>", xml(a)))
                .collect::<String>(),
            env.iter()
                .map(|(k, v)| format!("<key>{k}</key><string>{}</string>", xml(v)))
                .collect::<String>(),
            xml(&root.join("service.log").to_string_lossy())
        )
    } else {
        format!(
            "[Unit]\nDescription=Roundtable coding session service\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={}\n{}\nRestart=on-failure\nRestartSec=10\nUMask=0077\nTimeoutStopSec=30\nKillMode=control-group\n\n[Install]\nWantedBy=default.target\n",
            args.iter()
                .map(|a| systemd(a))
                .collect::<Vec<_>>()
                .join(" "),
            env.iter()
                .map(|(k, v)| format!("Environment={}", systemd(&format!("{k}={v}"))))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

/// Whether this installation's LaunchAgent or systemd unit exists.
pub fn installed(root: &Path) -> Result<bool> {
    Ok(service_file(root)?.exists())
}

pub fn manage(action: &str, root: &Path, config: &Path, server: &Url) -> Result<()> {
    let config = fs::canonicalize(config)?;
    let file = service_file(root)?;
    let name = label(root);
    let domain = format!("gui/{}", unsafe { libc::geteuid() });
    if action == "install" {
        private_dir(root)?;
        if crate::daemon::request(root, &serde_json::json!({"op":"status"})).is_ok() {
            bail!(
                "Service is running. Stop it deliberately before installing an updated service binary; this ends Roundtable-managed sessions."
            );
        }
        let native = root.join("native");
        private_dir(&native)?;
        let binary = native.join(format!("roundtable-{}", env!("CARGO_PKG_VERSION")));
        let mut staged = tempfile::NamedTempFile::new_in(&native)?;
        std::io::copy(&mut fs::File::open(std::env::current_exe()?)?, &mut staged)?;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o700))?;
        staged.as_file().sync_all()?;
        staged.persist(&binary)?;
        fs::create_dir_all(file.parent().unwrap())?;
        let mut staged = tempfile::NamedTempFile::new_in(file.parent().unwrap())?;
        use std::io::Write;
        staged.write_all(
            definition(root, &config, server, &binary, cfg!(target_os = "macos")).as_bytes(),
        )?;
        staged.as_file().sync_all()?;
        staged.persist(&file)?;
        if !cfg!(target_os = "macos") {
            run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            run(Command::new("systemctl").args(["--user", "enable", &format!("{name}.service")]))?;
        }
        println!(
            "Installed {}. Starts at user login. Run roundtable service start.",
            file.display()
        );
        return Ok(());
    }
    if action == "status" {
        if cfg!(target_os = "macos") {
            return run(Command::new("launchctl").args(["print", &format!("{domain}/{name}")]));
        }
        return run(Command::new("systemctl").args([
            "--user",
            "status",
            &format!("{name}.service"),
            "--no-pager",
        ]));
    }
    if action == "start" {
        if !file.exists() {
            bail!("Service is not installed. Run roundtable service install.");
        }
        if cfg!(target_os = "macos") {
            run(Command::new("launchctl")
                .arg("bootstrap")
                .arg(&domain)
                .arg(&file))?;
        } else {
            run(Command::new("systemctl").args(["--user", "start", &format!("{name}.service")]))?;
        }
    } else if action == "stop" || action == "uninstall" {
        if cfg!(target_os = "macos") {
            run(Command::new("launchctl").args(["bootout", &format!("{domain}/{name}")]))?;
        } else {
            let mut command = Command::new("systemctl");
            command.arg("--user");
            if action == "uninstall" {
                command.args(["disable", "--now"]);
            } else {
                command.arg("stop");
            }
            run(command.arg(format!("{name}.service")))?;
        }
        if action == "uninstall" && file.exists() {
            fs::remove_file(file)?;
            if !cfg!(target_os = "macos") {
                run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            }
        }
    } else {
        bail!("Unknown service operation");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_paths_are_escaped_and_native() {
        let url = Url::parse("https://roundtable.bot").unwrap();
        let root = Path::new("/home/test & user/.roundtable");
        let mac = definition(root, root, &url, Path::new("/a & b/native"), true);
        assert!(mac.contains("/a &amp; b/native"));
        assert!(mac.contains("<key>RunAtLoad</key>"));
        let linux = definition(root, root, &url, Path::new("/a %i $HOME/native"), false);
        assert!(linux.contains("%%i $$HOME"));
        assert!(!linux.contains("node "));
    }
}
