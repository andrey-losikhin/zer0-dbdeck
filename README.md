# zer0-dbdeck

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![Platform: Linux](https://img.shields.io/badge/platform-Linux-lightgrey?logo=linux)](SUPPORT.md)
[![Rust](https://img.shields.io/badge/built_with-Rust-DEA584?logo=rust)](https://www.rust-lang.org/)

A keyboard-first database client for Linux, built with Rust, GTK4 and
libadwaita. Browse PostgreSQL, MySQL and MariaDB, run SQL, edit rows and move
data without turning a daily development tool into a database administration suite.

**[Русская документация](README.ru.md)** · [Contributing](CONTRIBUTING.md) ·
[Security](SECURITY.md) · [Architecture](docs/ARCHITECTURE.md)

## Status

DBDeck is a **pre-1.0 personal project**. Core workflows are implemented and
covered by automated checks; this is not a promise of compatibility with every
server version, extension or production deployment. The application UI is
currently in Russian. Linux/Wayland is the primary target; Windows and macOS
builds are not supported.

## Features

- PostgreSQL, MySQL and MariaDB profiles, multiple active connections and tabs.
- Database/schema/table browser, including multiple databases per connection.
- Server-side filters and sorting, raw SQL conditions and incremental table browsing.
- Spreadsheet-style ranges, TSV clipboard, resizable columns and staged cell edits.
- Optimistic conflict detection, primary-key checks and explicit save/discard.
- Editable SQL results when a single source table and its primary key can be identified.
- SQL highlighting, completion, diagnostics, encrypted history and restored drafts.
- Dedicated SQL sessions, manual transactions, commit/rollback, cancellation and timeouts.
- Read-only mode and per-operation confirmation for production writes.
- CSV import with column mapping; streaming full-table CSV export.
- Schema-only dumps, full database backups and native SQL/PostgreSQL custom restore.
- Encrypted portable connection storage, OS-keyring/gopass auto-unlock and gopass references.
- Command palette, searchable keyboard shortcuts and dark themes including Tokyo Night.

## Requirements

- A current Rust stable toolchain with Cargo, rustfmt and Clippy.
- GTK4, libadwaita, GtkSourceView 5, WebKitGTK 6.0 and `pkg-config` development files.
- A Wayland session for normal interactive use.
- Optional: matching PostgreSQL/MySQL/MariaDB native tools for dump/restore,
  `secret-tool` for the desktop keyring, or `gopass` for password-store integration.
- Development checks require Node.js 20 or newer; the application itself does not use Node.js.
  Docker Compose is optional for database integration testing.

### Arch Linux / CachyOS

```sh
sudo pacman -S --needed base-devel rust gtk4 libadwaita gtksourceview5 webkitgtk-6.0 pkgconf
```

If Rust is managed by rustup, use that toolchain instead of installing a second
system toolchain. `rust-toolchain.toml` selects stable and the development components.

### Ubuntu 24.04

Install Rust with [rustup](https://rustup.rs/), then install native dependencies:

```sh
sudo apt-get update
sudo apt-get install build-essential pkg-config libgtk-4-dev libadwaita-1-dev \
  libgtksourceview-5-dev libwebkitgtk-6.0-dev
```

## Build and run

```sh
git clone https://github.com/andrey-losikhin/zer0-dbdeck.git
cd zer0-dbdeck
cargo run --locked
```

For an optimized build:

```sh
cargo build --release --locked
./target/release/zer0-dbdeck
```

Prebuilt packages are not published yet. Native libraries are runtime dependencies;
the executable is not a standalone static binary.

## Daily workflow

1. Create a connection, choose its database engine and transport, and test it.
2. Open a database and a table, or create an SQL tab beside the existing tabs.
3. Click to select cells; double-click or press Enter to edit. Changes remain
   local until saved. Use the command palette to discover available actions.
4. Use a read-only database account for browsing. Enable writes separately when needed.

| Shortcut | Action |
| --- | --- |
| `Ctrl+,` | Settings |
| `Ctrl+Shift+P` | Command palette |
| `Ctrl+Shift+?` | Searchable shortcut reference |
| `Ctrl+Enter` | Run selected SQL, otherwise the whole editor |
| `Ctrl+Space` | SQL completion |
| `Ctrl+Shift+.` | Cancel the SQL query |
| `Ctrl+Alt+T` | Toggle manual transactions |
| `Ctrl+Alt+Enter` / `Ctrl+Alt+R` | Commit / rollback |
| `Ctrl+Alt+H` | SQL history |
| `Ctrl+Shift+D` | Table structure and DDL |
| `Ctrl+Shift+I` / `Ctrl+Alt+I` | Native restore / CSV import |
| `Ctrl+Shift+X` | Cancel a data transfer |
| `Escape` | Dismiss a popup or cancel the current editor/selection |

## Data handling and safety

The environment label (`local`, `stage`, `prod`) is independent of transport.
All three allow no TLS, TLS without certificate verification, or TLS with
certificate and hostname verification. Selecting PROD does not change the chosen
transport; it defaults the profile to viewing and requires confirmation for writes.

Read-only SQL and table browsing run in server READ ONLY transactions. Known
state-changing calls are rejected in read-only mode. These controls supplement
server permissions: they do not sandbox arbitrary stored functions, prevent
expensive queries, or replace a restricted database role. Use verified TLS or an
appropriately secured external tunnel for untrusted networks. Built-in SSH tunneling
is not implemented.

Production confirmations are bound to the operation and SQL-session revision.
A lost commit acknowledgement is not retried automatically. Check the server
state before repeating a write. MySQL/MariaDB DDL and nontransactional objects
can leave a partially applied native import.

### Preview and export limits

| Operation | Scope |
| --- | --- |
| Table browser | 200-row pages; up to 10,000 cached rows and 32 MiB of model data |
| SQL preview | Up to 1,000 rows, 128 columns and 32 MiB of model data |
| Cell preview | Up to 1,024 characters; truncated values cannot be overwritten |
| SQL result CSV | The displayed bounded snapshot |
| Full-table CSV | Streaming export of the complete table |
| CSV import | UTF-8, up to 1 MiB per record and 1,024 fields; transactional batches |

A preview limit is not a server-side query limit. Use `LIMIT`, filters and suitable
indexes for large or expensive SELECTs. The full query stream may still be read.

Native backup/restore requires tools compatible with the actual server. Configure
their directory in the connection settings when they are not on PATH. MariaDB
client aliases are not accepted as Oracle MySQL tools. PostgreSQL full backups use
custom `.dump` archives; schema-only and MySQL/MariaDB backups use SQL. Database
backups do not include global PostgreSQL roles. Owners/roles must exist before
restoring privileges. DBDeck MySQL backups retain the source database identity.

SQL, CSV and database dumps are **not encrypted**. Exports are written privately
and published atomically after success. Native tools and imported SQL are trusted
local code; SQL files can contain native-client commands and cross-database statements.

### Portable encrypted storage

Profiles, saved passwords, history and drafts live in the encrypted
`$XDG_DATA_HOME/zer0/zer0-dbdeck/connections.dbdeck` file (normally under
`~/.local/share`). Encryption uses AES-256-GCM and PBKDF2-HMAC-SHA256 with
600,000 iterations. Auto-unlock can use the desktop keyring or gopass.

Copy the encrypted file or use the encrypted profile export to move machines.
Keep the master password or transfer the configured unlock key securely; copying
an OS-keyring entry is not automatic. Certificate/tool paths are machine-local.
Database backups and connection-profile backups are different things.

## Development and testing

```sh
cargo fmt --all -- --check
cargo check --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D clippy::correctness
node --test tests/grid_logic.test.cjs
```

Run these checks locally before opening a pull request. Hosted CI is not configured.
Existing style/complexity Clippy warnings are visible but are not a warning-free
quality claim. Database and GUI tests are opt-in; the ordinary test command does not count them
as executed. See [docs/TESTING.md](docs/TESTING.md) for local Docker fixtures,
isolated restore/TLS checks and GUI tests that do not open on your desktop.

## Community

- [Report a bug or request a feature](https://github.com/andrey-losikhin/zer0-dbdeck/issues)
- [Contribution guide](CONTRIBUTING.md)
- [Code of conduct](CODE_OF_CONDUCT.md)
- [Support and compatibility](SUPPORT.md)
- [Changelog](CHANGELOG.md)

Please use synthetic data in issues. Report suspected vulnerabilities privately
as described in [SECURITY.md](SECURITY.md).

## License

Original project code is licensed under [Apache-2.0](LICENSE).
Bundled libraries, icons and adapted theme assets retain their own licenses;
see [NOTICE](NOTICE) and [third-party notices](docs/THIRD_PARTY_NOTICES.md).
