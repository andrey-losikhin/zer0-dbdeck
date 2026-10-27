#!/usr/bin/env bash
# Public rustls fixture certificates only; no key generation and no host hosts-file edits.
set -euo pipefail
: "${DBDECK_PG_PASSWORD:?source .env first}"
: "${DBDECK_MARIADB_ROOT_PASSWORD:?source .env first}"
: "${DBDECK_MYSQL_ROOT_PASSWORD:?source .env first}"
fixture=${DBDECK_TLS_FIXTURES:?Set DBDECK_TLS_FIXTURES to prepared public test certificates; see docs/TESTING.md}
fixture=$(realpath "$fixture")
for file in certs/ca.crt certs/server.crt certs/client.crt keys/server.key keys/client.key; do
    test -f "$fixture/$file" || { echo "Missing public test fixture: $file" >&2; exit 1; }
done
: "${DBDECK_TLS_WRONG_CA:?Set DBDECK_TLS_WRONG_CA to an unrelated public CA certificate; see docs/TESTING.md}"
tls_root=$(mktemp -d /tmp/dbdeck-tls-validation.XXXXXXXX)
tls_token="dbdeck-tls-$$-$(date +%s)"
tls_id=""
cleanup() {
    if [[ -n "$tls_id" && "$(docker inspect -f '{{index .Config.Labels "dbdeck.validation"}}' "$tls_id" 2>/dev/null || true)" == "$tls_token" ]]; then
        docker stop --time 5 "$tls_id" >/dev/null || true
        docker rm "$tls_id" >/dev/null || true
    fi
    rm -f "$tls_root/ca.crt" "$tls_root/server.crt" "$tls_root/client.crt" "$tls_root/client.key" "$tls_root/hosts" "$tls_root/nsswitch.conf" "$tls_root/wrong-ca.crt"
    rmdir "$tls_root"
}
trap cleanup EXIT
cp "$fixture/certs/ca.crt" "$fixture/certs/server.crt" "$fixture/certs/client.crt" "$tls_root/"
cp "$fixture/keys/client.key" "$tls_root/client.key"
chmod 600 "$tls_root/client.key"
cp "$DBDECK_TLS_WRONG_CA" "$tls_root/wrong-ca.crt"
printf '127.0.0.1 localhost testserver.com sqlx.rs\n::1 localhost\n' > "$tls_root/hosts"
sed 's/^hosts:.*/hosts: files dns/' /etc/nsswitch.conf > "$tls_root/nsswitch.conf"
export DBDECK_TLS_VALIDATION=1 DBDECK_TLS_FILES="$tls_root"
for engine in postgres mariadb mysql; do
    case "$engine" in
        postgres)
            export POSTGRES_PASSWORD="$DBDECK_PG_PASSWORD"
            export DBDECK_TLS_KIND=postgre_sql DBDECK_TLS_USER=dbdeck_test DBDECK_TLS_PASSWORD="$DBDECK_PG_PASSWORD"
            tls_id=$(docker run -d --label "dbdeck.validation=$tls_token" --tmpfs /var/lib/postgresql/data:rw --publish 127.0.0.1::5432 --mount "type=bind,src=$fixture,dst=/fixtures,readonly" --env POSTGRES_PASSWORD --env POSTGRES_USER=dbdeck_test --env POSTGRES_DB=dbdeck_validation --user root --entrypoint sh postgres:17.11-alpine -c 'cp /fixtures/keys/server.key /tmp/server.key; chmod 600 /tmp/server.key; chown postgres /tmp/server.key; printf "local all all trust\nhostssl all all all scram-sha-256 clientcert=verify-ca\nhost all all all scram-sha-256\n" > /tmp/hba.conf; exec docker-entrypoint.sh postgres -c ssl=on -c ssl_ca_file=/fixtures/certs/ca.crt -c ssl_cert_file=/fixtures/certs/server.crt -c ssl_key_file=/tmp/server.key -c hba_file=/tmp/hba.conf')
            ;;
        mariadb|mysql)
            if [[ "$engine" == mariadb ]]; then
                image=mariadb:11.4.13
                export DBDECK_TLS_DAEMON=mariadbd
                export MARIADB_ROOT_PASSWORD="$DBDECK_MARIADB_ROOT_PASSWORD"
                export DBDECK_TLS_KIND=maria_db DBDECK_TLS_PASSWORD="$DBDECK_MARIADB_ROOT_PASSWORD"
                env_args=(--env MARIADB_ROOT_PASSWORD --env MARIADB_ROOT_HOST=% --env MARIADB_DATABASE=dbdeck_validation)
            else
                image=mysql:8.4
                export DBDECK_TLS_DAEMON=mysqld
                export MYSQL_ROOT_PASSWORD="$DBDECK_MYSQL_ROOT_PASSWORD"
                export DBDECK_TLS_KIND=my_sql DBDECK_TLS_PASSWORD="$DBDECK_MYSQL_ROOT_PASSWORD"
                env_args=(--env MYSQL_ROOT_PASSWORD --env MYSQL_ROOT_HOST=% --env MYSQL_DATABASE=dbdeck_validation)
            fi
            export DBDECK_TLS_USER=root
            tls_id=$(docker run -d --label "dbdeck.validation=$tls_token" --tmpfs /var/lib/mysql:rw --publish 127.0.0.1::3306 --mount "type=bind,src=$fixture,dst=/fixtures,readonly" "${env_args[@]}" --env DBDECK_TLS_DAEMON --user root --entrypoint sh "$image" -c 'cp /fixtures/keys/server.key /tmp/server.key; chmod 600 /tmp/server.key; chown mysql /tmp/server.key; exec docker-entrypoint.sh "$DBDECK_TLS_DAEMON" --ssl-ca=/fixtures/certs/ca.crt --ssl-cert=/fixtures/certs/server.crt --ssl-key=/tmp/server.key')
            ;;
    esac
    for ((attempt=0;attempt<180;attempt++)); do
        if [[ "$engine" == postgres ]]; then
            if docker exec "$tls_id" pg_isready -h 127.0.0.1 -U dbdeck_test -d dbdeck_validation >/dev/null 2>&1; then break; fi
        elif [[ "$engine" == mariadb ]]; then
            if docker exec "$tls_id" sh -c 'MYSQL_PWD="$MARIADB_ROOT_PASSWORD" mariadb --skip-ssl -h 127.0.0.1 --protocol=tcp -N -e "ALTER USER '\''root'\''@'\''%'\'' REQUIRE X509"' >/dev/null 2>&1; then break; fi
        else
            if docker exec "$tls_id" sh -c 'MYSQL_PWD="$MYSQL_ROOT_PASSWORD" mysql --get-server-public-key --ssl-mode=DISABLED -h 127.0.0.1 --protocol=tcp -N -e "ALTER USER '\''root'\''@'\''%'\'' REQUIRE X509"' >/dev/null 2>&1; then break; fi
        fi
        if [[ "$(docker inspect -f '{{.State.Running}}' "$tls_id")" != true ]]; then docker logs --tail 30 "$tls_id" >&2; exit 1; fi
        if ((attempt==179)); then echo "Isolated TLS $engine startup timed out" >&2; docker logs --tail 30 "$tls_id" >&2 || true; exit 1; fi
        sleep 0.5
    done
    export DBDECK_TLS_PORT=$(docker port "$tls_id" | awk -F: 'NR==1 {print $NF}')
    # Bind mounting in a private user/mount namespace changes only this test's resolver.
    unshare -Ur -m bash -c 'mount --bind "$1/hosts" /etc/hosts; mount --bind "$1/nsswitch.conf" /etc/nsswitch.conf; exec "${DBDECK_CARGO:-cargo}" test --locked --offline --bin zer0-dbdeck isolated_tls_verification -- --ignored --nocapture --test-threads=1' dbdeck-tls "$tls_root"
    docker stop --time 5 "$tls_id" >/dev/null
    docker rm "$tls_id" >/dev/null
    tls_id=""
done
