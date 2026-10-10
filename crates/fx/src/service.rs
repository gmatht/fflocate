//! `ff service install` — write a systemd user unit / launchd plist (§12, §15).
//!
//! We do not enable or start anything automatically; we write the unit and tell
//! the operator the two commands to run. Silently mutating the user's session is
//! not a thing a search tool should do.

use std::path::PathBuf;

use crate::client::CliError;
use findex::config_paths::{socket_path, state_dir};

pub fn run(args: &[String]) -> Result<std::process::ExitCode, CliError> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("install");
    match sub {
        "install" => install(),
        "unit" => {
            println!("{}", systemd_unit());
            Ok(std::process::ExitCode::SUCCESS)
        }
        other => Err(CliError::Usage(format!("unknown service subcommand: {other}"))),
    }
}

fn install() -> Result<std::process::ExitCode, CliError> {
    #[cfg(target_os = "linux")]
    {
        let dir = config_home().join("systemd/user");
        std::fs::create_dir_all(&dir).map_err(|e| CliError::Io(e.to_string()))?;
        let path = dir.join("findexd.service");
        std::fs::write(&path, systemd_unit()).map_err(|e| CliError::Io(e.to_string()))?;
        println!("wrote {}", path.display());
        println!("enable and start it with:");
        println!("  systemctl --user daemon-reload");
        println!("  systemctl --user enable --now findexd.service");
        Ok(std::process::ExitCode::SUCCESS)
    }
    #[cfg(target_os = "macos")]
    {
        let dir = home().join("Library/LaunchAgents");
        std::fs::create_dir_all(&dir).map_err(|e| CliError::Io(e.to_string()))?;
        let path = dir.join("dev.findex.findexd.plist");
        std::fs::write(&path, launchd_plist()).map_err(|e| CliError::Io(e.to_string()))?;
        println!("wrote {}", path.display());
        println!("load it with:");
        println!("  launchctl load -w {}", path.display());
        Ok(std::process::ExitCode::SUCCESS)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(CliError::Usage("service install is only implemented for Linux and macOS".into()))
    }
}

// Only used by the Linux/macOS install paths.
#[cfg_attr(windows, allow(dead_code))]
fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"))
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
pub fn systemd_unit() -> String {
    let socket = socket_path();
    let state = state_dir();
    format!(
        "[Unit]\n\
         Description=findex filename index daemon\n\
         Documentation=https://github.com/findex\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=%h/.cargo/bin/findexd --socket {socket} --state-dir {state}\n\
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

#[cfg(target_os = "macos")]
pub fn launchd_plist() -> String {
    let socket = socket_path();
    let state = state_dir();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.findex.findexd</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/findexd</string>
    <string>--socket</string><string>{socket}</string>
    <string>--state-dir</string><string>{state}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict>
</plist>
"#,
        socket = socket.display(),
        state = state.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_has_exec_and_install_sections() {
        let u = systemd_unit();
        assert!(u.contains("[Service]"));
        assert!(u.contains("ExecStart="));
        assert!(u.contains("findexd"));
        assert!(u.contains("[Install]"));
        assert!(u.contains("WantedBy=default.target"));
        assert!(!u.contains("/tmp"), "unit must not point at /tmp");
    }
}
