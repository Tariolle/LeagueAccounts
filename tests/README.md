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
Diagnostic tests verify that repeated waiting states are logged once per
transition and remain fixed categories without account data. Renderer tests
reject nonlocal debugger URLs and unknown authentication results.

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
5. With the Riot login method and a test account, try a cold start, a minimized
   client, switching accounts, and cancelling during setup or verification.
   Credentials must be submitted without keystrokes or foreground focus. CAPTCHA
   and MFA must appear in Riot Client. A rejected login must stop without retries.
   After success, cancellation, or failure, check that a normal Riot window is
   restored and its temporary debugger is gone. Verify the selected identity
   before launching the game. Logs must contain fixed categories only.

## Renderer login

`node scripts/try-riot-renderer-login.mjs <saved-account-id>` is a Windows
development prototype of the native Rust integration in the app. Close the normal
Riot Client and any game first. It starts Riot Services with `--headless`, runs
one Electron renderer, fills the native React form while minimized, and verifies
the signed-in username through the local API. An already-connected target account
is left alone. CAPTCHA/MFA, when required, stay in Riot's own UI for a human to
complete. The prototype does not retry rejected credentials automatically.

The debugger uses an allocated nonzero loopback port. Chromium's special
`--remote-debugging-port=0` launch sets `navigator.webdriver=true`; that setting
coincided with rejected authentication in the live experiment. After switching
to a nonzero port, a live login returned `success` on its first attempt without
a visible CAPTCHA. The target account remained connected after replacing the
debug renderer with a normal renderer, and no debugger remained enabled.
This is one successful end-to-end test, not a guarantee that CAPTCHA will never
be required. Riot's internal React handlers and module exports can change.

`--verify-fill-only` checks the DOM and React credential state without submitting
the login form. Diagnostics contain fixed categories and match booleans only;
credentials and authentication tokens are never printed. The experiment does
not modify Riot's installed files. The initial direct-auth and separate-browser
CAPTCHA prototypes were removed after validating the native-renderer approach.
