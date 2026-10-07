//! Clipboard writes for credentials, shared by login and explicit copying.

pub fn set_private_text(
    clipboard: &mut arboard::Clipboard,
    text: &str,
) -> Result<(), arboard::Error> {
    #[cfg(windows)]
    let result = {
        use arboard::SetExtWindows;
        clipboard.set().exclude_from_monitoring().text(text)
    };
    #[cfg(not(windows))]
    let result = clipboard.set_text(text);

    // A failed privacy flag write may have already placed the text on the
    // clipboard. Remove it before reporting failure.
    if result.is_err() {
        let _ = clipboard.clear();
    }
    result
}
