//! Finding the Codex CLI and launching it with a usable PATH, plus the
//! process-group helpers that let a whole job tree be signalled at once.

use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
#[cfg(not(target_os = "windows"))]
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(4);
#[cfg(unix)]
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
                    .unwrap_or_else(default_codex_executable)
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
        #[cfg(target_os = "windows")]
        entries.extend([
            home.join(".bun/bin"),
            home.join(".cargo/bin"),
            home.join(".volta/bin"),
            home.join("AppData/Roaming/npm"),
            home.join("AppData/Local/pnpm"),
            home.join("AppData/Local/Microsoft/WinGet/Links"),
        ]);
        #[cfg(not(target_os = "windows"))]
        entries.extend([
            home.join(".bun/bin"),
            home.join(".local/bin"),
            home.join(".cargo/bin"),
            home.join(".volta/bin"),
            home.join(".npm-global/bin"),
            home.join("Library/pnpm"),
        ]);
        #[cfg(not(target_os = "windows"))]
        {
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
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = env::var_os("APPDATA") {
            entries.push(PathBuf::from(app_data).join("npm"));
        }
        if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            let local_app_data = PathBuf::from(local_app_data);
            entries.extend([
                local_app_data.join("pnpm"),
                local_app_data.join("Volta/bin"),
                local_app_data.join("Microsoft/WinGet/Links"),
            ]);
        }
        for variable in ["PNPM_HOME", "NVM_HOME", "NVM_SYMLINK"] {
            if let Some(directory) = env::var_os(variable) {
                entries.push(PathBuf::from(directory));
            }
        }
        for variable in ["BUN_INSTALL", "VOLTA_HOME"] {
            if let Some(directory) = env::var_os(variable) {
                entries.push(PathBuf::from(directory).join("bin"));
            }
        }
        for variable in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(program_files) = env::var_os(variable) {
                entries.push(PathBuf::from(program_files).join("nodejs"));
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
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
    env::join_paths(entries).unwrap_or_else(|_| inherited.unwrap_or_default())
}

pub fn find_executable_on_path(name: &OsStr, path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path).find_map(|directory| {
        executable_names(name)
            .into_iter()
            .map(|name| directory.join(name))
            .find(|candidate| is_executable(candidate))
    })
}

fn executable_names(name: &OsStr) -> Vec<OsString> {
    let names = vec![name.to_owned()];
    #[cfg(target_os = "windows")]
    let names = {
        if Path::new(name).extension().is_some() {
            return names;
        }
        let mut names = names;
        for extension in ["exe", "cmd", "bat"] {
            let mut candidate = name.to_owned();
            candidate.push(".");
            candidate.push(extension);
            names.push(candidate);
        }
        names
    };
    names
}

fn default_codex_executable() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        PathBuf::from("codex.exe")
    }
    #[cfg(not(target_os = "windows"))]
    {
        PathBuf::from("codex")
    }
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

#[cfg(unix)]
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

#[cfg(not(unix))]
pub fn login_shell_path() -> Option<OsString> {
    None
}

#[cfg(unix)]
pub fn terminate_process(child: &mut std::process::Child) {
    kill_process_group(child.id(), libc::SIGKILL);
    // This is also a fallback when signalling the process group fails.
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

#[cfg(unix)]
pub fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(target_os = "windows")]
pub fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    // Prevent the Codex CLI and taskkill from flashing console windows when
    // CodexImage itself is built as a GUI executable.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(any(unix, target_os = "windows")))]
pub fn configure_process_group(_: &mut Command) {}

#[cfg(unix)]
pub fn kill_process_group(pid: u32, signal: i32) {
    if let Ok(pid) = i32::try_from(pid)
        && pid > 0
    {
        unsafe {
            libc::kill(-pid, signal);
        }
    }
}

#[cfg(target_os = "windows")]
pub fn kill_process_group(pid: u32, _: i32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if pid == 0 {
        return;
    }
    let mut command = Command::new("taskkill.exe");
    command
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    // Do not block the UI thread. The generation worker owns the Child handle
    // and will observe its exit, while /T closes every descendant as well.
    let _ = command.spawn();
}

#[cfg(not(any(unix, target_os = "windows")))]
pub fn kill_process_group(_: u32, _: i32) {}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::parse_login_shell_path;
    use super::{build_command_path, find_executable_on_path, read_tail};
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::ffi::OsString;
    use std::io::Cursor;
    #[cfg(not(target_os = "windows"))]
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
        let login_bin = directory.path().join("login-bin");
        let shared_bin = directory.path().join("shared-bin");
        let inherited_bin = directory.path().join("inherited-bin");
        let login = std::env::join_paths([&login_bin, &shared_bin]).unwrap();
        let inherited = std::env::join_paths([&shared_bin, &inherited_bin]).unwrap();

        let path = build_command_path(Some(login), Some(inherited), Some(directory.path()));
        let entries: Vec<_> = std::env::split_paths(&path).collect();

        assert_eq!(entries[0], login_bin);
        assert_eq!(entries[1], shared_bin);
        assert_eq!(entries[2], inherited_bin);
        assert_eq!(
            entries.iter().filter(|entry| *entry == &shared_bin).count(),
            1
        );
        assert!(entries.contains(&directory.path().join(".bun/bin")));
        #[cfg(not(target_os = "windows"))]
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

    #[cfg(target_os = "windows")]
    #[test]
    fn executable_resolution_finds_npm_command_shims() {
        let directory = TempDir::new().unwrap();
        let executable = directory.path().join("codex.cmd");
        std::fs::write(&executable, "@echo off\r\n").unwrap();
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
