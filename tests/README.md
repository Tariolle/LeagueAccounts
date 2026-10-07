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

Logging tests cover rotation, retention, concurrent writes, and errors whose
Display implementation must never be called. `log_privacy.rs` initializes the
real file logger in an isolated process with temporary app data, then exercises
failed storage/import operations, a credential-bearing request error, and
panics containing dummy IDs/passwords (including a sensitive thread name).
It checks the readable log files for expected diagnostic events and verifies
that no dummy secrets, input JSON, or user paths reached the files.

Riot login tests simulate cold startup, a signed-out client, a signed-in client,
and lifecycle sign-out with a loopback HTTP server. The lifecycle API is the
readiness source; a missing RSO authorization is not treated as a login form.
Readiness must restart after an authentication interruption, window replacement,
or client endpoint change. Diagnostic tests verify that repeated waiting states
are logged once per transition and remain fixed categories without account data.

The endpoint contract is described in the [extracted Riot API documentation](https://github.com/KebsCS/lcu-and-riotclient-api/blob/main/riotclient/data_info.json).
The signed-out `PendingLoginStrategy` and signed-in `PendingProductContext`
responses are documented in [these desktop client observations](https://github.com/ArtemChig/LeagueSwitcher/blob/main/docs/RESEARCH.md).
These fixtures do not replace testing against the installed Riot Client.

Account storage tests inject credential failures without accessing the system
keyring. They cover skipped imports, duplicate batches, save and deletion
rollback, restoration of overwritten passwords, and temporary-file cleanup.
On Windows they also verify that a locked account file survives a failed
replacement intact.

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
