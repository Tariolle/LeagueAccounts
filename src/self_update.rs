//! Replace the running portable executable with a verified download, and hand
//! over to the new process.
//!
//! Windows lets a running executable be renamed but not overwritten, so the
//! swap renames `<name>.exe` to `<name>.old.exe` and moves the download into
//! its place. An install receipt written before the swap records the target
//! build; the next process marks it healthy once it is ready, and only then,
//! a day later, removes the old copy.

use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `--updated-from <pid>`: the previous version started this process.
pub const UPDATED_FROM_FLAG: &str = "--updated-from";
/// Start minimized regardless of settings, as the window was before a restart.
pub const MINIMIZED_FLAG: &str = "--minimized";
/// A download older than this was abandoned (the app closed or crashed).
const STALE_DOWNLOAD: Duration = Duration::from_secs(60 * 60);
/// Keep the replaced executable this long after a healthy install, so a bad
/// release can still be rolled back by hand.
const ROLLBACK_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;

/// Serialize receipt read-modify-write cycles in this process, so a background
/// swap and a readiness report cannot interleave. Held only across disk work.
static RECEIPT_LOCK: Mutex<()> = Mutex::new(());

fn lock_receipts() -> MutexGuard<'static, ()> {
    RECEIPT_LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

/// What the last swap installed, persisted beside the executable. `healthy`
/// becomes true only after the new build has started and reported ready.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct InstallReceipt {
    pub target_version: String,
    pub installed_at_ms: u64,
    pub healthy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// The running executable, under whatever name the user gave it.
    pub exe: PathBuf,
    /// `<name>.exe.download`: on the same volume, so the swap is a rename.
    pub staged: PathBuf,
    /// `<name>.old.exe`: the replaced version until it has exited.
    pub old: PathBuf,
    /// `<name>.exe.receipt.json`: the target build and its health.
    pub receipt: PathBuf,
}

impl Paths {
    pub fn for_exe(exe: &Path) -> Option<Self> {
        let directory = exe.parent()?;
        let file_name = exe.file_name()?;
        let stem = exe.file_stem()?;
        Some(Self {
            exe: exe.to_owned(),
            staged: directory.join(concat(file_name, ".download")),
            old: directory.join(concat(stem, ".old.exe")),
            receipt: directory.join(concat(file_name, ".receipt.json")),
        })
    }

    pub fn current() -> Option<Self> {
        Self::for_exe(&std::env::current_exe().ok()?)
    }
}

fn concat(name: &OsStr, suffix: &str) -> OsString {
    let mut name = name.to_owned();
    name.push(suffix);
    name
}

/// Create the download file. Fails early in a read-only folder.
pub fn create_staged(paths: &Paths) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&paths.staged)
}

/// Move the running executable aside and the verified download into place.
///
/// The receipt is written first, so the process that starts from the new image
/// can prove which build was installed. Any failure restores both the original
/// executable and the previous receipt; a target is never left that could be
/// mistaken for a successful install.
pub fn swap(paths: &Paths, target_version: &str) -> io::Result<()> {
    if target_version.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "update target version is empty",
        ));
    }
    let _guard = lock_receipts();
    let previous_bytes = match fs::read(&paths.receipt) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    ensure_rollback_window_closed(paths, previous_bytes.as_deref())?;
    let previous = previous_bytes
        .as_deref()
        .map(|bytes| prepare_receipt(&paths.receipt, bytes))
        .transpose()?;
    let receipt = InstallReceipt {
        target_version: target_version.to_owned(),
        installed_at_ms: now_ms(),
        healthy: false,
    };
    store_receipt(&paths.receipt, &receipt)?;
    match swap_executable(paths) {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Err(restore_error) = restore_receipt(&paths.receipt, previous) {
                // Never retain the attempted target if restoring an older
                // receipt itself failed. Return the recovery failure instead.
                match fs::remove_file(&paths.receipt) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                return Err(restore_error);
            }
            Err(error)
        }
    }
}

/// A pending rollback copy may be overwritten only after its healthy install
/// has aged out of the rollback window. Without this gate, a second release
/// within 24 hours would delete the only copy of the last working build.
fn ensure_rollback_window_closed(paths: &Paths, previous: Option<&[u8]>) -> io::Result<()> {
    match fs::symlink_metadata(&paths.old) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let expired = previous
        .and_then(|bytes| serde_json::from_slice::<InstallReceipt>(bytes).ok())
        .is_some_and(|receipt| {
            receipt.installed_at_ms > 0
                && receipt.healthy
                && now_ms().saturating_sub(receipt.installed_at_ms) >= ROLLBACK_WINDOW_MS
        });
    if expired {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "previous version is still within its rollback window",
        ))
    }
}

