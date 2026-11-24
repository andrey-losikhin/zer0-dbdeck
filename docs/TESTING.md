# Testing

## Checks without database servers or a display

Install the native libraries from the README, Node.js 20 or newer, and use Rust stable:

```sh
cargo fmt --all -- --check
cargo check --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D clippy::correctness
cargo build --locked
node --test tests/grid_logic.test.cjs
```

Theme checks: `cargo test --locked --bin zer0-dbdeck contrast` runs the WCAG AA
contrast tests for every built-in dark palette and for the Noctalia fallback and
example palettes. Noctalia parsing, fallback and file-reload tests live in
`src/noctalia.rs` and use temporary directories.

These checks also run in GitHub Actions (`.github/workflows/ci.yml`); database,
GUI and isolated scenarios do not run there. Clippy correctness lints
are errors; existing style and
complexity warnings remain visible. The application and integration test binary
share some modules, so some module tests execute in both binaries.

Database/GUI tests are marked ignored and must be selected explicitly. A green
ordinary `cargo test` does not mean those external-environment tests ran.
Use `--offline` only after Cargo has downloaded the locked dependencies.

## Local database fixtures

`.env.example` contains intentionally public, local-only fixture credentials.
Never reuse them for a real database. Compose exposes ports only on 127.0.0.1.

```sh
cp .env.example .env
set -a
. ./.env
set +a
docker compose up -d
./tests/apply-fixtures.sh
cargo test --locked --test database_integration -- --ignored --test-threads=1
```

| Engine | Local port | Database/user | Password variable |
| --- | --- | --- | --- |
| PostgreSQL | 55432 | dbdeck_test | DBDECK_PG_PASSWORD |
| MariaDB | 53306 | dbdeck_test | DBDECK_MARIADB_PASSWORD |
| MySQL | 53307 | dbdeck_test | DBDECK_MYSQL_PASSWORD |

The fixtures include Unicode, NULL, long text, JSON, binary values, multiple
MariaDB/MySQL databases and multiple PostgreSQL schemas. Do not redirect the test
profiles to live servers. `docker compose stop` preserves the fixture volumes.

### Foreign-key navigation

The integration test covers both directions of a composite foreign key, a quoted identifier, a NULL component, and revalidation after the constraint is dropped. PostgreSQL coverage includes separate schemas, links from leaf partitions, a `regclass` primary-key component, and replacement of an identically named constraint. MySQL and MariaDB coverage checks that a unique index alone is insufficient. The GTK scenario opens the cell menu in WebKit and checks that a pending edit survives viewing linked rows.

```sh
cargo test --locked --test database_integration related_rows_follow_composite_foreign_keys -- --ignored --test-threads=1
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck application_related_rows_preserve_staged_edits -- --ignored --test-threads=1
```

Set `DBDECK_RELATIONS_ENGINE` to `postgres`, `mariadb`, or `mysql` to run the integration case against one fixture.

### Table structure comparison

The comparison test uses two connections and the 003 fixtures. It checks matching metadata, differences, and errors on both sides. PostgreSQL covers an empty table and changes to index operator class options. MySQL and MariaDB cover case-distinct table names and changes to a CHECK expression. The PostgreSQL GTK scenario also selects the target connection and table from the structure window.

```sh
cargo test --locked --test database_integration compare_table_structure_across_connections -- --ignored --test-threads=1
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck postgres_schema_groups_sql_and_drafts -- --ignored --test-threads=1
```

Set `DBDECK_STRUCTURE_ENGINE` to `postgres`, `mariadb`, or `mysql` to select one fixture.

## Production-policy and streaming checks

With fixture credentials exported:

```sh
cargo test --locked --bin zer0-dbdeck readonly_and_production_policy_all_engines -- --ignored --test-threads=1
cargo test --locked --bin zer0-dbdeck estimated_plans_all_engines -- --ignored --test-threads=1
cargo test --locked --bin zer0-dbdeck sql_sessions_transactions_and_isolation -- --ignored --test-threads=1
cargo test --locked --bin zer0-dbdeck million_row_csv_streams_and_cancel_preserves_file -- --ignored --test-threads=1
```

The policy/session tests use connection-local TEMP objects. The streaming check
uses synthetic data; local timing results are not a performance SLA.
The estimated plan test uses the sample_records fixture. It checks the SELECT
plan, row preservation when estimating an UPDATE and that the editor's
transaction and table locks survive EXPLAIN.

## Isolated native restore round trips

```sh
cargo fetch --locked
tests/run-production-validation.sh
```

This runner requires the exported `.env` variables, Docker, PostgreSQL native
clients and MariaDB clients on PATH. It creates labeled disposable tmpfs servers,
checks schema-only/full restore fidelity, large CSV imports/cancellation and
connection loss, and cleans only its own containers. No DROP DATABASE operation
is used against existing servers.

For Oracle MySQL it temporarily extracts matching client binaries/libraries from
its disposable MySQL image; this requires a compatible Linux architecture/runtime.
Use `DBDECK_CARGO` for a custom Cargo executable and
`DBDECK_VALIDATION_ENGINES=postgres` (or `mariadb`/`mysql`) for a focused run.

## TLS/mTLS

`bash tests/run-tls-validation.sh` uses ready-made public certificates; it does
not generate keys. Prepare a directory outside the checkout containing:

```text
certs/ca.crt
certs/server.crt
certs/client.crt
keys/server.key
keys/client.key
```

