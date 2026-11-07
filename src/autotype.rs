//! Types credentials into the previously focused window (the Riot Client).
//!
//! The sequence is: validate the previously focused window against Riot's own
//! install metadata, Alt+Tab back to it, paste the account ID, Tab, paste the
//! password, Enter. The target process is pinned before the first keystroke,
//! and every later input rechecks it. Clipboard writes exclude credentials
//! from Windows history and cloud sync, then clear them after typing.

#[cfg(any(windows, test))]
use std::ffi::OsString;
#[cfg(any(windows, test))]
use std::path::Path;
use std::thread;
use std::time::Duration;

/// Alt+Tab back to the previously focused window, then type credentials.
///
/// The target is pinned after the switch: its image path must be the Riot
/// Client of the installation that Riot's `client_path` metadata names, and
/// one process handle keeps every later keystroke tied to that same, still
/// running process. A window from any other program is never typed into.
pub fn auto_type_credentials(account_id: &str, password: &str) -> bool {
    // Do not inject input while a physical shortcut key is still held.
    if !wait_for_shortcut_release() {
        return false;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_MENU, VK_TAB};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };

        // Riot's install metadata is the trusted source for where the client
        // lives. Without it the previous window cannot be identified.
        let Some(services) = crate::riot_client::client_path() else {
            return false;
        };

        let original = unsafe { GetForegroundWindow() };
        if original.is_null() {
            return false;
        }

        // SendInput is used instead of keybd_event because it preserves the
        // Alt+Tab transition and is handled consistently by modern Windows
        // applications (including the Riot Client).
        if !send_key(VK_MENU, 0) {
            return false;
        }
        // Give the shell a moment to register Alt before pressing Tab. A
        // single back-to-back SendInput batch is intermittently ignored by
        // the Windows task switcher on slower desktops.
        thread::sleep(Duration::from_millis(35));
        if !send_key(VK_TAB, 0) {
            let _ = send_key(VK_MENU, KEYEVENTF_KEYUP);
            return false;
        }
        thread::sleep(Duration::from_millis(35));
        let tab_released = send_key(VK_TAB, KEYEVENTF_KEYUP);
        let alt_released = send_key(VK_MENU, KEYEVENTF_KEYUP);
        if !tab_released || !alt_released {
            return false;
        }
        // Wait for an actual switch to another process, not just a successful
        // SendInput call. Once stable, pin that process for the whole typing
        // sequence and refuse it unless it is this installation's client.
        let mut target = None;
        let mut stable = 0;
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(100));
            let current = unsafe { GetForegroundWindow() };
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(current, &mut pid) };
            if current.is_null() || current == original || pid == 0 || pid == std::process::id() {
                target = None;
                stable = 0;
                continue;
            }
            match target {
                Some((window, seen_pid)) if window == current && seen_pid == pid => {
                    stable += 1;
                    if stable < 3 {
                        continue;
                    }
                    let Some(process) = target_process::open(pid) else {
                        return false;
                    };
                    let valid = process.alive()
                        && process
                            .image_path()
                            .is_some_and(|image| is_riot_client_executable(&image, &services));
                    if !valid {
                        return false;
                    }
                    return type_credentials(account_id, password, || {
                        process.alive() && process.focused(current)
                    });
                }
                _ => {
                    target = Some((current, pid));
                    stable = 0;
                }
            }
        }
        false
    }
    #[cfg(not(windows))]
    type_credentials(account_id, password, || true)
}

/// Whether `image` is the Riot Client executable of the installation that
/// `services` belongs to. Riot's metadata names `RiotClientServices.exe`;
/// newer installs run `RiotClientElectron\Riot Client.exe`, and older ones
/// `RiotClientUx.exe` beside the service.
#[cfg(any(windows, test))]
fn is_riot_client_executable(image: &Path, services: &Path) -> bool {
    let Some(root) = services.parent().map(path_components) else {
        return false;
    };
    let image = path_components(image);
    let Some((name, parent)) = image.split_last() else {
        return false;
    };
    let name = name.to_string_lossy();
    let mut electron = root.clone();
    electron.push(OsString::from("riotclientelectron"));
    match name.as_ref() {
        "riot client.exe" => parent == root.as_slice() || parent == electron.as_slice(),
        "riotclientux.exe" => parent == root.as_slice(),
        _ => false,
    }
}

/// Compare paths the way Windows resolves them: case-insensitively and with
/// either separator, while still requiring whole path components to match.
#[cfg(any(windows, test))]
fn path_components(path: &Path) -> Vec<OsString> {
    path.components()
        .map(|component| component.as_os_str().to_ascii_lowercase())
        .collect()
}

/// One process handle per login target, so every keystroke is checked against
/// the same process rather than a pid that could be reused.
#[cfg(windows)]
mod target_process {
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        GetProcessId, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    pub(super) struct PinnedProcess {
        handle: HANDLE,
        pid: u32,
    }

    pub(super) fn open(pid: u32) -> Option<PinnedProcess> {
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return None;
        }
        if unsafe { GetProcessId(handle) } != pid {
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(PinnedProcess { handle, pid })
    }

