#!/usr/bin/env bash
# Disposable DB servers: no host DB connections, no shared volumes, no DROP DATABASE.
set -euo pipefail
: "${DBDECK_PG_PASSWORD:?source .env first}"
: "${DBDECK_MARIADB_ROOT_PASSWORD:?source .env first}"
: "${DBDECK_MYSQL_ROOT_PASSWORD:?source .env first}"
validation_token="dbdeck-validation-$$-$(date +%s)"
validation_ids=()
validation_tools=""
cleanup() {
    for id in "${validation_ids[@]}"; do
        if [[ "$(docker inspect -f '{{index .Config.Labels "dbdeck.validation"}}' "$id" 2>/dev/null || true)" == "$validation_token" ]]; then
            docker stop --time 5 "$id" >/dev/null || true
        fi
    done
    if [[ -n "$validation_tools" ]]; then
        rm -f "$validation_tools/mysql" "$validation_tools/mysqldump" "$validation_tools/libncurses.so.6" "$validation_tools/libtinfo.so.6"
        rmdir "$validation_tools"
    fi
}
trap cleanup EXIT
start_server() {
    local engine="$1" id
    case "$engine" in
        postgres)
            export POSTGRES_PASSWORD="$DBDECK_PG_PASSWORD"
            id=$(docker run -d --rm --label "dbdeck.validation=$validation_token" --tmpfs /var/lib/postgresql/data:rw --publish 127.0.0.1::5432 --env POSTGRES_PASSWORD --env POSTGRES_USER=dbdeck_test --env POSTGRES_DB=dbdeck_validation postgres:17.11-alpine)
            ;;
        mariadb)
            export MARIADB_ROOT_PASSWORD="$DBDECK_MARIADB_ROOT_PASSWORD"
            id=$(docker run -d --rm --label "dbdeck.validation=$validation_token" --tmpfs /var/lib/mysql:rw --publish 127.0.0.1::3306 --env MARIADB_ROOT_PASSWORD --env MARIADB_ROOT_HOST=% --env MARIADB_DATABASE=dbdeck_validation mariadb:11.4.13)
            ;;
        mysql)
            export MYSQL_ROOT_PASSWORD="$DBDECK_MYSQL_ROOT_PASSWORD"
            id=$(docker run -d --rm --label "dbdeck.validation=$validation_token" --tmpfs /var/lib/mysql:rw --publish 127.0.0.1::3306 --env MYSQL_ROOT_PASSWORD --env MYSQL_ROOT_HOST=% --env MYSQL_DATABASE=dbdeck_validation mysql:8.4)
            ;;
    esac
    validation_ids+=("$id")
    for ((attempt=0;attempt<180;attempt++)); do
        if [[ "$engine" == postgres ]]; then
            if docker exec "$id" pg_isready -U dbdeck_test -d dbdeck_validation >/dev/null 2>&1; then break; fi
        elif [[ "$engine" == mariadb ]]; then
            if docker exec "$id" healthcheck.sh --connect --innodb_initialized >/dev/null 2>&1; then break; fi
        else
            if docker exec "$id" sh -c 'MYSQL_PWD="$MYSQL_ROOT_PASSWORD" mysqladmin ping -h 127.0.0.1 --protocol=tcp --silent' >/dev/null 2>&1; then break; fi
        fi
        if ((attempt==179)); then echo "Isolated $engine startup timed out" >&2; return 1; fi
        sleep 0.5
    done
    validation_last_id="$id"
    validation_last_port=$(docker port "$id" | awk -F: 'NR==1 {print $NF}')
}
export DBDECK_ISOLATED_VALIDATION=1
for engine in ${DBDECK_VALIDATION_ENGINES:-postgres mariadb mysql}; do
    case "$engine" in
        postgres) export DBDECK_ISOLATED_KIND=postgre_sql DBDECK_ISOLATED_USER=dbdeck_test DBDECK_ISOLATED_PASSWORD="$DBDECK_PG_PASSWORD";;
        mariadb) export DBDECK_ISOLATED_KIND=maria_db DBDECK_ISOLATED_USER=root DBDECK_ISOLATED_PASSWORD="$DBDECK_MARIADB_ROOT_PASSWORD";;
        mysql) export DBDECK_ISOLATED_KIND=my_sql DBDECK_ISOLATED_USER=root DBDECK_ISOLATED_PASSWORD="$DBDECK_MYSQL_ROOT_PASSWORD";;
        *) echo "Unknown validation engine" >&2; exit 1;;
    esac
    start_server "$engine"
    source_id="$validation_last_id"
    unset DBDECK_ISOLATED_TOOLS
    if [[ "$engine" == mysql ]]; then
        # Use the exact vendor/version from our disposable server, not MariaDB aliases
        # installed on the host. The binaries contain no credentials or DB data.
        validation_tools=$(mktemp -d /tmp/dbdeck-mysql-tools.XXXXXXXX)
        docker cp "$source_id:/usr/bin/mysql" "$validation_tools/mysql" >/dev/null
        docker cp "$source_id:/usr/bin/mysqldump" "$validation_tools/mysqldump" >/dev/null
        docker cp -L "$source_id:/lib64/libncurses.so.6" "$validation_tools/libncurses.so.6" >/dev/null
        docker cp -L "$source_id:/lib64/libtinfo.so.6" "$validation_tools/libtinfo.so.6" >/dev/null
        export LD_LIBRARY_PATH="$validation_tools${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
        "$validation_tools/mysql" --version
        "$validation_tools/mysqldump" --version
        export DBDECK_ISOLATED_TOOLS="$validation_tools"
    fi
    export DBDECK_ISOLATED_SOURCE_PORT="$validation_last_port"
    for mode in schema full; do
        start_server "$engine"
        target_id="$validation_last_id"
        export DBDECK_ISOLATED_TARGET_PORT="$validation_last_port" DBDECK_ISOLATED_MODE="$mode"
        "${DBDECK_CARGO:-cargo}" test --locked --offline --bin zer0-dbdeck isolated_backup_restore_fidelity -- --ignored --nocapture --test-threads=1
        docker stop --time 5 "$target_id" >/dev/null
    done
    docker stop --time 5 "$source_id" >/dev/null
    echo "Isolated $engine validation complete"
done
