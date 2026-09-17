# Regression verification

Run `cargo test --locked --all-targets` and
`cargo clippy --locked --all-targets -- -D warnings` on Windows. The workflow in
`.github/workflows/rust.yml` also builds the release executable.

The UI tests cover the real headless egui application with a temporary account
file and a rank provider that refuses network access. Parser tests use static
fixtures, including the original multiline Python level fixture.

Windows-only native shortcut tests install a keyboard hook restricted to the
test's own thread. They call the event handler directly and save/restore that
thread's keyboard state to test modifier lookup, peek filtering, queued requests,
focus changes, and cleanup. They do not create native windows, inject keystrokes,
read or modify the clipboard, or invoke auto-type. These tests do not replace an
interactive check of native event delivery and the complete login sequence.

## Interactive Windows checks

Use a disposable dummy account and a test window, never real credentials.

1. Select an account, then focus Search, an add-account input, or the description
   editor. Delete must edit text without removing the account. Ctrl+C must copy
   selected text. Return focus to an account-table cell and verify Ctrl+C
   alternates between account ID and password. A fresh unmodified Delete press
   may delete the selected account; holding it must not cascade through the
   list. Modified Delete presses must not invoke account deletion.
2. Put a test login form with two text fields in the previous window. Select the
   dummy account, empty the clipboard, and press Ctrl+Shift+V. Verify the switch,
   ID, Tab, password, and Enter sequence. Repeat with a nonempty clipboard and
   with an app text field focused: the chord must not paste into that field.
3. Hold the chord longer than one second. Auto-type must report failure rather
   than injecting input with held modifiers. Release and press again; only one
   sequence should run. Verify normal Ctrl+V still works and the shortcut does
   not intercept another application or a native file dialog.
4. Verify Add New Account, Multi Add, and Friend Elo with an empty account list
   and a long list. Reduce the window height and scroll the main content to the
   controls below the independently scrolling table.
