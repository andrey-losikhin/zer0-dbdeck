#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

docker compose exec -T postgres \
  psql -v ON_ERROR_STOP=1 -U dbdeck_test -d dbdeck_test \
  < tests/fixtures/postgres/001_sample_records.sql
docker compose exec -T postgres \
  psql -v ON_ERROR_STOP=1 -U dbdeck_test -d dbdeck_test \
  < tests/fixtures/postgres/002_browser_fixtures.sql
docker compose exec -T postgres \
  psql -v ON_ERROR_STOP=1 -U dbdeck_test -d dbdeck_test \
  < tests/fixtures/postgres/003_multiple_schemas.sql

docker compose exec -T mariadb sh -ec \
  'export MYSQL_PWD="$MARIADB_PASSWORD"; exec mariadb --default-character-set=utf8mb4 -u "$MARIADB_USER" "$MARIADB_DATABASE"' \
  < tests/fixtures/mariadb/001_sample_records.sql
docker compose exec -T mariadb sh -ec \
  'export MYSQL_PWD="$MARIADB_PASSWORD"; exec mariadb --default-character-set=utf8mb4 -u "$MARIADB_USER" "$MARIADB_DATABASE"' \
  < tests/fixtures/mariadb/002_browser_fixtures.sql
docker compose exec -T mariadb sh -ec \
  'export MYSQL_PWD="$MARIADB_ROOT_PASSWORD"; exec mariadb --default-character-set=utf8mb4 -u root' \
  < tests/fixtures/mariadb/003_multiple_databases.sql

docker compose exec -T mysql sh -ec \
  'export MYSQL_PWD="$MYSQL_PASSWORD"; exec mysql --default-character-set=utf8mb4 -u "$MYSQL_USER" "$MYSQL_DATABASE"' \
  < tests/fixtures/mysql/001_sample_records.sql
docker compose exec -T mysql sh -ec \
  'export MYSQL_PWD="$MYSQL_PASSWORD"; exec mysql --default-character-set=utf8mb4 -u "$MYSQL_USER" "$MYSQL_DATABASE"' \
  < tests/fixtures/mysql/002_browser_fixtures.sql
docker compose exec -T mysql sh -ec \
  'export MYSQL_PWD="$MYSQL_ROOT_PASSWORD"; exec mysql --default-character-set=utf8mb4 -u root' \
  < tests/fixtures/mysql/003_multiple_databases.sql

printf 'Готово: PostgreSQL — public/crm/audit; MariaDB и MySQL — dbdeck_test/dbdeck_analytics.\n'
