//! `ff service install` — write a systemd unit (Linux) or launchd plist
//! (macOS) (§12, §15).
//!
//! We do not enable or start anything automatically; we write the unit and tell
//! the operator the commands to run. Silently mutating the user's session is
//! not a thing a search tool should do.

use std::path::PathBuf;

use crate::client::CliError;
use fflocate::config_paths::{socket_path, state_dir};

/// `dev.fflocate.fflocated` is the label the plist is registered under; keep it
/// and the filename in sync (launchd does not read the Label, but users do).
pub const LAUNCHD_LABEL: &str = "com.fflocate.fflocated";

pub fn run(args: &[String]) -> Result<std::process::ExitCode, CliError> {
    let system = args.iter().any(|a| a == "--system");
    let sub = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(|s| s.as_str())
        .unwrap_or("install");
    match sub {
        "install" => install(system),
        "unit" => {
            println!("{}", platform_unit(system));
            Ok(std::process::ExitCode::SUCCESS)
        }
        other => Err(CliError::Usage(format!(
            "unknown service subcommand: {other}"
        ))),
    }
}

fn install(system: bool) -> Result<std::process::ExitCode, CliError> {
    #[cfg(target_os = "linux")]
    {
        let _ = system; // Linux system-wide installs are packaging's job.
        let dir = config_home().join("systemd/user");
        std::fs::create_dir_all(&dir).map_err(|e| CliError::Io(e.to_string()))?;
        let path = dir.join("fflocated.service");
        std::fs::write(&path, systemd_unit()).map_err(|e| CliError::Io(e.to_string()))?;
        println!("wrote {}", path.display());
        println!("enable and start it with:");
        println!("  systemctl --user daemon-reload");
        println!("  systemctl --user enable --now fflocated.service");
        Ok(std::process::ExitCode::SUCCESS)
    }
    #[cfg(target_os = "macos")]
    {
        // Per-user agents live in ~/Library/LaunchAgents and run in the user's
        // GUI session; system daemons live in /Library/LaunchDaemons and need
        // root. Both are written, never loaded, here.
        let (dir, path) = if system {
            (
                PathBuf::from("/Library/LaunchDaemons"),
                PathBuf::from(format!("/Library/LaunchDaemons/{LAUNCHD_LABEL}.plist")),
            )
        } else {
            let dir = home().join("Library/LaunchAgents");
            let path = dir.join(format!("{LAUNCHD_LABEL}.plist"));
            (dir, path)
        };
        std::fs::create_dir_all(&dir).map_err(|e| CliError::Io(e.to_string()))?;
        std::fs::write(&path, launchd_plist(system)).map_err(|e| CliError::Io(e.to_string()))?;
        println!("wrote {}", path.display());
        let uid = std::env::var("UID")
            .ok()
            .or_else(|| {
                let out = std::process::Command::new("id").arg("-u").output().ok()?;
                String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string())
            })
            .unwrap_or_else(|| "$(id -u)".to_string());
        if system {
            println!("load it with (as root):");
            println!("  sudo launchctl bootstrap system {}", path.display());
            println!("  sudo launchctl enable system/{LAUNCHD_LABEL}");
            println!(
                "the daemon's socket is /var/run/fflocated.sock, owned by group `fflocate`;"
            );
            println!("create it first if it does not exist:");
            println!("  sudo dseditgroup -o create fflocate");
            println!("  sudo dseditgroup -o edit -a $USER -t user fflocate");
        } else {
            println!("load it with:");
            println!("  launchctl bootstrap gui/{uid} {}", path.display());
            println!("  launchctl enable gui/{uid}/{LAUNCHD_LABEL}");
            println!("  launchctl kickstart -k gui/{uid}/{LAUNCHD_LABEL}");
        }
        Ok(std::process::ExitCode::SUCCESS)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = system;
        Err(CliError::Usage(
            "service install is only implemented for Linux and macOS".into(),
        ))
    }
}

