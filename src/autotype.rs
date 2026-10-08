//! Types credentials into the previously focused window (the Riot Client).
//!
//! The sequence is: Alt+Tab back to the previous window, paste the account
//! ID, Tab, paste the password, Enter. Clipboard writes exclude credentials
//! from Windows history and cloud sync, then clear them after typing.

use std::thread;
use std::time::Duration;

/// Alt+Tab back to the previously focused window, then type credentials.
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
        // SendInput call. Pin that target for the entire typing sequence.
        let mut target = std::ptr::null_mut();
        let mut stable = 0;
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(100));
            let current = unsafe { GetForegroundWindow() };
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(current, &mut pid) };
            if current.is_null() || current == original || pid == 0 || pid == std::process::id() {
                target = std::ptr::null_mut();
                stable = 0;
            } else if current == target {
                stable += 1;
                if stable >= 3 {
                    return type_credentials(account_id, password, || unsafe {
                        GetForegroundWindow() == target
                    });
                }
            } else {
                target = current;
                stable = 0;
            }
        }
        false
    }
    #[cfg(not(windows))]
    type_credentials(account_id, password, || true)
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
pub(crate) fn wait_for_shortcut_release() -> bool {
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
pub(crate) fn wait_for_shortcut_release() -> bool {
    thread::sleep(Duration::from_millis(120));
    true
}

#[cfg(windows)]
pub(crate) fn tap_alt() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_MENU};
    let down = send_key(VK_MENU, 0);
    let up = send_key(VK_MENU, KEYEVENTF_KEYUP);
    down && up
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
}
