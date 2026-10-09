# Riot login lifetime regressions

Run on Windows 10 or 11:

```text
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
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
  The service-launch trace distinguishes a headless start from normal restoration
  and verifies that successful cleanup runs only once.
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
3. Cancel while the initial authentication/session-restore step is running,
   before the temporary renderer is created. A normal client must be restored.
4. Close League Accounts using the title-bar close button during renderer setup
   and during CAPTCHA/MFA. Shutdown must request cancellation and wait for native
   cleanup; the temporary renderer and debugger listener must terminate before
   the app exits. Repeat the close request while cleanup is in progress.
5. Separately force-terminate League Accounts while its temporary debugger is
   active. The Windows job must remove that renderer and its descendants even
   though Rust cleanup cannot run. A normal replacement window is guaranteed
   only by orderly cleanup, not by a forced process termination.
6. Verify that successful login and account switching still restore a normal
   renderer without DevTools before game launch, and that identity verification
   and the warning before closing another account's game still apply.

The automated dummy-process tests are not a substitute for these installed-client
checks: Riot's native renderer and authentication UI are undocumented interfaces.
