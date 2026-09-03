//! Small operating-system adapters used by storage and the desktop UI.

use std::io;
use std::path::{Path, PathBuf};

/// Chooses the native data directory while preserving data created by the
/// legacy Electron application when it is the only existing installation.
pub(crate) fn data_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let base = dirs::data_dir()?;
        Some(prefer_existing_legacy(
            base.join("CodexImage").join("data"),
            base.join("codeximage").join("data"),
        ))
    }

    #[cfg(target_os = "windows")]
    {
        let native = dirs::data_local_dir()?.join("CodexImage").join("data");
        let legacy = dirs::data_dir()?.join("codeximage").join("data");
        Some(prefer_existing_legacy(native, legacy))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::data_dir().map(|base| base.join("codeximage"))
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn prefer_existing_legacy(native: PathBuf, legacy: PathBuf) -> PathBuf {
    if !native.join("boards.json").exists() && legacy.join("boards.json").exists() {
        legacy
    } else {
        native
    }
}

/// Opens a file in the operating system's default application.
pub(crate) fn open_path(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let operation: Vec<u16> = "open\0".encode_utf16().collect();
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: every string is NUL-terminated and remains alive for the
        // duration of the synchronous ShellExecuteW call.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                path.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if result as isize > 32 {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "Windows could not open the file (ShellExecuteW code {})",
                result as isize
            )))
        }
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
}

/// Replaces `destination` with an adjacent temporary file. Windows' standard
/// `rename` refuses to overwrite an existing file, unlike POSIX rename.
pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };

        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: both path buffers are valid, NUL-terminated UTF-16 strings.
        if unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::fs::rename(source, destination)
    }
}

/// Flushes the containing directory where the platform supports opening a
/// directory as a file. MoveFileExW's WRITE_THROUGH flag provides the Windows
/// equivalent for the replacement itself.
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::File;
        File::open(path)?.sync_all()
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::prefer_existing_legacy;

    #[test]
    fn legacy_data_is_only_reused_when_native_data_does_not_exist() {
        let directory = tempfile::TempDir::new().unwrap();
        let native = directory.path().join("native");
        let legacy = directory.path().join("legacy");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("boards.json"), "[]").unwrap();

        assert_eq!(
            prefer_existing_legacy(native.clone(), legacy.clone()),
            legacy
        );

        std::fs::create_dir_all(&native).unwrap();
        std::fs::write(native.join("boards.json"), "[]").unwrap();
        assert_eq!(prefer_existing_legacy(native.clone(), legacy), native);
    }
}