A verified fixture source is [rustls test-ca/rsa-2048](https://github.com/rustls/rustls/tree/51370d96f0e746d01a53c3acb4c7d5cc2db7cba1/test-ca/rsa-2048):
map `ca.cert` to `ca.crt`, `end.fullchain` to `server.crt`, `end.key` to
`server.key`, `client.fullchain` to `client.crt`, and `client.key` to `client.key`.
Supply a genuinely unrelated public CA as `DBDECK_TLS_WRONG_CA` (for example the
[SQLx 0.8.6 test CA](https://github.com/launchbadge/sqlx/blob/v0.8.6/tests/certs/ca.crt)).
Read and preserve the upstream fixture license when copying test material.

```sh
export DBDECK_TLS_FIXTURES=/absolute/path/to/public-test-fixtures
export DBDECK_TLS_WRONG_CA=/absolute/path/to/unrelated-public-ca.crt
cargo fetch --locked
bash tests/run-tls-validation.sh
```

The runner requires Docker, `unshare`, `mount`, `realpath`, and enabled unprivileged
user/mount namespaces. It changes hostname resolution only in its own namespace,
never in the host's hosts file. Tests verify a successful mTLS SELECT and refusal
of wrong CA, wrong hostname and missing client certificates on all three engines.
Never use these publicly known test keys in a real deployment.

## GUI checks without opening a desktop window

Install `kwin_wayland`, D-Bus and a software OpenGL stack. Run each GTK scenario
in a separate process; the runner creates an independent virtual Wayland display
and isolated XDG directories:

```sh
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck production_confirmation_snapshot_and_readonly_ui -- --ignored --test-threads=1
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck sql_save_csv_and_backup_ui_handlers -- --ignored --test-threads=1
```

The background-task scenarios cover bounded history, concurrent SQL and
Markdown cancellation, and the effects of late cancellation and reconnecting.
The cancellation scenario requires local PostgreSQL and its fixture credentials.

```sh
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck background_tasks_retains_recent_outcomes -- --ignored --test-threads=1
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck background_tasks_cancel_scoped_queries_and_report -- --ignored --test-threads=1
```

The tab-management scenario needs only headless GTK and synthetic tabs:

```sh
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck tab_management_preserves_edits_and_group_boundaries -- --ignored --test-threads=1
```

It checks menu actions, tab order and selection, group boundaries, stale menus,
preservation of pending cell/bulk edits and tabs being edited or saved, and
cleanup of an open menu when the tab strip refreshes.
Batch close skips protected tabs and reports their count in the status bar.

Cell preview is covered by the synthetic GTK and WebKit scenarios. They check
the keyboard menu, preserved multiline text, pending edits, truncation notices,
and the preview after a follow-up read succeeds or fails. The fixture browse
tests above also check follow-up reads against each database engine.
The WebKit scenario also drives the table's GTK column chooser and checks that
hidden columns retain staged values, are skipped by clipboard and range actions,
and follow their names when the grid reloads with a different column order.

```sh
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck cell_preview_preserves_multiline_text_and_indicates_truncation -- --ignored --test-threads=1
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck webkit_grid_interactions -- --ignored --test-threads=1
```

Other focused scenarios include `application_staged_save_discard_and_paging`,
`sql_result_staging_and_delete` and `popup_dismissal_and_file_locations`.
Do not run all ignored GUI tests in one process: GTK state is process-global.

The saved table view scenario uses a PostgreSQL temporary table. It saves a view
from the GTK menu, reopens the table and checks that the view restores the SQL
filter, server-side sort and WebKit column visibility. It also checks rejection
with staged edits or a missing sort column and confirms view deletion.

```sh
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck saved_table_view_round_trip -- --ignored --test-threads=1
```

Table edit previews show the row key and previous and staged values. Bulk
replacement shows the affected column and its whole-table scope. The preview
shows up to 50 cell edits and marks values shortened to 240 characters.
The synthetic test checks closing without a write and rejection of stale
previews. The PostgreSQL scenarios also check applying edits from a live
cell editor, conflicts on apply, and database-name confirmation in PROD.

```sh
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck table_edit_preview_checks_snapshot_and_bulk_scope -- --ignored --test-threads=1
```
Export the `.env` variables first (`set -a; . ./.env; set +a`), otherwise tests
that connect to the fixture servers fail on missing passwords. Layout scenarios
(`layout_snapshot`, `workspace_controls_layout_and_risk`,
`postgres_schema_opens_on_explicit_choice`, `keyboard_table_and_sql_commands`,
`settings_theme_controls`) save window screenshots to `target/*.png`.

Known environment limits: `production_*` and `isolated_*` scenarios run only via
`tests/run-production-validation.sh` and `tests/run-tls-validation.sh`. The MySQL
backup/restore scenarios (`native_schema_and_full_backups_postgres_mariadb_mysql`,
`mysql_mariadb_import_temp_sql_and_error`) fail if the `mysql`/`mysqldump` on PATH
come from MariaDB; point the connection at the MySQL client directory in its
settings.

## SQL editor

The synthetic GTK scenario covers contextual suggestions after an alias dot,
replacement at a caret inside an identifier, undo, and comment/string suppression.
It also checks SQL formatting from the tab menu, its shortcut and the command
palette, selection preservation, and undo in a single step.

```sh
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck sql_editor_assistance_and_connected_icon -- --ignored --test-threads=1
G_DEBUG=fatal-criticals tests/run-headless.sh cargo test --locked --bin zer0-dbdeck sql_estimated_plan_uses_selection_and_preserves_editor -- --ignored --test-threads=1
```

The `multiple_namespaces_and_unformatted_sql` integration test also checks the
completion catalog against the PostgreSQL, MariaDB and MySQL fixtures.
The EXPLAIN GTK check requires the PostgreSQL sample_records fixture. It
exercises menu and keyboard invocation, the selected SQL, plan indentation
and focus, switching back to ordinary results, and cancelling a plan without
interrupting the editor's transaction.
