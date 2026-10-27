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
| Window, tabs, application events | `src/main.rs` |
| Database adapters, browsing, edits, export | `src/database.rs` |
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

## Data boundaries

SQLx provides separate PostgreSQL/MySQL adapters. Connection profiles distinguish
MySQL and MariaDB for native tooling and user-facing behavior. Each SQL tab owns
its connection; session settings and temporary tables are not shared with other
tabs. Connection pools serve catalog/table operations.

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