fn swap_executable(paths: &Paths) -> io::Result<()> {
    // Keep any previous rollback copy until the entire new swap has succeeded.
    // In particular, an absent download must not delete the last good image.
    let old_backup = match fs::symlink_metadata(&paths.old) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "rollback copy is not a file"));
            }
            let parent = paths.old.parent().unwrap_or_else(|| Path::new("."));
            let folder = tempfile::tempdir_in(parent)?;
            let saved = folder.path().join("previous.old.exe");
            fs::rename(&paths.old, &saved)?;
            Some((folder, saved))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut original_restored = true;
    let result = (|| {
        fs::rename(&paths.exe, &paths.old)?;
        original_restored = false;
        if let Err(error) = fs::rename(&paths.staged, &paths.exe) {
            // Do not hide a failed recovery behind the original install error.
            fs::rename(&paths.old, &paths.exe)?;
            original_restored = true;
            return Err(error);
        }
        Ok(())
    })();
    if result.is_err() {
        if let Some((folder, saved)) = old_backup {
            if !original_restored {
                // The current image remains at `old`; preserve the earlier
                // backup too rather than overwriting either during recovery.
                let _ = folder.keep();
                return result;
            }
            if let Err(error) = fs::rename(&saved, &paths.old) {
                // A filesystem failure during recovery must not delete the
                // saved image when the temporary directory is dropped.
                let _ = folder.keep();
                return Err(error);
            }
        }
    }
    result
}

fn store_receipt(path: &Path, receipt: &InstallReceipt) -> io::Result<()> {
    let json = serde_json::to_vec(receipt).map_err(io::Error::other)?;
    store_receipt_bytes(path, &json)
}

fn store_receipt_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    prepare_receipt(path, bytes)?
        .persist(path)
        .map(|_| ())
        .map_err(|error| error.error)
}

fn prepare_receipt(path: &Path, bytes: &[u8]) -> io::Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // Unique sibling files avoid colliding with another instance. Dropping a
    // failed write removes its temporary file.
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    Ok(file)
}

fn restore_receipt(path: &Path, previous: Option<tempfile::NamedTempFile>) -> io::Result<()> {
    if let Some(file) = previous {
        return match file.persist(path) {
            Ok(_) => Ok(()),
            Err(error) => {
                // Preserve the prepared rollback data if the filesystem
                // refused even its atomic restoration.
                let _ = error.file.keep();
                Err(error.error)
            }
        };
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn read_receipt(path: &Path) -> Option<InstallReceipt> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Remove what an update left behind: the replaced version, once the new build
/// has reported healthy and its rollback window has passed, and a download
/// abandoned long enough ago that no other instance can still be writing it.
/// Only the two derived names are ever deleted.
pub fn clean_up(paths: &Paths, after_update: bool) {
    let _guard = lock_receipts();
    let rollback_expired = read_receipt(&paths.receipt).is_some_and(|receipt| {
        receipt.installed_at_ms > 0
            && receipt.healthy
            && now_ms().saturating_sub(receipt.installed_at_ms) >= ROLLBACK_WINDOW_MS
    });
    if rollback_expired {
        // The image of a process that has just exited can stay locked briefly.
        let attempts = if after_update { 10 } else { 1 };
        for attempt in 1..=attempts {
            match fs::remove_file(&paths.old) {
                Err(error) if error.kind() != io::ErrorKind::NotFound && attempt < attempts => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                _ => break,
            }
        }
    }
    let stale = fs::metadata(&paths.staged)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > STALE_DOWNLOAD));
    if stale {
        let _ = fs::remove_file(&paths.staged);
    }
}

fn flag_value<'a>(args: &'a [OsString], flag: &str) -> Option<&'a str> {
    let position = args.iter().position(|arg| arg == flag)?;
    args.get(position + 1)?.to_str()
}

/// The pid passed with `--updated-from`, if this process was started by an update.
pub fn previous_process(args: &[OsString]) -> Option<u32> {
    flag_value(args, UPDATED_FROM_FLAG)?.parse().ok()
}

/// The build the last swap installed, taken from the receipt beside the
/// executable. Healthy or not, this is what the new process compares with its
/// own version before reporting ready.
pub fn installed_target(paths: &Paths) -> Option<String> {
    read_receipt(&paths.receipt).map(|receipt| receipt.target_version)
}

