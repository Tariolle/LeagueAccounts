# LeagueAccounts

**LeagueAccounts** helps you manage multiple **League of Legends** accounts from one Windows app, with secure password storage, quick login helpers, and automatic rank updates. The application is implemented in Rust and ships as a native Windows desktop executable.

## Features

- **Quick Account Switching**: Keep all accounts in one list, searchable by account ID or summoner name.
- **Secure Password Storage**: Store credentials with Windows Credential Manager; the local account file never contains passwords.
- **Auto Credential Entry**: Use `CTRL+SHIFT+V` to switch back to the previous window and fill Riot Client login fields.
- **Automatic Rank Updates**: Fetch current rank, last-season peak/finished ranks, and level from OP.GG in parallel.
- **Import / Export**: Move account data between installs as JSON (exports include passwords by explicit request).
- **Friend Elo filtering**: Show accounts compatible with a selected tier and division.
- **Inline editing**: Double-click a summoner name or description to edit it.

## Installation

[Download the latest release](https://github.com/FlorentTariolle/LeagueAccounts/releases/latest).

## Building and running from source

```bash
# Run the native app
cargo run --release

# Build the Windows executable
cargo build --release
```

The executable is written to `target/release/LeagueAccounts.exe`. Account data is stored at `%APPDATA%\\LeagueAccounts\\league_accounts.json`; passwords are stored in the native Windows Credential Manager under the `LeagueAccounts` service.

## Keyboard shortcuts

- `Ctrl+C`: copy the selected account ID; press again for its password.
- `Ctrl+Shift+V`: auto-type the selected account ID and password in the previous window.
- `Delete`: delete the selected account.
- Double-click a summoner name or description to edit it.
