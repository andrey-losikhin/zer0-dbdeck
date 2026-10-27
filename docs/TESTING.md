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

Run these checks locally; hosted CI is not configured. Clippy correctness lints
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

## Production-policy and streaming checks

With fixture credentials exported:

```sh
cargo test --locked --bin zer0-dbdeck readonly_and_production_policy_all_engines -- --ignored --test-threads=1
cargo test --locked --bin zer0-dbdeck sql_sessions_transactions_and_isolation -- --ignored --test-threads=1
cargo test --locked --bin zer0-dbdeck million_row_csv_streams_and_cancel_preserves_file -- --ignored --test-threads=1
```

The policy/session tests use connection-local TEMP objects. The streaming check
uses synthetic data; local timing results are not a performance SLA.

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

Other focused scenarios include `application_staged_save_discard_and_paging`,
`sql_result_staging_and_delete` and `popup_dismissal_and_file_locations`.
Do not run all ignored GUI tests in one process: GTK state is process-global.