/// Mark the installed target healthy after the process it started has passed
/// its readiness checks. The receipt is read and verified under the same lock
/// as `swap`, so a background install cannot change the target between the
/// caller's check and this write. A receipt for a different build is refused,
/// leaving its bytes untouched.
pub fn mark_healthy(paths: &Paths, current_version: &str) -> io::Result<()> {
    let _guard = lock_receipts();
    let mut receipt = read_receipt(&paths.receipt)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "install receipt is missing"))?;
    if receipt.target_version != current_version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "install receipt targets a different version",
        ));
    }
    receipt.healthy = true;
    store_receipt(&paths.receipt, &receipt)
}

/// Arguments for the new process: the original ones (so `--autostart` keeps
/// Start minimized working) without an earlier hand-over, plus this one.
pub fn restart_args(args: &[OsString], pid: u32, minimized: bool) -> Vec<OsString> {
    let mut kept = Vec::new();
    let mut skip_value = false;
    for arg in args {
        if std::mem::take(&mut skip_value) {
            continue;
        }
        if arg == UPDATED_FROM_FLAG {
            skip_value = true;
        } else if arg != MINIMIZED_FLAG {
            kept.push(arg.clone());
        }
    }
    kept.extend([UPDATED_FROM_FLAG.into(), pid.to_string().into()]);
    if minimized {
        kept.push(MINIMIZED_FLAG.into());
    }
    kept
}

/// Wait for the previous version to exit, so its files and WebView2 profile
/// are released before this process opens them. False on a timeout.
#[cfg(windows)]
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

    // SAFETY: a wait-only handle; an exited pid is reported as an error.
    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if raw.is_null() {
        return true;
    }
    // SAFETY: OpenProcess returned a new owned handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: the handle is valid for the duration of the wait.
    unsafe { WaitForSingleObject(handle.as_raw_handle(), millis) == WAIT_OBJECT_0 }
}