    impl PinnedProcess {
        pub(super) fn alive(&self) -> bool {
            unsafe { WaitForSingleObject(self.handle, 0) == WAIT_TIMEOUT }
        }

        pub(super) fn image_path(&self) -> Option<PathBuf> {
            let mut buffer = [0u16; 32_768];
            let mut length = buffer.len() as u32;
            let ok = unsafe {
                QueryFullProcessImageNameW(self.handle, 0, buffer.as_mut_ptr(), &mut length)
            };
            if ok == 0 {
                return None;
            }
            Some(PathBuf::from(String::from_utf16_lossy(
                &buffer[..length as usize],
            )))
        }

        pub(super) fn focused(&self, expected: windows_sys::Win32::Foundation::HWND) -> bool {
            let window = unsafe { GetForegroundWindow() };
            if window.is_null() || window != expected {
                return false;
            }
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(window, &mut pid) };
            pid == self.pid
        }
    }

    impl Drop for PinnedProcess {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.handle) };
        }
    }
}

/// Type into the focused window: paste ID, Tab, paste password, Enter.
/// On Windows, stop when the guard rejects input and clear the clipboard
/// after every attempted sequence, including failed or cancelled sequences.
pub fn type_credentials(
    account_id: &str,
    password: &str,
    mut can_type: impl FnMut() -> bool,
) -> bool {
    if !wait_for_shortcut_release() || !can_type() {
        return false;
    }
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return false;
    };
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_RETURN, VK_TAB};

        let typed = type_credentials_with(
            account_id,
            password,
            &mut can_type,
            |value| crate::clipboard::set_private_text(&mut clipboard, value).is_ok(),
            |action| match action {
                TypingAction::Paste(_) => paste_current_clipboard(),
                TypingAction::Tab | TypingAction::Enter => {
                    let key = if action == TypingAction::Tab { VK_TAB } else { VK_RETURN };
                    let down = send_key(key, 0);
                    let up = send_key(key, KEYEVENTF_KEYUP);
                    down && up
                }
            },
            || thread::sleep(Duration::from_millis(60)),
        );
        // Let the target read the clipboard before it is wiped, even when
        // a later action fails. Private writes also exclude history/cloud sync.
        thread::sleep(Duration::from_millis(250));
        let _ = clipboard.clear();
        typed
    }
    #[cfg(not(windows))]
    {
        clipboard
            .set_text(format!(
                "{account_id}
{password}"
            ))
            .is_ok()
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypingAction<'a> {
    Paste(&'a str),
    Tab,
    Enter,
}

#[cfg(any(windows, test))]
fn type_credentials_with<'a>(
    account_id: &'a str,
    password: &'a str,
    mut can_type: impl FnMut() -> bool,
    mut prepare_paste: impl FnMut(&str) -> bool,
    mut send: impl FnMut(TypingAction<'a>) -> bool,
    mut wait: impl FnMut(),
) -> bool {
    for action in [
        TypingAction::Paste(account_id),
        TypingAction::Tab,
        TypingAction::Paste(password),
        TypingAction::Enter,
    ] {
        if matches!(action, TypingAction::Paste(value) if value.is_empty()) {
            continue;
        }
        if !can_type() {
            return false;
        }
        if let TypingAction::Paste(value) = action {
            if !prepare_paste(value) {
                return false;
            }
            // Let the clipboard update become visible. Recheck after the wait:
            // focus or cancellation may have changed since preparation began.
            wait();
            if !can_type() {
                return false;
            }
        }
        if !send(action) {
            return false;
        }
        wait();
    }
    true
}

#[cfg(windows)]
fn wait_for_shortcut_release() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
    };

    for _ in 0..100 {
        let shortcut_still_down = [VK_CONTROL, VK_SHIFT, VK_V, VK_MENU, VK_LWIN, VK_RWIN]
            .into_iter()
            .any(|key| {
                // GetAsyncKeyState's high bit is set while the physical key is
                // down. The low bit is intentionally ignored because it reports
                // historical presses rather than current modifier state.
                unsafe { GetAsyncKeyState(key as i32) < 0 }
            });
        if !shortcut_still_down {
            thread::sleep(Duration::from_millis(20));
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[cfg(not(windows))]
fn wait_for_shortcut_release() -> bool {
    thread::sleep(Duration::from_millis(120));
    true
}

#[cfg(windows)]
fn send_key(virtual_key: u16, flags: u32) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    };

    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) == 1 }
}

