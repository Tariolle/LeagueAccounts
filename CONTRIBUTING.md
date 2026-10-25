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

## Translations

Every dictionary lives in `ui/src/i18n.ts`. English (`en`) is the reference:
`MessageKey` is derived from it, and each other dictionary is typed `Dict`, so
a missing, misspelled or extra key fails `tsc` instead of showing a raw key at
runtime.

To add a language:

1. Copy the `en` object to `const <lang>: Dict = { … }` and translate every
   value. Keep the `{placeholders}` exactly as they are.
2. Add it to `LANGUAGES`, to `DICTS`, and to `HTML_LANG` (a real BCP-47 tag —
   `zh-TW`, not `zhTw`).
3. Run `npx tsc --noEmit`.

Chinese also needs the fallback glyphs: `node scripts/cjk-font.mjs` regenerates
the bundled subsets from the characters the interface can render. Run it after
changing any string.

Chinese needs two entries: `zh` (`zh-CN`) and `zhTw` (`zh-TW`). The browser
language is matched in `fromBrowser`, where `zh-HK`/`zh-MO`/`zh-Hant` map to
Traditional.

### Terminology

Agreed wording, so the same concept reads the same everywhere:

| English | 简体中文 | 繁體中文 |
|---|---|---|
| account (credentials) | 账号 | 帳號 |
| account ID | 账号 ID | 帳號 ID |
| Riot ID | Riot ID | Riot ID |
| Riot Client | Riot客户端 | Riot客戶端 |
| region | 区服 | 伺服器 |
| rank / tier | 段位 | 階級 |
| Iron … Challenger | 黑铁 … 王者 | 鐵牌 … 菁英 |
| TFT | 云顶之弈 | 聯盟戰棋 |
| ARAM | 大乱斗 | 隨機單中 |
| Windows Credential Manager | Windows 凭据管理器 | Windows 認證管理員 |

### Localization platform

Not integrated, deliberately: no Crowdin, Weblate or Transifex configuration
exists. Those platforms sync against one file per language, so adding one later
means splitting the dictionaries back out and writing the tool's config — worth
doing only if translations are actually delegated.

Weblate is the first choice when that happens: open-source projects can be
hosted free on `libre.weblate.org`, and its translations land as ordinary Git
commits, which matches how this repository already works.

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