#[cfg(not(windows))]
pub fn wait_for_exit(_pid: u32, _timeout: Duration) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn folder_with_exe() -> (tempfile::TempDir, Paths) {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::for_exe(&directory.path().join("My Accounts.exe")).unwrap();
        fs::write(&paths.exe, "current").unwrap();
        (directory, paths)
    }

    const CURRENT: &str = env!("CARGO_PKG_VERSION");

    #[test]
    fn derives_names_from_the_executable_the_user_runs() {
        let paths = Paths::for_exe(Path::new(r"D:\Games\My Accounts.exe")).unwrap();
        assert_eq!(paths.staged, Path::new(r"D:\Games\My Accounts.exe.download"));
        assert_eq!(paths.old, Path::new(r"D:\Games\My Accounts.old.exe"));
        assert_eq!(paths.receipt, Path::new(r"D:\Games\My Accounts.exe.receipt.json"));
    }

    #[test]
    fn swap_installs_the_download_and_records_the_target_as_unhealthy() {
        let (_directory, paths) = folder_with_exe();
        fs::write(&paths.staged, "new").unwrap();
        swap(&paths, "3.2.0").unwrap();
        assert_eq!(fs::read_to_string(&paths.exe).unwrap(), "new");
        assert_eq!(fs::read_to_string(&paths.old).unwrap(), "current");
        assert!(!paths.staged.exists());
        let receipt = read_receipt(&paths.receipt).unwrap();
        assert_eq!(receipt.target_version, "3.2.0");
        assert!(!receipt.healthy);
        assert!(receipt.installed_at_ms > 0);
        assert_eq!(installed_target(&paths).as_deref(), Some("3.2.0"));
    }

    #[test]
    fn failed_swap_restores_the_executable_and_the_previous_receipt() {
        let (_directory, paths) = folder_with_exe();
        let previous = InstallReceipt {
            target_version: "3.1.0".to_owned(),
            installed_at_ms: 1_700_000_000_000,
            healthy: true,
        };
        store_receipt(&paths.receipt, &previous).unwrap();
        // No download: the second rename fails after the first succeeded.
        assert!(swap(&paths, "9.9.9").is_err());
        assert_eq!(fs::read_to_string(&paths.exe).unwrap(), "current");
        assert!(!paths.old.exists());
        assert_eq!(read_receipt(&paths.receipt), Some(previous));
    }

    #[test]
    fn failed_first_swap_leaves_no_pending_receipt() {
        let (_directory, paths) = folder_with_exe();
        assert!(swap(&paths, "9.9.9").is_err());
        assert!(!paths.receipt.exists());
        assert_eq!(fs::read_to_string(&paths.exe).unwrap(), "current");
    }

    #[test]
    fn swap_refuses_an_empty_target_version() {
        let (_directory, paths) = folder_with_exe();
        fs::write(&paths.staged, "new").unwrap();
        let error = swap(&paths, "  ").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_to_string(&paths.exe).unwrap(), "current");
        assert!(!paths.receipt.exists());
        assert!(paths.staged.exists());
    }

    #[test]
    #[cfg(windows)]
    fn swap_leaves_everything_in_place_while_an_old_copy_is_still_open() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_directory, paths) = folder_with_exe();
        fs::write(&paths.old, "still running").unwrap();
        fs::write(&paths.staged, "new").unwrap();
        let receipt = InstallReceipt {
            target_version: CURRENT.to_owned(),
            installed_at_ms: now_ms().saturating_sub(ROLLBACK_WINDOW_MS + 60_000),
            healthy: true,
        };
        store_receipt(&paths.receipt, &receipt).unwrap();
        let before = fs::read(&paths.receipt).unwrap();
        // Like a mapped executable image: no FILE_SHARE_DELETE.
        let _open = OpenOptions::new().read(true).share_mode(1).open(&paths.old).unwrap();
        assert!(swap(&paths, "9.9.9").is_err());
        assert_eq!(fs::read_to_string(&paths.exe).unwrap(), "current");
        assert_eq!(fs::read_to_string(&paths.staged).unwrap(), "new");
        assert_eq!(fs::read_to_string(&paths.old).unwrap(), "still running");
        assert_eq!(fs::read(&paths.receipt).unwrap(), before);
    }

    #[test]
    fn swap_refuses_to_overwrite_a_protected_rollback_copy() {
        let cases = [
            ("missing receipt", None),
            (
                "unhealthy install",
                Some(InstallReceipt {
                    target_version: CURRENT.to_owned(),
                    installed_at_ms: now_ms().saturating_sub(ROLLBACK_WINDOW_MS * 2),
                    healthy: false,
                }),
            ),
            (
                "healthy install within the window",
                Some(InstallReceipt {
                    target_version: CURRENT.to_owned(),
                    installed_at_ms: now_ms(),
                    healthy: true,
                }),
            ),
        ];
        for (label, receipt) in cases {
            let (_directory, paths) = folder_with_exe();
            fs::write(&paths.old, "rollback").unwrap();
            fs::write(&paths.staged, "newer").unwrap();
            if let Some(receipt) = &receipt {
                store_receipt(&paths.receipt, receipt).unwrap();
            }
            let before = fs::read(&paths.receipt).ok();
            let error = swap(&paths, "9.9.9").unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{label}");
            assert_eq!(fs::read(&paths.receipt).ok(), before, "{label}");
            assert_eq!(fs::read_to_string(&paths.old).unwrap(), "rollback", "{label}");
            assert_eq!(fs::read_to_string(&paths.exe).unwrap(), "current", "{label}");
            assert_eq!(fs::read_to_string(&paths.staged).unwrap(), "newer", "{label}");
        }
    }

    #[test]
    fn swap_proceeds_once_the_rollback_window_has_expired() {
        let (_directory, paths) = folder_with_exe();
        fs::write(&paths.old, "expired rollback").unwrap();
        fs::write(&paths.staged, "newer").unwrap();
        let previous = InstallReceipt {
            target_version: CURRENT.to_owned(),
            installed_at_ms: now_ms().saturating_sub(ROLLBACK_WINDOW_MS + 60_000),
            healthy: true,
        };
        store_receipt(&paths.receipt, &previous).unwrap();
        swap(&paths, "9.9.9").unwrap();
        assert_eq!(fs::read_to_string(&paths.exe).unwrap(), "newer");
        assert_eq!(fs::read_to_string(&paths.old).unwrap(), "current");
        assert_eq!(installed_target(&paths).as_deref(), Some("9.9.9"));
        assert!(!read_receipt(&paths.receipt).unwrap().healthy);
    }

    #[test]
    fn clean_up_keeps_the_previous_version_until_healthy_and_a_day_old() {
        let (directory, paths) = folder_with_exe();
        let unrelated = directory.path().join("notes.old.exe");
        fs::write(&unrelated, "keep").unwrap();
        fs::write(&paths.old, "replaced").unwrap();
        fs::write(&paths.staged, "fresh download").unwrap();

        clean_up(&paths, false);
        assert!(paths.old.exists(), "a missing receipt keeps the rollback copy");

        let now = now_ms();
        let unhealthy = InstallReceipt {
            target_version: CURRENT.to_owned(),
            installed_at_ms: now.saturating_sub(ROLLBACK_WINDOW_MS * 2),
            healthy: false,
        };
        store_receipt(&paths.receipt, &unhealthy).unwrap();
        clean_up(&paths, false);
        assert!(paths.old.exists(), "an unhealthy install keeps the rollback copy");

        let recent = InstallReceipt {
            target_version: CURRENT.to_owned(),
            installed_at_ms: now.saturating_sub(ROLLBACK_WINDOW_MS / 2),
            healthy: true,
        };
        store_receipt(&paths.receipt, &recent).unwrap();
        clean_up(&paths, false);
        assert!(paths.old.exists(), "the rollback window is still open");

        let expired = InstallReceipt {
            target_version: CURRENT.to_owned(),
            installed_at_ms: now.saturating_sub(ROLLBACK_WINDOW_MS + 60_000),
            healthy: true,
        };
        store_receipt(&paths.receipt, &expired).unwrap();
        clean_up(&paths, false);
        assert!(!paths.old.exists());
        assert!(paths.exe.exists() && unrelated.exists());
        assert!(paths.staged.exists(), "a fresh download is not an abandoned one");
    }

    #[test]
    fn clean_up_removes_only_an_abandoned_download() {
        let (directory, paths) = folder_with_exe();
        let unrelated = directory.path().join("notes.download");
        fs::write(&unrelated, "keep").unwrap();
        fs::write(&paths.staged, "another instance is downloading").unwrap();
        clean_up(&paths, false);
        assert!(paths.staged.exists());
        assert!(paths.exe.exists() && unrelated.exists());

        let abandoned = SystemTime::now() - STALE_DOWNLOAD * 2;
        File::options()
            .write(true)
            .open(&paths.staged)
            .unwrap()
            .set_modified(abandoned)
            .unwrap();
        clean_up(&paths, false);
        assert!(!paths.staged.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn mark_healthy_records_readiness_on_the_pending_receipt() {
        let (_directory, paths) = folder_with_exe();
        let pending = InstallReceipt {
            target_version: CURRENT.to_owned(),
            installed_at_ms: now_ms(),
            healthy: false,
        };
        store_receipt(&paths.receipt, &pending).unwrap();
        assert_eq!(installed_target(&paths).as_deref(), Some(CURRENT));
        assert!(!read_receipt(&paths.receipt).unwrap().healthy);
        mark_healthy(&paths, CURRENT).unwrap();
        assert!(read_receipt(&paths.receipt).unwrap().healthy);
        assert_eq!(installed_target(&paths).as_deref(), Some(CURRENT));
    }

    #[test]
    fn mark_healthy_refuses_another_target_without_touching_the_receipt() {
        let (_directory, paths) = folder_with_exe();
        let pending = InstallReceipt {
            target_version: "0.0.1".to_owned(),
            installed_at_ms: now_ms(),
            healthy: false,
        };
        store_receipt(&paths.receipt, &pending).unwrap();
        let before = fs::read(&paths.receipt).unwrap();
        let error = mark_healthy(&paths, CURRENT).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&paths.receipt).unwrap(), before);
        assert!(!read_receipt(&paths.receipt).unwrap().healthy);
    }

    #[test]
    fn mark_healthy_requires_an_install_receipt() {
        let (_directory, paths) = folder_with_exe();
        assert_eq!(installed_target(&paths), None);
        let error = mark_healthy(&paths, CURRENT).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn restart_keeps_startup_arguments_and_replaces_an_earlier_hand_over() {
        let original = args(&["--autostart", UPDATED_FROM_FLAG, "41", MINIMIZED_FLAG]);
        let restarted = restart_args(&original, 42, false);
        assert_eq!(restarted, args(&["--autostart", UPDATED_FROM_FLAG, "42"]));
        assert_eq!(
            restart_args(&[], 7, true),
            args(&[UPDATED_FROM_FLAG, "7", MINIMIZED_FLAG])
        );
        assert_eq!(previous_process(&restarted), Some(42));
        assert_eq!(previous_process(&args(&[UPDATED_FROM_FLAG])), None);
        assert_eq!(previous_process(&args(&["--autostart"])), None);
    }

    #[test]
    #[cfg(windows)]
    fn waits_for_the_previous_process_to_exit() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 2 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        assert!(!wait_for_exit(child.id(), Duration::from_millis(50)));
        assert!(wait_for_exit(child.id(), Duration::from_secs(10)));
        child.wait().unwrap();
        // Unknown pids (already exited and reaped) do not block startup.
        assert!(wait_for_exit(u32::MAX - 1, Duration::from_secs(10)));
    }
}
