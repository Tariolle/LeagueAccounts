# Contributing

## Building from source

Requirements: [Rust](https://rustup.rs) (MSVC toolchain), [Node.js](https://nodejs.org) 20+, and the Visual Studio C++ Build Tools.

```bash
npm install
npx tauri dev                # run with hot reload
npx tauri build --no-bundle  # build target/release/LeagueAccounts.exe
```

| Folder | Contents |
|---|---|
| `src/` | Rust core: storage, Credential Manager, OP.GG parsing, Riot Client integration, auto-type |
| `src-tauri/` | Tauri shell: commands, auto refresh scheduler, settings, automatic updates |
| `ui/` | Web interface (TypeScript + CSS); translations live in `ui/src/i18n.ts` |

## Browser preview

`npm run dev` serves the interface at **http://localhost:1420** with 11 synthetic accounts (one per rank from Iron to Challenger, plus Unranked, in LoL and TFT). A **DEV DATA** badge marks the preview. Login, refresh, edits and settings are simulated in memory: no real accounts, credentials, files or Riot Client connections are used. **Reset demo** restores the sample data.

`?scene=` opens a specific screen: `tft`, `list`, `settings`, `add`, `menu`, `login`, `update`, `updating` (update found at launch), `restart` (update installed in the background) and `loading` (holds the startup screen).

## Tests

| Command | Covers |
|---|---|
| `cargo test --locked --workspace --all-targets` | Rust core and shell (build the UI first: `npm ci && npm run build`) |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Lints |
| `npm test` | Riot renderer authentication regressions |
| `npm run test:ui` | The interface in headless Chrome: startup, filters, crests, keyboard navigation, dropdowns, Settings, adding accounts, login progress, motion and update screens |

See [tests/README.md](tests/README.md) for what the Rust suites cover.

## Screenshots

`npm run screenshots` regenerates the README's PNGs and GIFs from the real interface with synthetic accounts (`npm run screenshots -- demo` for one scene). The **Screenshots** workflow runs it on every release tag (or from the Actions tab) and opens a pull request with the new captures. Locally it needs Chrome and ffmpeg on `PATH`; set `CHROME_PATH` for another Chromium browser (e.g. Edge) or `BASE_URL` to reuse a running preview.

## Client assets

Client assets are bundled locally and pinned to patch 16.20. `npm run assets:verify` checks their hashes (builds run it); `node scripts/league-assets.mjs --fetch` restores missing ones. See [THIRD_PARTY_ASSETS.md](THIRD_PARTY_ASSETS.md) for provenance and rights.

## Releasing

1. Set the new version in `package.json`, `src-tauri/tauri.conf.json`, `Cargo.toml` and `src-tauri/Cargo.toml`, run `npm install` and `cargo check` to update the lock files, and merge.
2. Push a matching tag, for example `git tag v3.1.0 && git push origin v3.1.0`.
3. The **Release** workflow builds `LeagueAccounts.exe` and creates a draft release. Write the notes and publish it; apps update only from published releases.
4. The **Screenshots** workflow opens a pull request with refreshed README captures; merge it if they look right.

Every installed copy updates to whatever is published here, so keep two-factor authentication on the GitHub account. See [the automatic updates design note](docs/automatic-updates.md).
