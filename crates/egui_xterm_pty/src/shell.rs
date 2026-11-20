//! Cross-platform shell discovery and command builder utilities for [`crate::PtyBackend`].

#[cfg(not(windows))]
use std::path::Path;

use egui_xterm_core::TerminalIoError;
use portable_pty::CommandBuilder;

use crate::PtyConfig;

/// Detects the default interactive shell executable for the current operating system.
///
/// - On Windows, checks `COMSPEC`, then falls back to PowerShell (`pwsh.exe` /
///   `powershell.exe` on `PATH`) or `"cmd.exe"`.
/// - On Unix-like systems, checks `$SHELL` (if non-empty and pointing to an existing
///   file), falling back to `"/bin/sh"`.
#[must_use]
pub fn default_shell() -> String {
    #[cfg(windows)]
    {
        if let Ok(comspec) = std::env::var("COMSPEC")
            && !comspec.trim().is_empty()
        {
            return comspec;
        }
        if let Some(ps) = find_executable_on_path(&["pwsh.exe", "powershell.exe"]) {
            return ps;
        }
        "cmd.exe".to_owned()
    }

    #[cfg(not(windows))]
    {
        if let Ok(shell) = std::env::var("SHELL") {
            let trimmed = shell.trim();
            if !trimmed.is_empty() && Path::new(trimmed).is_file() {
                return trimmed.to_owned();
            }
        }
        "/bin/sh".to_owned()
    }
}

#[cfg(windows)]
fn find_executable_on_path(candidates: &[&str]) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for candidate in candidates {
            let full = dir.join(candidate);
            if full.is_file() {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Validates [`PtyConfig`] and constructs a `portable_pty::CommandBuilder`.
pub(crate) fn build_command(config: PtyConfig) -> Result<CommandBuilder, TerminalIoError> {
    if let Some(cwd) = &config.cwd
        && !cwd.is_dir()
    {
        return Err(TerminalIoError::message(format!(
            "PTY working directory does not exist or is not a directory: {}",
            cwd.display()
        )));
    }

    // `CommandBuilder::new_default_prog()` panics if `.arg()` / `.args()` is called on it,
    // so only use it when both `shell` is `None` and `args` is empty.
    let mut cmd = match config.shell {
        Some(shell) => {
            if shell.trim().is_empty() {
                return Err(TerminalIoError::message("PTY shell path must not be empty"));
            }
            let mut builder = CommandBuilder::new(shell);
            if !config.args.is_empty() {
                builder.args(config.args);
            }
            builder
        }
        None if config.args.is_empty() => CommandBuilder::new_default_prog(),
        None => {
            let mut builder = CommandBuilder::new(default_shell());
            builder.args(config.args);
            builder
        }
    };

    // Set modern terminal capability defaults before applying user overrides.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    if let Some(cwd) = config.cwd {
        cmd.cwd(cwd);
    }

    for (key, value) in config.env {
        cmd.env(key, value);
    }

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_is_non_empty() {
        assert!(!default_shell().trim().is_empty());
    }

    #[test]
    fn build_command_rejects_missing_cwd() {
        let missing = std::env::temp_dir().join("egui_xterm_pty_nonexistent_dir_xyz");
        let cfg = PtyConfig {
            cwd: Some(missing),
            ..PtyConfig::default()
        };
        let err = build_command(cfg).expect_err("missing cwd must fail");
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn build_command_with_default_shell_and_args_does_not_panic() {
        let cfg = PtyConfig {
            shell: None,
            args: vec!["--version".to_owned()],
            ..PtyConfig::default()
        };
        let cmd = build_command(cfg).expect("build_command with args");
        assert!(cmd.get_argv().len() >= 2);
    }
}
