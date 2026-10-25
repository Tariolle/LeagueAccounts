//! Writing the game's language into Riot's product settings file.
//!
//! The Riot Client builds the game's `--locale=` argument from
//! `<product>.live.product_settings.yaml`, so that file — not the local API —
//! is the value the game actually starts with. Two details make a single write
//! unreliable:
//!
//! - The client rewrites the file itself (on launch and after updates), so a
//!   value written while it is running can be replaced. The caller therefore
//!   writes, launches, and checks again afterwards instead of writing once.
//! - The file is often read-only, which a plain write fails on. The attribute
//!   is cleared, the file rewritten, and the attribute restored — the same
//!   thing LOLauncher does with `chmod`.
//!
//! Only the `settings.locale` line is touched. `available_locales` is left
//! alone: it describes what Riot publishes, and rewriting it would make the
//! file disagree with the registry without changing what the client accepts.

use std::path::{Path, PathBuf};

/// Where Riot keeps per-product metadata.
fn metadata_root() -> Option<PathBuf> {
    let program_data = std::env::var_os("ProgramData").map(PathBuf::from)?;
    Some(program_data.join(r"Riot Games\Metadata"))
}

/// The settings file for a product's live patchline.
pub fn settings_file(product: &str) -> Option<PathBuf> {
    let path = metadata_root()?
        .join(format!("{product}.live"))
        .join(format!("{product}.live.product_settings.yaml"));
    path.exists().then_some(path)
}

/// The locale currently written in the file, if it can be read.
pub fn read(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    locale_line(&contents).map(|(_, value)| value)
}

/// Replace the `settings.locale` value, leaving the rest of the file as-is.
///
/// Returns `true` only when the file now reads back as the requested locale.
/// A rewrite of the whole document is deliberately avoided: the client writes
/// its own ordering and quoting, and a YAML round-trip would churn every line.
pub fn write(path: &Path, locale: &str) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some((line, current)) = locale_line(&contents) else {
        return false;
    };
    if current == locale {
        return true;
    }
    let replacement = format!("{locale}\"");
    let mut lines: Vec<String> = contents.lines().map(str::to_owned).collect();
    // The line ends with `"xx_YY"`; swap only the value inside the quotes.
    let updated = lines[line].replace(&format!("{current}\""), &replacement);
    lines[line] = updated;
    let mut text = lines.join("\n");
    if contents.ends_with('\n') {
        text.push('\n');
    }
    write_through_read_only(path, &text) && read(path).as_deref() == Some(locale)
}

/// Write even when the file is marked read-only, restoring the attribute.
///
/// Riot sets the attribute so the language survives updates; leaving it set is
/// as much a part of the contract as the content.
fn write_through_read_only(path: &Path, text: &str) -> bool {
    let was_read_only = std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly());
    if was_read_only {
        set_read_only(path, false);
    }
    let written = std::fs::write(path, text).is_ok();
    if was_read_only {
        // Restore even when the write failed: leaving Riot's attribute cleared
        // would let the next game update overwrite the user's language.
        set_read_only(path, true);
    }
    written
}

/// Toggle the read-only file attribute.
///
/// `Permissions::set_readonly(false)` would work on Windows but is deprecated
/// there for a reason: on Unix it clears every write bit, and the pair of calls
/// here would turn a file's mode into something the user did not choose. Riot
/// only runs on Windows, so the attribute is set directly.
#[cfg(windows)]
fn set_read_only(path: &Path, read_only: bool) {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    let Ok(metadata) = std::fs::metadata(path) else { return };
    let attributes = metadata.file_attributes();
    let wanted = if read_only {
        attributes | FILE_ATTRIBUTE_READONLY
    } else {
        attributes & !FILE_ATTRIBUTE_READONLY
    };
    if wanted == attributes {
        return;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call.
    unsafe {
        windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), wanted);
    }
}

#[cfg(not(windows))]
fn set_read_only(path: &Path, read_only: bool) {
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(read_only);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

/// Line index and value of `settings.locale`.
///
/// The file groups `locale` under `settings:`; `default_locale` and
/// `available_locales` under `locale_data:` come first, so the search starts at
/// the `settings:` block rather than matching the first `locale` it sees.
fn locale_line(contents: &str) -> Option<(usize, String)> {
    let start = contents.lines().position(|line| line.starts_with("settings:"))?;
    for (index, line) in contents.lines().enumerate().skip(start + 1) {
        // A top-level key ends the block.
        if !line.starts_with(' ') && !line.trim().is_empty() {
            break;
        }
        let trimmed = line.trim_start();
        let Some(value) = trimmed.strip_prefix("locale:") else {
            continue;
        };
        let value = value.trim();
        let value = value.strip_prefix('"')?.strip_suffix('"')?;
        return Some((index, value.to_owned()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like the real file: `locale_data` first, `settings` last.
    fn sample() -> String {
        [
            "locale_data:",
            "    available_locales:",
            "    - \"zh_MY\"",
            "    - \"zh_TW\"",
            "    default_locale: \"zh_TW\"",
            "settings:",
            "    create_shortcut: false",
            "    locale: \"zh_TW\"",
            "    shortcut_name: \"League of Legends.lnk\"",
        ]
        .join("\n")
            + "\n"
    }

    #[test]
    fn reads_the_locale_under_settings_not_the_default() {
        let contents = sample();
        let (line, value) = locale_line(&contents).expect("locale line");
        assert_eq!(value, "zh_TW");
        assert_eq!(contents.lines().nth(line).unwrap().trim(), "locale: \"zh_TW\"");
    }

    #[test]
    fn rewrites_the_value_and_keeps_every_other_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("product_settings.yaml");
        std::fs::write(&path, sample()).unwrap();

        assert!(write(&path, "zh_CN"));
        assert_eq!(read(&path).as_deref(), Some("zh_CN"));

        // Only the locale line changed; the default and the list are intact.
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("default_locale: \"zh_TW\""));
        assert!(after.contains("    - \"zh_MY\""));
        assert!(after.contains("    - \"zh_TW\""));
        assert!(after.contains("locale: \"zh_CN\""));
        assert_eq!(after.lines().count(), sample().lines().count());
    }

    #[test]
    fn writes_through_a_read_only_file_and_restores_the_attribute() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("product_settings.yaml");
        std::fs::write(&path, sample()).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();

        assert!(write(&path, "zh_MY"));
        assert_eq!(read(&path).as_deref(), Some("zh_MY"));
        assert!(
            std::fs::metadata(&path).unwrap().permissions().readonly(),
            "the read-only attribute must survive the write"
        );
    }

    #[test]
    fn a_file_without_a_settings_block_is_left_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("product_settings.yaml");
        let only_data = "locale_data:\n    default_locale: \"zh_TW\"\n";
        std::fs::write(&path, only_data).unwrap();

        assert!(!write(&path, "zh_CN"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), only_data);
    }
}