#[cfg(windows)]
fn paste_current_clipboard() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_CONTROL};
    // Always send both key-up events, even if a key-down event is rejected,
    // so a transient SendInput failure cannot leave Ctrl or V stuck down.
    let control_down = send_key(VK_CONTROL, 0);
    let v_down = send_key(b'V' as u16, 0);
    let v_up = send_key(b'V' as u16, KEYEVENTF_KEYUP);
    let control_up = send_key(VK_CONTROL, KEYEVENTF_KEYUP);
    control_down && v_down && v_up && control_up
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn typing_stops_before_each_step_if_focus_is_lost_or_cancelled() {
        for lost_at in 0..4 {
            let completed = Cell::new(0);
            let result = type_credentials_with(
                "dummy",
                "dummy-password",
                || completed.get() < lost_at,
                |_| true,
                |_| {
                    completed.set(completed.get() + 1);
                    true
                },
                || {},
            );
            assert!(!result);
            assert_eq!(completed.get(), lost_at);
        }
    }

    #[test]
    fn focus_loss_during_clipboard_preparation_prevents_the_paste() {
        for lost_on_write in [1, 2] {
            let focused = Cell::new(true);
            let writes = Cell::new(0);
            let mut sent = Vec::new();
            let result = type_credentials_with(
                "dummy",
                "dummy-password",
                || focused.get(),
                |_| {
                    writes.set(writes.get() + 1);
                    true
                },
                |action| {
                    sent.push(action);
                    true
                },
                || {
                    if writes.get() == lost_on_write {
                        focused.set(false);
                    }
                },
            );
            assert!(!result);
            assert_eq!(writes.get(), lost_on_write);
            let expected = if lost_on_write == 1 {
                vec![]
            } else {
                vec![TypingAction::Paste("dummy"), TypingAction::Tab]
            };
            assert_eq!(sent, expected);
        }
    }

    #[test]
    fn failed_private_clipboard_write_prevents_further_input() {
        for failed_write in [1, 2] {
            let mut writes = 0;
            let mut sent = Vec::new();
            let result = type_credentials_with(
                "dummy",
                "dummy-password",
                || true,
                |_| {
                    writes += 1;
                    writes < failed_write
                },
                |action| {
                    sent.push(action);
                    true
                },
                || {},
            );
            assert!(!result);
            assert_eq!(writes, failed_write);
            assert_eq!(sent.len(), if failed_write == 1 { 0 } else { 2 });
            assert!(!sent.contains(&TypingAction::Paste("dummy-password")));
        }
    }

    #[test]
    fn rejected_input_is_not_retried_or_followed_by_more_input() {
        for failed_at in 0..4 {
            let mut attempted = 0;
            let result = type_credentials_with(
                "dummy",
                "dummy-password",
                || true,
                |_| true,
                |_| {
                    attempted += 1;
                    attempted <= failed_at
                },
                || {},
            );
            assert!(!result);
            assert_eq!(attempted, failed_at + 1);
        }
    }

    #[test]
    fn typing_preserves_field_order_and_supports_an_empty_password() {
        for password in ["dummy-password", ""] {
            let mut actions = Vec::new();
            let mut prepared = Vec::new();
            assert!(type_credentials_with(
                "dummy",
                password,
                || true,
                |value| {
                    prepared.push(value.to_owned());
                    true
                },
                |action| {
                    actions.push(action);
                    true
                },
                || {},
            ));
            let mut expected = vec![TypingAction::Paste("dummy"), TypingAction::Tab];
            let mut expected_prepared = vec!["dummy"];
            if !password.is_empty() {
                expected.push(TypingAction::Paste("dummy-password"));
                expected_prepared.push("dummy-password");
            }
            expected.push(TypingAction::Enter);
            assert_eq!(actions, expected);
            assert_eq!(prepared, expected_prepared);
        }
    }

    fn test_path(parts: &[&str]) -> std::path::PathBuf {
        parts.iter().copied().collect()
    }

    #[test]
    fn only_the_client_under_the_installed_riot_folder_is_accepted() {
        let services = test_path(&["Riot Games", "Riot Client", "RiotClientServices.exe"]);
        for image in [
            test_path(&["Riot Games", "Riot Client", "RiotClientElectron", "Riot Client.exe"]),
            test_path(&["Riot Games", "Riot Client", "Riot Client.exe"]),
            test_path(&["RIOT GAMES", "RIOT CLIENT", "RIOTCLIENTUX.EXE"]),
        ] {
            assert!(is_riot_client_executable(&image, &services), "{}", image.display());
        }
        for image in [
            test_path(&["Riot Games", "Riot Client", "RiotClientServices.exe"]),
            test_path(&["Riot Games", "Riot Client", "Another.exe"]),
            test_path(&["Riot Games", "Riot Client", "nested", "Riot Client.exe"]),
            test_path(&["Riot Games", "Riot Client", "RiotClientElectron", "Another.exe"]),
            test_path(&["Riot Games", "Riot Client Evil", "Riot Client.exe"]),
            test_path(&["Elsewhere", "Riot Client.exe"]),
            test_path(&["Riot Games", "Riot Client", "RiotClientElectron", "..", "RiotClientUx.exe"]),
        ] {
            assert!(!is_riot_client_executable(&image, &services), "{}", image.display());
        }
        #[cfg(windows)]
        assert!(is_riot_client_executable(
            Path::new(r"C:/Riot Games/Riot Client/RiotClientElectron/Riot Client.exe"),
            Path::new(r"C:\Riot Games\Riot Client\RiotClientServices.exe"),
        ));
    }
}
