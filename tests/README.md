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

## Interactive Windows checks

Login regression tests in `src/autotype.rs` and `src/riot_client.rs` cover delayed
activation, refused activation, stable focus, cancellation, and stopping input
when focus changes or an input step fails. Geometry cases cover monitors left,
right, above, and below the primary origin at 100%, 125%, 150%, 200%, and 300%
scaling. A Windows API check verifies that window discovery restores the calling
thread's DPI context. These checks require no injected keys or real credentials;
they do not reproduce an actual mixed-DPI desktop or Riot's login form.

For the monitor-specific regression, test both login methods with Riot and
League Accounts on different monitors, including different scaling settings
and a monitor to the left of the primary. Repeat with Riot minimized. In the
previous-window method, focus the dummy form before returning to League
Accounts. Activation must complete before typing. If focus changes during
typing, the remaining steps must stop rather than continue in another window.

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
