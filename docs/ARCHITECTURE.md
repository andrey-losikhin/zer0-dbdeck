# Architecture

DBDeck is a native Linux database client. This document describes the current
source structure rather than a roadmap or a promise of production certification.

## Runtime

The GTK/libadwaita main thread owns widgets and UI state (`src/main.rs`). Tokio
runs database operations outside that thread. Results return through application
events; session/table identities guard against stale asynchronous results.

Table grids use a local WebKitGTK view and bundled Tabulator assets. No remote
script/CDN is needed for grid rendering. JavaScript sends typed messages to the
Rust bridge; Rust controls database access, clipboard data and staged writes.

## Main modules

| Area | Files |
| --- | --- |
| Window, application events, status bar | `src/main.rs` |
| Footer task list and in-memory outcomes | `src/background_tasks.rs` |
| Single tab strip in the header bar and connection block of the status bar | `src/tab_strip.rs` |
| Database adapters, browsing, edits, export | `src/database.rs` |
| SSH, SOCKS5 and HTTP CONNECT | `src/connection_transport.rs`, `src/connection_transport_ui.rs` |
| Dedicated SQL connection and transactions | `src/sql_session.rs` |
| SQL risk classification and source-table detection | `src/sql_risk.rs`, `src/query_edit.rs` |
| SQL editor and completion | `src/sql_editor.rs` |
| Grid bridge and local grid UI | `src/tabulator.rs`, `assets/grid/` |
| Catalog and explicit database selection | `src/catalog_tree.rs`, `src/database_target_ui.rs` |
| Table metadata/DDL | `src/database_structure.rs`, `src/structure_ui.rs` |
| CSV parser/import and wizard | `src/csv_input.rs`, `src/csv_import.rs`, `src/csv_import_ui.rs` |
| Native backup/restore | `src/native_backup.rs`, `src/backup_ui.rs` |
| Atomic files and cancellation/publication | `src/output_file.rs`, `src/transfer.rs` |
| Encrypted profiles/workspace | `src/settings.rs`, `src/sql_workspace.rs` |
| Keyring/gopass and storage UI | `src/secret_store.rs`, `src/storage_ui.rs` |
| Production confirmation and TLS controls | `src/production_ui.rs`, `src/connection_tls_ui.rs` |
| Themes, preferences and keyboard commands | `src/theme.rs`, `src/preferences_ui.rs`, `src/shortcuts.rs` |
| Noctalia theme (colours read from `~/.config/noctalia/colors.json`) | `src/noctalia.rs` |

## Window layout

One tab strip in the header bar mirrors both workspace notebooks (table tabs and
SQL tabs). The notebooks keep their tab bars hidden and still own pages and
selection; the strip only shows them and forwards clicks. Each tab has a type
icon, a connection colour marker, a title (`schema.table` or `SQL N`) and a small
second line `connection / database`. A middle click on a table in the tree opens
it in a new tab. The status bar shows the active tab summary
(`name - rows - ms - connection`) and a connection block (name, `LOCAL`, `RO`,
TLS state).

The table pane has a bottom bar with the row count, a "more rows" marker, query
time, `+ row`, and `Apply (N)` / `Rollback` for staged edits. The SQL pane keeps
`Execute as Markdown` in the overflow menu and shows Auto Commit as a switch kept
in sync with the transaction mode.

The catalog tree lists databases, then schemas, then tables; there is no separate
schema dropdown. System schemas (PostgreSQL `information_schema`, `pg_catalog`,
`pg_toast`, `pg_temp_*`; MySQL/MariaDB `information_schema`, `mysql`, `sys`,
`performance_schema`) are hidden by default. The `show_system_schemas` setting
and the tree checkbox control them.

## Themes

`ThemeMode` (`src/theme.rs`) has Dark, Tokyo Night, Catppuccin Mocha (default),
Gruvbox Dark, Nord, Rose Pine, Noctalia and System. Each theme is a `Palette`
plus a CSS file in `assets/themes/` and a GtkSourceView scheme in
`assets/styles/`; `assets/themes/layout.css` holds shared layout rules that use
palette roles. Old saved ids still map through `from_id`; unknown ids fall back
to Catppuccin Mocha.

Noctalia reads `~/.config/noctalia/colors.json`, accepts only `#RRGGBB(AA)`
values through a strict `Rgb` type, takes missing keys from Tokyo Night, and uses
Tokyo Night if the file is absent. A file monitor reloads the palette after
about 250 ms; if a reload fails, the last valid palette stays. Text colours below
WCAG AA contrast are lightened. The SQL scheme is generated into a cache.

## Data boundaries

SQLx provides separate PostgreSQL/MySQL adapters. Connection profiles distinguish
MySQL and MariaDB for native tooling and user-facing behavior. Each SQL tab owns
its connection; session settings and temporary tables are not shared with other
tabs. Connection pools serve catalog/table operations. Routed profiles use a
private Unix relay socket. SQLx keeps the database hostname for TLS verification.
Cloned pool options retain the relay for per-database pools; dropping the last
owner closes the listener and its forwarding tasks.

Read-only SQL and table SELECTs run in READ ONLY transactions with explicit
rollback. Writes from the grid validate metadata/keys and check the original
value before updating. SQL results are editable only when origin detection can
identify one table and a complete primary key. Transactional MySQL edits require
InnoDB and strict mode. No automatic retry is performed for uncertain writes.

Production confirmations capture the operation and check the SQL-session revision
before committing. The environment tag does not choose or enforce TLS. Server
roles and transport policy remain deployment responsibilities; see SECURITY.md.

## Storage and files

`connections.dbdeck` is the encrypted canonical store in XDG data. It contains
profiles, locally stored credentials, SQL history and drafts. Keyring/gopass can
provide an unlock key or a live connection password. TLS/native-tool paths are
local configuration, not portable credential material.

Preview models are bounded. Full-table CSV exports stream rows; CSV imports use
bounded batches inside a transaction. Native tools receive temporary private
credential files instead of password arguments. Export destinations are replaced
only after successful atomic publication. SQL/CSV/database backups are plaintext.

## Verification

See [TESTING.md](TESTING.md) for commands, fixtures and isolation boundaries.
