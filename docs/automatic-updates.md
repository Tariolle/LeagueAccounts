# Automatic updates

How League Accounts replaces itself. The user-facing behavior is in the [README](../README.md#updates); releasing is under [Releasing](../README.md#releasing).

## Why the app replaces itself

Releases are a single portable `LeagueAccounts.exe`. Tauri's updater plugin installs only NSIS or MSI bundles on Windows, so the app downloads, checks and swaps its own executable instead. Distribution and installation stay as they are.

## Trust

The app installs any release published on this repository. Only the repository owner can publish one, so that account is what protects users: keep two-factor authentication on it.

Releases are not signed. With the key held in GitHub secrets, as a CI pipeline would need, a signature would not stop someone who controls the account, and HTTPS plus the URL and digest checks below already cover the download. Signing would add protection only with a key kept off GitHub and used locally for every release.

## Flow

1. **Check** (`src/updates.rs`). `latest_release()` reads GitHub's `releases/latest` (published, not prerelease). A release is installed automatically only if its `LeagueAccounts.exe` is uploaded under `https://github.com/Tariolle/LeagueAccounts/releases/download/` and GitHub reports a SHA-256 `digest` for it. While automatic updates are on, the first check runs at launch, then every 6 hours, or 15 minutes after a failed check. When they are off, GitHub is contacted only from Settings › Check now.
2. **Download.** Over HTTPS, checked against the app's bundled root certificates rather than the Windows store. The file is written to `<exe>.download` in the executable's folder: the same volume, so the swap is a rename. Its size must match the size GitHub announced.
3. **Verify.** The file on disk must match the SHA-256 digest GitHub computed when the asset was uploaded.
4. **Swap** (`src/self_update.rs`). The verified image replaces the running executable, with rollback on a failed rename or receipt commit. `<exe>.receipt.json` atomically records the target version, install time and `healthy: false`. An existing `<name>.old.exe` is protected until its previous receipt is healthy and at least 24 hours old; another install cannot overwrite that rollback copy sooner.
5. **Restart** (`src-tauri/src/updater.rs`).
   - **At launch:** right away from the update screen.
   - **Otherwise:** once the UI has no dialog, menu, login or rank refresh in progress and `update_idle` reports no login, refresh or `League of Legends.exe`, after a 10 s notice.
   - **Admission safety:** login and every full or partial rank refresh share one admission lock. `LoginControl::close_if_idle` closes admission only when all admitted work has finished; normal exit cancels login and waits for native cleanup and admitted storage work.
   - **Hand-over:** on `RunEvent::Exit`, the app starts the new executable with its original arguments (keeping `--autostart`), plus `--updated-from <pid>`. It passes `--minimized` when the window was minimized or not focused.
6. **New-process health.** The single-instance check runs before account loading, credential migration and update cleanup. The process waits for the previous pid when present, then compares its reported version with the persisted installation target on every launch, including a normal launch after choosing Later. A higher target identifies a mislabeled release and disables automatic reinstallation of that release. After accounts load and the interface is revealed, `app_ready` marks the matching receipt healthy. Cleanup retains `<name>.old.exe` until health is confirmed and 24 hours have elapsed; missing, corrupt or unhealthy receipts retain it. Downloads abandoned for over an hour are still cleaned up.

## UI

- **Status.** `update_status` returns the state, and every change is an `update-status` event carrying the full state and an increasing `seq`. The UI subscribes before reading, so it misses nothing.
- **Launch hold.** With automatic updates on, the loading screen waits up to 3 s from the `launchUpdate` call, not from page navigation. A delayed bootstrap does not consume this budget. During a launch download the loader shows the update screen; Continue in the background reveals the app and enters silent mode.
- **Late progress.** A launch download that starts after the app is visible shows its version, percentage and downloaded/total size in a progress banner. Only its close button dismisses it; clicking status text does not. Dismissal remains silent through later progress events, but completion still shows the restart notice.
- **Restart notice.** When its 10 s countdown ends, the app checks again that no match has started; *Restart now* restarts right away.
- **Banner fallback.** `available` (found by Check now with automatic updates off, or a release without a usable executable) and `failed` (read-only folder, network, digest mismatch) show the existing "Update available" banner with a link to the release page.

## Settings

`autoUpdate`, on by default, replaces `checkUpdates`. When it is off, nothing is checked until the user clicks Check now, which then offers a newer release through the banner. Settings saved with `checkUpdates: false` load as `autoUpdate: false`, so users who turned checks off keep them off.

## Logging

The log events carry fixed codes only, never URLs or paths:

- `update_check_failed`
- `update_downloaded`
- `update_verification_failed`, with reason `checksum`
- `update_installed`
- `update_failed`, with reason `version_mismatch` for a mislabeled release
- `update_restarted`

## Releases

- **Workflow:** `.github/workflows/release.yml` runs on a plain `vX.Y.Z` tag. It checks the tag against the version files, runs the Rust tests, builds the exe, creates a draft release with it, and checks that GitHub's digest of the upload matches the build.
- **Versions:** only complete SemVer versions are compared, with an optional `v` prefix. Prerelease precedence is respected; build metadata does not imply a newer release. The release workflow excludes prerelease tags from the published latest-release channel.

## Testing

- **Unit tests.** `cargo test` covers:
  - asset selection, including the digest format;
  - the digest check;
  - swap, rollback and cleanup in temporary folders;
  - restart arguments;
  - waiting for a process to exit.
- **UI tests.** `tests/update.test.mjs` covers normal and delayed startup, late progress, status-text clicks, silent dismissal, completed-install notices and restart admission.
- **End to end on Windows.** Debug builds read `LEAGUEACCOUNTS_UPDATE_FEED`, a `releases/latest`-style JSON URL whose assets live next to it. It is compiled out of release builds, and without it a debug build never replaces itself. To run it:
  1. Build with `npx tauri build --debug --no-bundle`.
  2. Serve a copy and a matching JSON, with its `size` and `sha256:` digest, from `127.0.0.1`.
  3. Run the build with the override, plus `APPDATA` and `WEBVIEW2_USER_DATA_FOLDER` pointed at a scratch folder, so real accounts stay untouched.
