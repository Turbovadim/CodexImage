//! Finding the Codex CLI and launching it with a usable PATH, plus the
//! process-group helpers that let a whole job tree be signalled at once.

use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(4);
const LOGIN_SHELL_PATH_MARKER: &[u8] = b"__CODEXIMAGE_PATH__=";

pub struct CodexInvocation {
    pub executable: PathBuf,
    path: OsString,
}

fn read_tail_bytes(mut reader: impl Read, limit: usize) -> Vec<u8> {
    let mut tail = Vec::with_capacity(limit);
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                tail.extend_from_slice(&chunk[..read]);
                if tail.len() > limit {
                    tail.drain(..tail.len() - limit);
                }
            }
        }
    }
    tail
}

pub fn read_tail(reader: impl Read, limit: usize) -> String {
    String::from_utf8_lossy(&read_tail_bytes(reader, limit)).into_owned()
}

impl CodexInvocation {
    pub fn resolve() -> Self {
        let path = build_command_path(
            login_shell_path(),
            env::var_os("PATH"),
            dirs::home_dir().as_deref(),
        );
        let executable = env::var_os("CODEX_BIN")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                find_executable_on_path(OsStr::new("codex"), &path)
                    .unwrap_or_else(|| PathBuf::from("codex"))
            });
        Self { executable, path }
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.env("PATH", &self.path);
        command
    }
}

pub fn build_command_path(
    login_shell: Option<OsString>,
    inherited: Option<OsString>,
    home: Option<&Path>,
) -> OsString {
    let mut entries = Vec::new();
    if let Some(path) = login_shell {
        entries.extend(env::split_paths(&path));
    }
    if let Some(path) = inherited.as_ref() {
        entries.extend(env::split_paths(path));
    }
    if let Some(home) = home {
        entries.extend([
            home.join(".bun/bin"),
            home.join(".local/bin"),
            home.join(".cargo/bin"),
            home.join(".volta/bin"),
            home.join(".npm-global/bin"),
            home.join("Library/pnpm"),
        ]);
        let nvm_versions = home.join(".nvm/versions/node");
        if let Ok(versions) = fs::read_dir(nvm_versions) {
            let mut version_bins: Vec<_> = versions
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("bin"))
                .filter(|path| path.is_dir())
                .collect();
            version_bins.sort_by(|left, right| right.cmp(left));
            entries.extend(version_bins);
        }
    }
    entries.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ]);

    let mut seen = HashSet::new();
    entries.retain(|entry| !entry.as_os_str().is_empty() && seen.insert(entry.clone()));
    env::join_paths(entries).unwrap_or_else(|_| {
        inherited.unwrap_or_else(|| OsString::from("/usr/bin:/bin:/usr/sbin:/sbin"))
    })
}

pub fn find_executable_on_path(name: &OsStr, path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub fn is_executable(path: &Path) -> bool {
    path.is_file()
}

pub fn login_shell_path() -> Option<OsString> {
    let shell = env::var_os("SHELL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("/bin/zsh"));
    let mut command = Command::new(shell);
    command
        .args(["-ilc", "printf '\n__CODEXIMAGE_PATH__=%s\n' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    configure_process_group(&mut command);
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let output = thread::spawn(move || read_tail_bytes(stdout, 64 * 1024));
    let deadline = Instant::now() + LOGIN_SHELL_TIMEOUT;
    let succeeded = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                terminate_process(&mut child);
                break false;
            }
            Err(_) => {
                terminate_process(&mut child);
                break false;
            }
        }
    };
    let bytes = output.join().ok()?;
    succeeded.then(|| parse_login_shell_path(&bytes)).flatten()
}

pub fn terminate_process(child: &mut std::process::Child) {
    #[cfg(unix)]
    kill_process_group(child.id() as i32, libc::SIGKILL);
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
pub fn parse_login_shell_path(output: &[u8]) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    let start = output
        .windows(LOGIN_SHELL_PATH_MARKER.len())
        .rposition(|window| window == LOGIN_SHELL_PATH_MARKER)?
        + LOGIN_SHELL_PATH_MARKER.len();
    let end = output[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(output.len(), |offset| start + offset);
    (end > start).then(|| OsString::from_vec(output[start..end].to_vec()))
}

#[cfg(not(unix))]
pub fn parse_login_shell_path(output: &[u8]) -> Option<OsString> {
    let output = String::from_utf8_lossy(output);
    output
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("__CODEXIMAGE_PATH__="))
        .filter(|path| !path.is_empty())
        .map(OsString::from)
}

#[cfg(unix)]
pub fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
pub fn configure_process_group(_: &mut Command) {}

#[cfg(unix)]
pub fn kill_process_group(pid: i32, signal: i32) {
    if pid > 0 {
        unsafe {
            libc::kill(-pid, signal);
        }
    }
}

#[cfg(not(unix))]
pub fn kill_process_group(_: i32, _: i32) {}

#[cfg(test)]
mod tests {
    use super::{build_command_path, find_executable_on_path, parse_login_shell_path, read_tail};
    use std::ffi::{OsStr, OsString};
    use std::io::Cursor;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn stderr_tail_drains_input_and_retains_the_requested_suffix() {
        let input: Vec<_> = (0_u32..20_000)
            .map(|value| b'a' + (value % 26) as u8)
            .collect();
        let result = read_tail(Cursor::new(&input), 4_096);
        assert_eq!(result.as_bytes(), &input[input.len() - 4_096..]);
    }

    #[test]
    fn command_path_prefers_the_login_shell_and_deduplicates_entries() {
        let directory = TempDir::new().unwrap();
        let login = OsString::from("/login/bin:/shared/bin");
        let inherited = OsString::from("/shared/bin:/inherited/bin");

        let path = build_command_path(Some(login), Some(inherited), Some(directory.path()));
        let entries: Vec<_> = std::env::split_paths(&path).collect();

        assert_eq!(entries[0], PathBuf::from("/login/bin"));
        assert_eq!(entries[1], PathBuf::from("/shared/bin"));
        assert_eq!(entries[2], PathBuf::from("/inherited/bin"));
        assert_eq!(
            entries
                .iter()
                .filter(|entry| *entry == &PathBuf::from("/shared/bin"))
                .count(),
            1
        );
        assert!(entries.contains(&directory.path().join(".bun/bin")));
        assert!(entries.contains(&PathBuf::from("/opt/homebrew/bin")));
    }

    #[cfg(unix)]
    #[test]
    fn executable_resolution_finds_an_executable_file() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TempDir::new().unwrap();
        let executable = directory.path().join("codex");
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        let mut permissions = executable.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let path = std::env::join_paths([directory.path()]).unwrap();

        assert_eq!(
            find_executable_on_path(OsStr::new("codex"), &path),
            Some(executable)
        );
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_path_parser_ignores_shell_startup_output() {
        let output = b"startup noise\n__CODEXIMAGE_PATH__=/user/bin:/usr/bin\n";

        assert_eq!(
            parse_login_shell_path(output),
            Some(OsString::from("/user/bin:/usr/bin"))
        );
    }
}
