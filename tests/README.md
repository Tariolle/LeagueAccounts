# Regression verification

Run `cargo test --locked --workspace --all-targets` and
`cargo clippy --locked --workspace --all-targets -- -D warnings` on Windows.
Build the UI first (`npm ci` and `npm run build`) because the Tauri shell embeds
`dist` at compile time. The workflow in `.github/workflows/rust.yml` runs these
checks and also builds the release executable.

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

Account storage tests inject credential failures with an in-memory store.
They cover skipped imports, duplicate batches, save and deletion
rollback, restoration of overwritten passwords, and temporary-file cleanup.
On Windows they also verify that a locked account file survives a failed
replacement intact.

A Windows regression also exercises the native credential adapter with dummy
entries under a unique test service. It verifies that a failed deletion restores
the selected account without changing another account's password or a stale
shared entry. The dummy entries are removed even if an assertion fails.

## Login focus and DPI regressions

The typing tests exercise the production sequencing helper with mocked clipboard
and input operations. They cover focus/cancellation guards before every action,
focus loss during the clipboard preparation delay, failed clipboard writes,
rejected input, field order, and an empty password. Actual Windows typing keeps
using `clipboard::set_private_text`; ordinary clipboard writes must not replace
it when changing the focus logic.

Activation tests cover delayed/refused requests, stable focus, bounded retries,
and cancellation, including cancellation during a focus observation. The newer
lifecycle, endpoint, identity, and readiness-reset tests remain in place.

Window geometry is queried under a scoped DPI-unaware thread context, making
`GetWindowRect` return 96-DPI logical coordinates. The 600x400 threshold uses the
same units; no physical-bounds/`GetDpiForWindow` division is involved. Geometry
tests cover threshold boundaries, negative origins, invalid rectangles, and
wide differences. Native Windows tests verify context restoration on failed
queries and discovery, and compare actual hidden-window bounds across unaware,
system-aware, per-monitor-aware, and per-monitor-v2 target/caller contexts.
They create and destroy only their own hidden windows; they do not activate
windows, inject keys, or read/write the clipboard.

These tests run on the available desktop and do not emulate a real mixed-DPI
monitor layout or Riot's login form. The interactive checks below remain needed.
The relevant Windows contracts are [GetWindowRect](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowrect),
[coordinate virtualization](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-physicaltologicalpointforpermonitordpi),
and [GetDpiForWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdpiforwindow).

## Interactive Windows checks

Use a disposable dummy account and a test window, never real credentials.

For the monitor regression, test both login methods with Riot and League
Accounts on different displays. Cover 100%, 125%, 150%, 200%, and 300% scaling,
a secondary monitor to the left/above the primary, and minimized restoration.
Include system DPI at 200% with a 100% secondary display and a system-DPI-aware
dummy form on that secondary display, not only per-monitor-aware targets.
A logical 800x600 form must not be rejected as 400x300. In the previous-window
method, focus the dummy form before returning to League Accounts.

Activation must finish before typing. Change focus or cancel while typing:
remaining steps must stop, without reactivating the target or continuing in the
new window. Repeat during the clipboard preparation delay where practical.
Enable Windows clipboard history and verify that neither dummy credential is
added to Win+V history and that the current clipboard is cleared after successful
and aborted typing. Also repeat cold-start, account-switch, and already-signed-in
flows to verify that the lifecycle/endpoint/identity safeguards are retained.

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