/// The unit/plist for this platform. `ff service unit` prints it so an operator
/// can install it by hand (packaging, Ansible, a container image).
pub fn platform_unit(system: bool) -> String {
    #[cfg(target_os = "macos")]
    {
        launchd_plist(system)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = system;
        systemd_unit()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = system;
        String::new()
    }
}

// Only used by the Linux/macOS install paths.
#[cfg_attr(windows, allow(dead_code))]
fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

/// XDG config home, used only by the Linux systemd-unit path.
#[cfg(target_os = "linux")]
fn config_home() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_CONFIG_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    home().join(".config")
}

/// A systemd user unit. `%h` is the user's home; we do not index it explicitly
/// because the daemon defaults to `$HOME`.
#[cfg(target_os = "linux")]
pub fn systemd_unit() -> String {
    let socket = socket_path();
    let state = state_dir();
    format!(
        "[Unit]\n\
         Description=fflocate filename index daemon\n\
         Documentation=https://github.com/fflocate\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=%h/.cargo/bin/fflocated --socket {socket} --state-dir {state}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         # The daemon is per-user and indexes only what the user can read (§16).\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        socket = socket.display(),
        state = state.display(),
    )
}

/// A launchd property list.
///
/// Differences from the systemd unit that matter on macOS:
///
/// * `ProcessType=Background` tells launchd this is not latency-critical, so
///   it is scheduled politely (and throttled less aggressively than a default
///   daemon when idle).
/// * `KeepAlive` restarts on crash; `ThrottleInterval` keeps a crash-loop from
///   spinning.
/// * stdout/stderr go to a log file, because launchd otherwise discards them.
#[cfg(target_os = "macos")]
pub fn launchd_plist(system: bool) -> String {
    let socket = socket_path();
    let state = state_dir();
    let log = if system {
        PathBuf::from("/var/log/fflocated.log")
    } else {
        home().join("Library/Logs/fflocated.log")
    };
    // Point at the daemon that ships with this `ff` (a Homebrew install lives
    // under /opt/homebrew on Apple Silicon, not /usr/local), falling back to
    // the conventional location when the sibling cannot be located.
    let daemon = fflocate::spawn::daemon_binary_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/usr/local/bin/fflocated".to_string());
    let mut args = vec![daemon];
    if system {
        args.push("--system".to_string());
    }
    args.extend([
        "--socket".to_string(),
        socket.display().to_string(),
        "--state-dir".to_string(),
        state.display().to_string(),
    ]);
    let args_xml = args
        .iter()
        .map(|a| format!("    <string>{}</string>", xml_escape(a)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{args}
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>ProcessType</key><string>Background</string>
  <key>LowPriorityIO</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        args = args_xml,
        log = xml_escape(&log.display().to_string()),
    )
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn unit_has_exec_and_install_sections() {
        let u = systemd_unit();
        assert!(u.contains("[Service]"));
        assert!(u.contains("ExecStart="));
        assert!(u.contains("fflocated"));
        assert!(u.contains("[Install]"));
        assert!(u.contains("WantedBy=default.target"));
        assert!(!u.contains("/tmp"), "unit must not point at /tmp");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn launchd_plist_is_a_valid_background_agent() {
        let p = launchd_plist(false);
        assert!(p.contains(&format!("<string>{LAUNCHD_LABEL}</string>")));
        assert!(p.contains("<key>RunAtLoad</key><true/>"));
        assert!(p.contains("<key>KeepAlive</key><true/>"));
        assert!(p.contains("<key>ProcessType</key><string>Background</string>"));
        assert!(p.contains("Library/Logs/fflocated.log"));
        assert!(p.contains("--socket"));
        // The per-user plist must not ask for system mode.
        assert!(!p.contains("--system"));

        let s = launchd_plist(true);
        assert!(s.contains("--system"));
        assert!(s.contains("/var/log/fflocated.log"));
        assert!(!s.contains("/tmp"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn unit_is_the_plist_on_macos() {
        assert_eq!(platform_unit(false), launchd_plist(false));
    }
}
