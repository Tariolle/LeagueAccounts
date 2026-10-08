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
        thread::sleep(Duration::from_millis(400));
    }
    type_credentials(account_id, password)
}

/// Type into the focused window: paste ID, Tab, paste password, Enter.
/// The clipboard is cleared afterwards.
pub fn type_credentials(account_id: &str, password: &str) -> bool {
    if !wait_for_shortcut_release() {
        return false;
    }
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return false;
    };
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_RETURN, VK_TAB};

        let typed = (|| {
            // arboard updates the process clipboard asynchronously on some
            // Windows versions; let each value become visible before Ctrl+V.
            if crate::clipboard::set_private_text(&mut clipboard, account_id).is_err() {
                return false;
            }
            thread::sleep(Duration::from_millis(60));
            if !paste_current_clipboard() {
                return false;
            }
            thread::sleep(Duration::from_millis(60));
            let tab_down = send_key(VK_TAB, 0);
            let tab_up = send_key(VK_TAB, KEYEVENTF_KEYUP);
            if !tab_down || !tab_up {
                return false;
            }
            if !password.is_empty() {
                thread::sleep(Duration::from_millis(60));
                if crate::clipboard::set_private_text(&mut clipboard, password).is_err() {
                    return false;
                }
                thread::sleep(Duration::from_millis(60));
                if !paste_current_clipboard() {
                    return false;
                }
            }
            thread::sleep(Duration::from_millis(60));
            let enter_down = send_key(VK_RETURN, 0);
            let enter_up = send_key(VK_RETURN, KEYEVENTF_KEYUP);
            // Let the target read the clipboard before it is wiped.
            thread::sleep(Duration::from_millis(250));
            enter_down && enter_up
        })();
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

#[cfg(windows)]
fn wait_for_shortcut_release() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_SHIFT, VK_V,
    };

    for _ in 0..100 {
        let shortcut_still_down = [VK_CONTROL, VK_SHIFT, VK_V].into_iter().any(|key| {
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
