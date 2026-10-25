# Automatic updates

How League Accounts replaces itself. The user-facing behavior is in the [README](../README.md#updates); releasing is under [Releasing](../README.md#releasing).

## Why the app replaces itself

Releases are a single portable `LeagueAccounts.exe`. Tauri's updater plugin installs only NSIS or MSI bundles on Windows, so the app downloads, checks and swaps its own executable instead. Distribution and installation stay as they are.

## Trust

The app installs any release published on this repository. Only the repository owner can publish one, so that account is what protects users: keep two-factor authentication on it.

Releases are not signed. With the key held in GitHub secrets, as a CI pipeline would need, a signature would not stop someone who controls the account, and HTTPS plus the URL and digest checks below already cover the download. Signing would add protection only with a key kept off GitHub and used locally for every release.

## Flow

1. **Check** (`src/updates.rs`). `latest_release()` reads GitHub's `releases/latest` (published, not prerelease) for the repository selected when the app is built. A release is installed automatically only if its `LeagueAccounts.exe` is uploaded to that repository and GitHub reports a SHA-256 `digest` for it. The build takes the repository from `LEAGUEACCOUNTS_UPDATE_REPOSITORY` when set, otherwise from the GitHub `origin` remote. Source archives without a Git remote default to `Tariolle/LeagueAccounts`. While automatic updates are on, the first check runs at launch, then every 6 hours, or 15 minutes after a failed check. When they are off, GitHub is contacted only from Settings › Check now.
2. **Download.** Over HTTPS, checked against the app's bundled root certificates rather than the Windows store. The file is written to `<exe>.download` in the executable's folder: the same volume, so the swap is a rename. Its size must match the size GitHub announced.
3. **Verify.** The file on disk must match the SHA-256 digest GitHub computed when the asset was uploaded.
4. **Swap** (`src/self_update.rs`). The running `<name>.exe` is renamed to `<name>.old.exe` (Windows allows renaming a running executable), then the download takes its name. If the second rename fails, the first is undone. The swap happens right after verification, so declining the restart, closing the app or a crash all still start the new version next time.
5. **Restart** (`src-tauri/src/updater.rs`).
   - **At launch:** right away from the update screen.
   - **Otherwise:** once the UI has no dialog, menu, login or rank refresh in progress and `update_idle` reports no login, refresh or `League of Legends.exe`, after a 10 s notice.
   - **Login safety:** `LoginControl::close_if_idle` closes login admission and refuses while a login runs, so a restart never interrupts a sign-in.
   - **Hand-over:** on `RunEvent::Exit`, after login cleanup, the app starts the new executable with its original arguments (keeping `--autostart`), plus `--updated-from <pid> --updated-to <version>`. It also passes `--minimized` when the window was minimized or not focused, so a restart never comes up in front of another app.
6. **Hand-over in the new process.** Before anything else, it waits up to 30 s for the previous process to exit, so the account files and the WebView2 profile are free. It then deletes `<name>.old.exe` and any `.download` older than an hour. If it reports a version older than `--updated-to`, the release was mislabeled: it logs `update_failed reason=version_mismatch` and saves the release as `mislabeledRelease` in the settings. From then on that release is only offered through the banner, so it cannot update in a loop. The release workflow also refuses a tag that doesn't match the version files.

## UI

- **Status.** `update_status` returns the state, and every change is an `update-status` event carrying the full state and an increasing `seq`. The UI subscribes before reading, so it misses nothing.
- **Launch hold.** With automatic updates on, the loading screen waits up to 3 s after page load for the launch check. When an update is downloading, the hextech loader stays up with "UPDATING LEAGUE ACCOUNTS" and a 200×16 progress bar (after the client's `lol-loading-screen`). *Continue in the background* shows the app; the restart notice follows later.
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
- **Tags:** pre-release tags such as `v3.1.0-beta` are excluded. They would otherwise publish as the latest release and stop users on them from ever getting `v3.1.0`, since both compare as equal.

## Testing

- **Unit tests.** `cargo test` covers:
  - asset selection, including the digest format;
  - the digest check;
  - swap, rollback and cleanup in temporary folders;
  - restart arguments;
  - waiting for a process to exit.
- **UI tests.** `tests/update.test.mjs` drives the browser mock through the launch screen, the restart notice and the banner.
- **End to end on Windows.** Debug builds read `LEAGUEACCOUNTS_UPDATE_FEED`, a `releases/latest`-style JSON URL whose assets live next to it. It is compiled out of release builds, and without it a debug build never replaces itself. To run it:
  1. Build with `npx tauri build --debug --no-bundle`.
  2. Serve a copy and a matching JSON, with its `size` and `sha256:` digest, from `127.0.0.1`.
  3. Run the build with the override, plus `APPDATA` and `WEBVIEW2_USER_DATA_FOLDER` pointed at a scratch folder, so real accounts stay untouched.
