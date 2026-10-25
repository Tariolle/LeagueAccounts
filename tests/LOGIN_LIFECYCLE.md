# Riot login lifetime regressions

Run on Windows 10 or 11:

```text
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
npm test
npm run build
```

The workspace test command includes the Tauri shell's login/exit controller.
These automated tests use dummy subprocesses and simulated service startup;
they do not read saved credentials, contact Riot, or terminate installed games.

## Automated coverage

- `src-tauri/src/login_control.rs` checks exclusive login admission, cancellation
  reset, queued-worker shutdown, repeated close requests, waiting until cleanup
  finishes, and cleanup during unwinding. Exit permanently closes admission, so
  a queued or subsequent login cannot clear an exit-triggered cancellation.
- `src/riot_renderer.rs` checks stale-lockfile startup, restoration when the
  selected account was already connected and no game is launched, early errors,
  cancellation, unwinding, failed startup, and existing normal/headless clients.
  A launched game replaces the temporary renderer without reopening Riot's
  window; a failed or cancelled launch reopens it once.
  The service-launch trace verifies that a normal window is opened before the
  initial authentication wait, including when only a headless service exists.
  Process ownership compares Windows path components: forward slashes in Riot's
  install metadata still match native process paths, while sibling directories
  and other drives are excluded.
- `src/riot_client.rs` checks the Riot Client region chosen before a game
  launch: the account's League platform (`lol`/`lol_region` userinfo claims)
  must belong to the client's region in `region_data`, a region the account
  also plays on is kept, and nothing changes without a known platform. A
  loopback API verifies the `PUT /riotclient/region-locale` request (the one
  Riot's own window sends after sign-in), that the launch waits until Riot
  reports the new region, and that a region that never applies fails with
  `riot_region_failed`.
- `tests/riot_renderer.test.mjs` runs the production renderer helper against a
  dummy native form. It checks rejection after MFA resets the form, terminal
  errors while a CAPTCHA remains visible, pending challenges, and success.
- `src/riot_process.rs` checks Windows argument quoting and real native process
  containment. One test drops the owner; another deliberately calls
  `std::process::exit` in a dummy parent, bypassing Rust destructors. In both cases
  the contained dummy child must terminate. The temporary renderer is assigned
  to a non-inheritable kill-on-close job during process creation, not afterward.

## Interactive checks with an installed Riot Client

Use a dedicated test account. Do not print credentials, local API tokens, CDP
expressions, or renderer command lines while inspecting these cases.

1. With Riot fully stopped, retain a syntactically valid stale Riot lockfile from
   an abnormal exit. Start login. The missing service must be launched despite
   the old lockfile, and the newly written endpoint must be reread.
2. Enable Riot's saved session for the selected test account and disable
   **Launch the game after signing in**. Start from a stopped client. A normal
   Riot window must open before League Accounts reports an already-connected
   result; the service must not remain headless with no UI.
3. Require verification during initial authentication/session restoration. The
   normal Riot window must already be available and the progress view must ask
   for verification. Cancel before the temporary renderer is created; the normal
   client must remain available. After an MFA challenge, trigger a rejected or
   expired login that resets the form: report failure immediately, not after the
   five-minute timeout.
4. Close League Accounts using the title-bar close button during renderer setup
   and during CAPTCHA/MFA. Shutdown must request cancellation and wait for native
   cleanup; the temporary renderer and debugger listener must terminate before
   the app exits. Repeat the close request while cleanup is in progress.
5. Separately force-terminate League Accounts while its temporary debugger is
   active. The Windows job must remove that renderer and its descendants even
   though Rust cleanup cannot run. A normal replacement window is guaranteed
   only by orderly cleanup, not by a forced process termination.
6. Verify that the temporary renderer and its debugger are gone before game
   launch, that no Riot window reopens while the game starts, and that a failed
   or cancelled launch reopens a normal renderer without DevTools. Identity
   verification and the warning before closing another account's game must
   still apply.
7. With forward slashes in `RiotClientInstalls.json`, sign in from both a stopped
   client and an already-open Riot window. Repeat with game launch enabled and
   disabled. With game launch disabled, exactly one normal Riot Client window
   must remain after login; with it enabled, only the game client opens. The old
   client and temporary renderer must both have exited.
8. With game launch enabled, sign in to an account from one League region (for
   example EUNE), then switch to an account from another (EUW) and back. League
   must sign in each time without "An unexpected error occurred while logging
   in", and `GET /riotclient/region-locale` must report the launched account's
   region.

The automated dummy-process tests are not a substitute for these installed-client
checks: Riot's native renderer and authentication UI are undocumented interfaces.
