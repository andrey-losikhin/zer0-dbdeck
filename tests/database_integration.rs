#![allow(dead_code)]

#[path = "../src/app_instance.rs"]
mod app_instance;
#[path = "../src/csv.rs"]
mod csv;
#[path = "../src/csv_input.rs"]
mod csv_input;
#[path = "../src/transfer.rs"]
mod transfer;
use csv::{write_csv_header, write_csv_row};

#[path = "../src/database.rs"]
mod database;
#[path = "../src/database_structure.rs"]
mod database_structure;
#[path = "../src/db_error.rs"]
mod db_error;
#[path = "../src/settings.rs"]
mod settings;
#[path = "../src/sql_risk.rs"]
mod sql_risk;
#[path = "../src/sql_workspace.rs"]
mod sql_workspace;

use database::{CellValue, DbObject, DbObjectKind, DbPool};
use settings::{ConnectionProfile, DatabaseKind, EnvironmentTag, TlsMode};
use sqlx::Row;

#[test]
#[ignore = "requires docker compose test databases; see README.md"]
fn postgres_browse_sample_table() {
    run_case(
        "postgres",
        DatabaseKind::PostgreSql,
        55432,
        "DBDECK_PG_PASSWORD",
    );
}

#[test]
#[ignore = "requires local PostgreSQL on 127.0.0.1:55432; see README.md"]
fn postgres_startup_eof_retries_once() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    let password = std::env::var("DBDECK_PG_PASSWORD").expect("set DBDECK_PG_PASSWORD from .env");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for disconnects in [1, usize::MAX, 0] {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let port = listener.local_addr().unwrap().port();
                let attempts = Arc::new(AtomicUsize::new(0));
                let proxy_attempts = Arc::clone(&attempts);
                let proxy = tokio::spawn(async move {
                    loop {
                        let (mut client, _) = listener.accept().await.unwrap();
                        let attempt = proxy_attempts.fetch_add(1, Ordering::Relaxed) + 1;
                        if attempt <= disconnects {
                            // Consume the full StartupMessage so closing sends FIN instead of RST.
                            let length = client.read_u32().await.unwrap() as usize;
                            assert!((8..=1024).contains(&length));
                            let mut startup = vec![0; length - 4];
                            client.read_exact(&mut startup).await.unwrap();
                            assert_eq!(&startup[..4], &196608_u32.to_be_bytes());
                            client.shutdown().await.unwrap();
                        } else {
                            let mut server =
                                TcpStream::connect(("127.0.0.1", 55432)).await.unwrap();
                            tokio::io::copy_bidirectional(&mut client, &mut server)
                                .await
                                .unwrap();
                        }
                    }
                });

                let mut profile =
                    system_catalog_profile(DatabaseKind::PostgreSql, port, "dbdeck_test");
                if disconnects == 0 {
                    profile.username = "dbdeck_nonexistent_retry_test_role".into();
                }
                let result = DbPool::connect(&profile, &password).await;
                match disconnects {
                    1 => {
                        let DbPool::PostgreSql(pool) =
                            result.expect("recover after StartupMessage EOF")
                        else {
                            panic!("expected a PostgreSQL pool");
                        };
                        pool.close().await;
                    }
                    0 => assert!(result.unwrap_err().contains("Ошибка аутентификации")),
                    _ => {
                        let error = result.unwrap_err();
                        assert!(error.contains("expected to read 5 bytes"), "{error}");
                    }
                }
                assert_eq!(
                    attempts.load(Ordering::Relaxed),
                    if disconnects == 0 { 1 } else { 2 }
                );
                assert!(!proxy.is_finished(), "proxy stopped unexpectedly");
                proxy.abort();
                assert!(proxy.await.unwrap_err().is_cancelled());
            }
        });
}

#[test]
#[ignore = "requires docker compose test databases; see README.md"]
fn mariadb_browse_sample_table() {
    run_case(
        "mariadb",
        DatabaseKind::MariaDb,
        53306,
        "DBDECK_MARIADB_PASSWORD",
    );
}

#[test]
#[ignore = "requires docker compose test databases; see README.md"]
fn mysql_browse_sample_table() {
    run_case("mysql", DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD");
}

fn run_case(id: &str, kind: DatabaseKind, port: u16, password_key: &str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create Tokio runtime");
    runtime.block_on(async {
        let profile = ConnectionProfile {
            id: id.to_owned(),
            name: id.to_owned(),
            kind,
            host: "127.0.0.1".to_owned(),
            port,
            database: "dbdeck_test".to_owned(),
            username: "dbdeck_test".to_owned(),
            environment: EnvironmentTag::Local,
            color: "#4f8fe8".to_owned(),
            read_only: true,
            tls_mode: TlsMode::Disabled,
            password_saved: false,
        gopass_entry: None, tls_files: Default::default(), native_tools_dir: None,
        };
        let password =
            std::env::var(password_key).unwrap_or_else(|_| panic!("set {password_key} from .env"));
        let pool = DbPool::connect(&profile, &password)
            .await
            .unwrap_or_else(|error| panic!("{id} connect failed: {error}"));
        pool.check()
            .await
            .unwrap_or_else(|error| panic!("{id} check failed: {error}"));
        for sql in ["SELECT * FROM sample_records ORDER BY id LIMIT 20", "SELECT id, name FROM sample_records WHERE id != 4 ORDER BY id DESC"] {
            let result=pool.execute_sql(sql).await.unwrap();
            assert!(result.editable_object.is_some(), "{id}: {sql}");
            assert_eq!(result.data.primary_key,vec!["id"]);
        }
        for sql in ["SELECT name FROM sample_records", "SELECT id AS name FROM sample_records", "SELECT COUNT(*) FROM sample_records", "SELECT a.id, a.name FROM sample_records a JOIN sample_records b ON a.id=b.id", "SELECT * FROM sample_records; SELECT * FROM typed_values"] {
            assert!(pool.execute_sql(sql).await.unwrap().editable_object.is_none(), "{id}: {sql}");
        }

        let probe = match &pool {
            DbPool::PostgreSql(pool) => sqlx::query("SELECT 'Привет' AS probe")
                .fetch_one(pool)
                .await
                .expect("postgres unicode query")
                .try_get::<String, _>("probe")
                .expect("postgres unicode decode"),
            DbPool::MySql(pool) => sqlx::query("SELECT 'Привет' AS probe")
                .fetch_one(pool)
                .await
                .expect("mysql unicode query")
                .try_get::<String, _>("probe")
                .expect("mysql unicode decode"),
        };
        assert_eq!(probe, "Привет");
        let tables = pool
            .list_tables(&profile)
            .await
            .unwrap_or_else(|error| panic!("{id} list tables failed: {error}"));
        let expected_schema = if matches!(kind, DatabaseKind::PostgreSql) {
            "public"
        } else {
            "dbdeck_test"
        };
        assert!(!tables.truncated);
        assert!(tables.objects.iter().any(|table| {
            table.schema == expected_schema && table.name == "sample_records"
        }));
        let data = pool
            .read_table(&DbObject {
                schema: if matches!(kind, DatabaseKind::PostgreSql) {
                    "public".to_owned()
                } else {
                    "dbdeck_test".to_owned()
                },
                name: "sample_records".to_owned(),
                kind: DbObjectKind::Table,
            })
            .await
            .unwrap_or_else(|error| panic!("{id} read table failed: {error}"));
        assert!(
            data.rows.len() >= 16,
            "{id} should contain at least sixteen fixture rows"
        );
        assert_eq!(data.columns, ["id", "name", "note", "created_at"]);
        assert!(!data.truncated);
        assert!(!data.truncated_columns);
        assert!(data.rows.iter().all(|row| row.len() == 4));
        assert!(data.rows.iter().any(|row| row[0] == CellValue::Text("1".to_owned())));
        assert!(data.rows.iter().all(|row| matches!(&row[3], CellValue::Text(value) if !value.is_empty())));
        assert!(data.rows.iter().any(|row| matches!(&row[2], CellValue::Text(note) if note.contains("Unicode") && note.contains(',') && note.contains('"'))));
        assert!(data.rows.iter().any(|row| row[2] == CellValue::Null));

        let raw_object = DbObject {
            schema: expected_schema.to_owned(), name: "sample_records".to_owned(), kind: DbObjectKind::Table,
        };
        let filtered = pool.read_table_view_with_condition(&raw_object, &[], Some("WHERE id != 4 AND (id <= 8 OR id = 10)"),
            Some(&database::TableSort { column: "id".to_owned(), descending: false }), 0).await.expect("raw filter");
        assert!(!filtered.rows.is_empty());
        for row in &filtered.rows {
            let CellValue::Text(id) = &row[0] else { panic!("id should be text") };
            let id: i64 = id.parse().unwrap();
            assert!(id != 4 && (id <= 8 || id == 10));
        }
        let page = pool.read_table_view_with_condition(&raw_object, &[], Some("id != 4 AND (id <= 8 OR id = 10)"),
            Some(&database::TableSort { column: "id".to_owned(), descending: false }), 2).await.expect("raw filter offset");
        assert_eq!(page.rows, filtered.rows[2..]);
        assert!(pool.read_table_view_with_condition(&raw_object, &[], Some("id ="), None, 0).await.is_err());
        pool.read_table(&raw_object).await.expect("connection usable after SQL error/rollback");
        check_browse_continuation(&pool, &raw_object).await;
        let typed_object = DbObject {
            schema: expected_schema.to_owned(),
            name: "typed_values".to_owned(),
            kind: DbObjectKind::Table,
        };
        let typed = pool
            .read_table(&typed_object)
            .await
            .unwrap_or_else(|error| panic!("{id} read typed fixture failed: {error}"));
        assert!(
            typed.rows.len() >= 8,
            "{id} should contain at least eight typed fixture rows"
        );
        assert_eq!(
            typed.columns,
            [
                "id",
                "label",
                "date_value",
                "timestamp_value",
                "amount",
                "enabled",
                "metadata",
                "payload",
                "optional_text"
            ]
        );
        assert!(typed.rows.iter().any(|row| row[2] == CellValue::Null));
        assert!(typed.rows.iter().any(|row| {
            matches!(&row[6], CellValue::Text(value) if value.contains("json") || value.contains("JSON"))
        }));
        assert!(typed.rows.iter().any(|row| {
            matches!(&row[7], CellValue::Text(value) if value.starts_with("0x"))
        }));
        assert!(typed.rows.iter().any(|row| {
            matches!(&row[8], CellValue::Truncated(value) if value.len() >= 1024)
        }));
        let row = typed.rows.iter().find(|row| row[0] == CellValue::Text("4".to_owned())).unwrap();
        let key = typed.cell_detail_key(row).unwrap();
        assert_eq!(
            pool.read_table_cell(&typed_object, "optional_text", &key)
                .await
                .unwrap(),
            CellValue::Text("длинный-текст-".repeat(120))
        );
        let binary = pool.read_table_cell(&typed_object, "payload", &key).await.unwrap();
        assert!(matches!(binary, CellValue::Text(value) if value.eq_ignore_ascii_case("0xdeadbeef")));
        let mut missing = key.clone();
        missing[0].1 = "0".to_owned();
        assert!(pool.read_table_cell(&typed_object, "optional_text", &missing).await.is_err());
        let mut stale_key = key.clone();
        stale_key[0].0.name = "label".to_owned();
        assert!(pool.read_table_cell(&typed_object, "optional_text", &stale_key).await.is_err());
        let mut stale_type = key;
        stale_type[0].0.key_type = "old type".to_owned();
        assert!(pool.read_table_cell(&typed_object, "optional_text", &stale_type).await.is_err());

        if let DbPool::PostgreSql(pool) = &pool {
            check_cell_detail_bounds(pool.connect_options().as_ref().clone()).await;
        }
    });
}

async fn check_browse_continuation(pool: &DbPool, object: &DbObject) {
    use database::{FilterLogic, FilterOperator, TableFilter, TableSort, TableViewState};

    for (column, descending) in [("id", false), ("id", true), ("name", false)] {
        let view = TableViewState {
            filters: [
                (FilterLogic::And, FilterOperator::Less, "5"),
                (FilterLogic::Or, FilterOperator::Greater, "8"),
            ]
            .into_iter()
            .map(|(logic, operator, value)| TableFilter {
                column: "id".to_owned(),
                logic,
                operator,
                value: value.to_owned(),
            })
            .collect(),
            sort: Some(TableSort {
                column: column.to_owned(),
                descending,
            }),
            ..Default::default()
        };
        let reference = pool
            .read_table_view(object, &view.filters, view.sort.as_ref(), 0)
            .await
            .unwrap();
        assert!(reference.rows.len() > 5);
        let mut current = reference.clone();
        current.rows.truncate(5);
        if column == "id" {
            // Removing rows from the client snapshot must not move the continuation.
            current.rows.drain(..2);
        }
        let page = pool
            .read_next_table_page(object, &view, &current)
            .await
            .unwrap();
        assert_eq!(
            page.rows,
            reference.rows[5..],
            "sort {column} DESC={descending}"
        );
        assert!(!page.truncated);

        current.cell_detail_key_columns[0]
            .key_type
            .push_str(" old type");
        assert!(
            pool.read_next_table_page(object, &view, &current)
                .await
                .is_err()
        );
    }
}

async fn check_cell_detail_bounds(options: sqlx::postgres::PgConnectOptions) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TEMP TABLE cell_detail ("part""a" text, part_b bytea, body text, bytes bytea, PRIMARY KEY ("part""a", part_b));
           INSERT INTO cell_detail VALUES ('x', decode('1a', 'hex'), NULL, NULL),
               ('x', decode('1b', 'hex'), repeat('🦉', 16385), decode(repeat('ab', 8192), 'hex'));"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let schema = sqlx::query_scalar("SELECT pg_my_temp_schema()::regnamespace::text")
        .fetch_one(&pool)
        .await
        .unwrap();
    let object = DbObject {
        schema,
        name: "cell_detail".to_owned(),
        kind: DbObjectKind::Table,
    };
    let database = DbPool::PostgreSql(pool.clone());
    let data = database.read_table(&object).await.unwrap();
    let row = data
        .rows
        .iter()
        .find(|row| row[1] == CellValue::Text("0x1b".to_owned()))
        .unwrap();
    let mut key = data.cell_detail_key(row).unwrap();
    assert_eq!(
        database
            .read_table_cell(&object, "body", &key)
            .await
            .unwrap(),
        CellValue::Truncated("🦉".repeat(database::MAX_CELL_DETAIL_CHARS)),
    );
    assert_eq!(
        database
            .read_table_cell(&object, "bytes", &key)
            .await
            .unwrap(),
        CellValue::Truncated(format!(
            "0x{}",
            "ab".repeat((database::MAX_CELL_DETAIL_CHARS - 2) / 2)
        )),
    );
    key[1].1 = "0x1a".to_owned();
    assert_eq!(
        database
            .read_table_cell(&object, "body", &key)
            .await
            .unwrap(),
        CellValue::Null
    );
    sqlx::query("CREATE TEMP TABLE cell_detail_child () INHERITS (cell_detail)")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        database
            .read_table_cell(&object, "body", &key)
            .await
            .is_err()
    );
    sqlx::raw_sql(
        "CREATE TEMP TABLE cell_detail_partitioned (id int PRIMARY KEY, body text) PARTITION BY RANGE (id);
         CREATE TEMP TABLE cell_detail_partition PARTITION OF cell_detail_partitioned FOR VALUES FROM (0) TO (10);
         INSERT INTO cell_detail_partitioned VALUES (1, 'partition value');",
    )
    .execute(&pool)
    .await
    .unwrap();
    check_postgres_page_keys(&pool, &object.schema).await;
    let partitioned = DbObject {
        name: "cell_detail_partitioned".to_owned(),
        ..object
    };
    let data = database.read_table(&partitioned).await.unwrap();
    let key = data.cell_detail_key(&data.rows[0]).unwrap();
    assert_eq!(
        database
            .read_table_cell(&partitioned, "body", &key)
            .await
            .unwrap(),
        CellValue::Text("partition value".to_owned()),
    );
}

async fn check_postgres_page_keys(pool: &sqlx::PgPool, schema: &str) {
    sqlx::raw_sql(
        "CREATE TEMP TABLE inherited_page (id int PRIMARY KEY);
         CREATE TEMP TABLE child_page () INHERITS (inherited_page);
         INSERT INTO inherited_page VALUES (1);
         INSERT INTO child_page SELECT 1 FROM generate_series(1, 200);
         INSERT INTO child_page VALUES (2);
         CREATE TEMP TABLE uuid_page (id uuid PRIMARY KEY) PARTITION BY RANGE (id);
         CREATE TEMP TABLE uuid_page_first PARTITION OF uuid_page
             FOR VALUES FROM (MINVALUE) TO ('00000000-0000-0000-0000-0000000000c9');
         CREATE TEMP TABLE uuid_page_rest PARTITION OF uuid_page
             FOR VALUES FROM ('00000000-0000-0000-0000-0000000000c9') TO (MAXVALUE);
         INSERT INTO uuid_page SELECT lpad(to_hex(n), 32, '0')::uuid
             FROM generate_series(1, 260) AS n;",
    )
    .execute(pool)
    .await
    .unwrap();
    let database = DbPool::PostgreSql(pool.clone());
    let mut object = DbObject {
        schema: schema.to_owned(),
        name: "inherited_page".to_owned(),
        kind: DbObjectKind::Table,
    };
    let view = database::TableViewState::default();
    let first = database.read_table(&object).await.unwrap();
    assert!(first.truncated);
    let next = database
        .read_next_table_page(&object, &view, &first)
        .await
        .unwrap();
    assert_eq!(
        next.rows,
        [
            vec![CellValue::Text("1".into())],
            vec![CellValue::Text("2".into())]
        ]
    );

    object.name = "uuid_page".to_owned();
    let mut first = database.read_table(&object).await.unwrap();
    assert!(first.truncated);
    first.rows.drain(..2);
    let next = database
        .read_next_table_page(&object, &view, &first)
        .await
        .unwrap();
    assert_eq!(next.rows.len(), 60);
    assert_eq!(
        next.rows[0],
        [CellValue::Text(
            "00000000-0000-0000-0000-0000000000c9".into()
        )]
    );
    assert!(!next.truncated);
}
#[path = "../src/secret_store.rs"]
mod secret_store;

#[test]
#[ignore = "requires Compose DBs; creates only connection-local TEMP tables"]
fn query_origin_follows_the_selected_connection_and_rejects_mysql_temp_shadowing() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let pg_options=sqlx::postgres::PgConnectOptions::new().host("127.0.0.1").port(55432).database("dbdeck_test").username("dbdeck_test")
            .password(&std::env::var("DBDECK_PG_PASSWORD").unwrap());
        let pg=sqlx::postgres::PgPoolOptions::new().max_connections(2).connect_with(pg_options).await.unwrap();
        let mut a=pg.acquire().await.unwrap();let mut b=pg.acquire().await.unwrap();
        let mut schemas=std::collections::HashMap::new();
        for (connection,marker) in [(&mut a,"a"),(&mut b,"b")] {
            sqlx::raw_sql("CREATE TEMP TABLE sample_records (id int PRIMARY KEY, name text); INSERT INTO sample_records VALUES (1,'placeholder');")
                .execute(&mut **connection).await.unwrap();
            sqlx::query("UPDATE sample_records SET name=$1").bind(marker).execute(&mut **connection).await.unwrap();
            let schema:String=sqlx::query_scalar("SELECT pg_my_temp_schema()::regnamespace::text").fetch_one(&mut **connection).await.unwrap();
            schemas.insert(marker.to_owned(),schema);
        }
        drop(a);drop(b);
        let db=DbPool::PostgreSql(pg.clone());
        for _ in 0..8 {
            let result=db.execute_sql("SELECT * FROM sample_records").await.unwrap();
            let CellValue::Text(marker)=&result.data.rows[0][1] else {panic!("marker")};
            assert_eq!(result.editable_object.unwrap().schema,schemas[marker]);
        }
        pg.close().await;
        for (port,key) in [(53306,"DBDECK_MARIADB_PASSWORD"),(53307,"DBDECK_MYSQL_PASSWORD")] {
            let options=sqlx::mysql::MySqlConnectOptions::new().host("127.0.0.1").port(port).database("dbdeck_test").username("dbdeck_test")
                .password(&std::env::var(key).unwrap());
            let pool=sqlx::mysql::MySqlPoolOptions::new().max_connections(1).connect_with(options).await.unwrap();
            sqlx::raw_sql("CREATE TEMPORARY TABLE sample_records (id bigint PRIMARY KEY, name varchar(255), note text, created_at timestamp DEFAULT CURRENT_TIMESTAMP); INSERT INTO sample_records (id,name) VALUES (1,'temporary');")
                .execute(&pool).await.unwrap();
            let db=DbPool::MySql(pool.clone());
            let result=db.execute_sql("SELECT * FROM dbdeck_test.sample_records").await.unwrap();
            assert!(result.editable_object.is_none(),"temporary shadow must not use permanent metadata");
            let object=DbObject{schema:"dbdeck_test".into(),name:"sample_records".into(),kind:DbObjectKind::Table};
            assert!(db.update_table_cells(&object,&[database::TableCellEdit{primary_key_values:vec![CellValue::Text("1".into())],column:"name".into(),original_value:CellValue::Text("temporary".into()),value:Some("wrong".into())}]).await.is_err());
            assert!(db.delete_table_row(&object,&[CellValue::Text("1".into())]).await.is_err());
            let value:String=sqlx::query_scalar("SELECT name FROM sample_records WHERE id=1").fetch_one(&pool).await.unwrap();
            assert_eq!(value,"temporary");
            pool.close().await;
        }
    });
}

#[test]
#[ignore = "requires local test databases; reads only metadata of existing fixtures"]
fn inspect_structure_postgres_mariadb_mysql() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for (kind, port, variable) in [
            (DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD"),
            (DatabaseKind::MariaDb, 53306, "DBDECK_MARIADB_PASSWORD"),
            (DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD"),
        ] {
            let profile = ConnectionProfile {
                id: "structure-test".into(),
                name: "test".into(),
                kind,
                host: "127.0.0.1".into(),
                port,
                database: "dbdeck_test".into(),
                username: "dbdeck_test".into(),
                environment: EnvironmentTag::Local,
                color: "#000000".into(),
                read_only: true,
                tls_mode: TlsMode::Disabled,
                password_saved: false,
                gopass_entry: None,
                tls_files: Default::default(),
                native_tools_dir: None,
            };
            let pool = DbPool::connect(
                &profile,
                &std::env::var(variable).expect("set test database password"),
            )
            .await
            .unwrap();
            let object = DbObject {
                schema: if matches!(kind, DatabaseKind::PostgreSql) {
                    "public"
                } else {
                    "dbdeck_test"
                }
                .into(),
                name: "sample_records".into(),
                kind: DbObjectKind::Table,
            };
            let structure = pool.inspect_object(&object).await.unwrap();
            assert!(
                structure
                    .columns
                    .rows
                    .iter()
                    .any(|row| row[0] == CellValue::Text("id".into()))
            );
            assert!(
                structure
                    .columns
                    .rows
                    .iter()
                    .any(|row| row[0] == CellValue::Text("name".into()))
            );
            assert!(
                structure
                    .constraints
                    .rows
                    .iter()
                    .any(|row| row[1] == CellValue::Text("PRIMARY KEY".into())),
                "port={port}: {:?}",
                structure.constraints
            );
            assert!(!structure.indexes.rows.is_empty());
            assert!(
                structure.ddl.contains("CREATE TABLE") && structure.ddl.contains("PRIMARY KEY")
            );
            assert!(!structure.ddl_note.is_empty());
            let absent = DbObject {
                name: "dbdeck_absent_object_5a985a06".into(),
                ..object.clone()
            };
            assert!(pool.inspect_object(&absent).await.is_err());
        }
    });
}

#[test]
#[ignore = "requires local fixture databases; DBDECK_STRUCTURE_ENGINE can select one engine"]
fn compare_table_structure_across_connections() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let selected = std::env::var("DBDECK_STRUCTURE_ENGINE").ok();
        let mut tested = 0;
        for (engine, kind, port, variable) in [
            (
                "postgres",
                DatabaseKind::PostgreSql,
                55432,
                "DBDECK_PG_PASSWORD",
            ),
            (
                "mariadb",
                DatabaseKind::MariaDb,
                53306,
                "DBDECK_MARIADB_PASSWORD",
            ),
            ("mysql", DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD"),
        ] {
            if selected
                .as_deref()
                .is_some_and(|selected| selected != engine)
            {
                continue;
            }
            tested += 1;
            let password = std::env::var(variable).expect("set fixture password");
            let profile = ConnectionProfile {
                id: "structure-source".into(),
                name: "source".into(),
                kind,
                host: "127.0.0.1".into(),
                port,
                database: "dbdeck_test".into(),
                username: "dbdeck_test".into(),
                environment: EnvironmentTag::Local,
                color: "#000000".into(),
                read_only: true,
                tls_mode: TlsMode::Disabled,
                password_saved: false,
                gopass_entry: None,
                tls_files: Default::default(),
                native_tools_dir: None,
            };
            let postgres = matches!(kind, DatabaseKind::PostgreSql);
            let target_profile = ConnectionProfile {
                id: "structure-target".into(),
                database: if postgres {
                    "dbdeck_test"
                } else {
                    "dbdeck_analytics"
                }
                .into(),
                ..profile.clone()
            };
            let source_pool = DbPool::connect(&profile, &password).await.unwrap();
            let target_pool = DbPool::connect(&target_profile, &password).await.unwrap();
            let table = |schema: &str, name: &str| DbObject {
                schema: schema.into(),
                name: name.into(),
                kind: DbObjectKind::Table,
            };
            let (source, equivalent, different) = if postgres {
                (
                    table("crm", "events"),
                    table("audit", "events"),
                    table("public", "sample_records"),
                )
            } else {
                (
                    table("dbdeck_test", "sample_records"),
                    table("dbdeck_test", "sample_records"),
                    table("dbdeck_analytics", "sample_records"),
                )
            };
            let equal = source_pool
                .compare_table_structure(&source, &target_pool, &equivalent)
                .await
                .unwrap();
            assert!(equal.is_empty(), "{engine}: {equal:?}");
            let differences = source_pool
                .compare_table_structure(&source, &target_pool, &different)
                .await
                .unwrap();
            let changed = |name| {
                differences
                    .iter()
                    .find(|item| item.category == "Столбец" && item.name == name)
                    .unwrap()
            };
            if postgres {
                assert!(changed("description").after.is_none());
                assert!(changed("name").before.is_none());
                assert!(
                    differences
                        .iter()
                        .any(|item| item.category == "Ограничение")
                );
                assert!(differences.iter().any(|item| item.category == "Индекс"));
                let changed_options = source_pool
                    .compare_table_structure(
                        &table("crm", "comparison_index_options"),
                        &target_pool,
                        &table("audit", "comparison_index_options"),
                    )
                    .await
                    .unwrap();
                let [index] = changed_options.as_slice() else {
                    panic!("expected one changed index, got {changed_options:?}");
                };
                assert_eq!(index.name, "comparison_brin");
                assert!(
                    index
                        .before
                        .as_ref()
                        .unwrap()
                        .contains("values_per_range=16")
                );
                assert!(
                    index
                        .after
                        .as_ref()
                        .unwrap()
                        .contains("values_per_range=32")
                );
                let empty = table("audit", "comparison_empty");
                assert!(
                    source_pool
                        .inspect_object(&empty)
                        .await
                        .unwrap()
                        .columns
                        .rows
                        .is_empty()
                );
                assert!(
                    source_pool
                        .compare_table_structure(&empty, &target_pool, &empty)
                        .await
                        .unwrap()
                        .is_empty()
                );
            } else {
                let name = changed("name");
                assert!(name.before.as_ref().unwrap().contains("varchar(255)"));
                assert!(name.after.as_ref().unwrap().contains("varchar(150)"));
                let changes = source_pool
                    .compare_table_structure(
                        &table("dbdeck_test", "comparison_case"),
                        &target_pool,
                        &table("dbdeck_analytics", "comparison_case"),
                    )
                    .await
                    .unwrap();
                let [check] = changes.as_slice() else {
                    panic!("{engine}: expected only the changed CHECK, got {changes:?}");
                };
                assert_eq!(check.name, "comparison_positive");
                let before = check.before.as_ref().unwrap();
                let after = check.after.as_ref().unwrap();
                assert!(
                    before.contains('0') && after.contains('1'),
                    "{before} -> {after}"
                );
            }
            let absent = table(&source.schema, "dbdeck_absent_comparison");
            for (left, right, prefix) in [
                (&absent, &different, "Исходная таблица"),
                (&source, &absent, "Таблица для сравнения"),
            ] {
                let error = source_pool
                    .compare_table_structure(left, &target_pool, right)
                    .await
                    .unwrap_err();
                assert!(error.starts_with(prefix), "{error}");
            }
        }
        assert!(tested > 0, "unknown DBDECK_STRUCTURE_ENGINE");
    });
}

#[test]
#[ignore = "requires Compose databases with 003 fixtures; read-only catalog/SQL checks"]
fn multiple_namespaces_and_unformatted_sql() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        for (kind, port, key) in [
            (DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD"),
            (DatabaseKind::MariaDb, 53306, "DBDECK_MARIADB_PASSWORD"),
            (DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD"),
        ] {
            let profile = ConnectionProfile {
                id: format!("namespaces-{port}"), name: "namespaces".into(), kind,
                host: "127.0.0.1".into(), port,
                database: if matches!(kind, DatabaseKind::PostgreSql) { "dbdeck_test".into() } else { String::new() },
                username: "dbdeck_test".into(), environment: EnvironmentTag::Local,
                color: "#4f8fe8".into(), read_only: true, tls_mode: TlsMode::Disabled,
                password_saved: false, gopass_entry: None, tls_files: Default::default(), native_tools_dir: None,
            };
            let pool = DbPool::connect(&profile, &std::env::var(key).unwrap()).await.unwrap();
            assert_eq!(pool.server_icon().await.unwrap(), match port { 55432 => "postgresql", 53306 => "mariadb", _ => "mysql" });
            let catalog = pool.list_tables(&profile).await.unwrap();
            let namespaces: &[(&str, &str)] = if matches!(kind, DatabaseKind::PostgreSql) {
                &[("crm", "events"), ("audit", "events")]
            } else {
                &[("dbdeck_test", "sample_records"), ("dbdeck_analytics", "sample_records")]
            };
            let completion = pool.list_sql_completions().await.unwrap();
            if matches!(kind, DatabaseKind::PostgreSql) {
                assert!(completion.search_path.iter().any(|schema| schema == "public"));
            } else {
                assert!(completion.search_path.is_empty());
            }
            for &(schema, name) in namespaces {
                assert_eq!(completion.namespaces[schema][name].first().map(String::as_str), Some("id"));
                assert!(catalog.schemas.iter().any(|s| s == schema), "{port}: {schema}");
                let object = catalog.objects.iter().find(|o| o.schema == schema && o.name == name).unwrap();
                let browsed = pool.read_table(object).await.unwrap();
                assert!(!browsed.rows.is_empty());
                assert_eq!(browsed.primary_key, ["id"]);
                let compact = pool.execute_sql(&format!("SELECT * FROM {schema}.{name} WHERE id > 0 ORDER BY id LIMIT 2")).await.unwrap();
                let untidy = pool.execute_sql(&format!(
                    " \r\n\tSELECT    *   \r\n  FROM\t {schema}.{name}\r\n  -- keep the following WHERE on a separate line\r\n  WHERE   id    >    0\n\tORDER  BY\t id\n LIMIT  2 ; \r\n"
                )).await.unwrap();
                assert_eq!(compact.data.columns, untidy.data.columns);
                assert_eq!(compact.data.rows, untidy.data.rows, "{port}: whitespace must not change data");
                assert_eq!(untidy.editable_object.as_ref().map(|o| o.schema.as_str()), Some(schema));
            }
            if matches!(kind, DatabaseKind::MySql | DatabaseKind::MariaDb) {
                let mut scoped = profile.clone();
                scoped.database = "dbdeck_analytics".into();
                let selected = pool.connect_database(&scoped.database).await.unwrap();
                let selected_completion = selected.list_sql_completions().await.unwrap();
                assert_eq!(selected_completion.search_path, ["dbdeck_analytics"]);
                assert!(selected_completion.namespaces["dbdeck_test"].contains_key("sample_records"));
                let only_analytics = pool.list_tables(&scoped).await.unwrap();
                assert_eq!(only_analytics.schemas, ["dbdeck_analytics"]);
                assert!(only_analytics.objects.iter().all(|o| o.schema == "dbdeck_analytics"));
            }
            let literal = "a  b\tc\r\n d";
            let result = pool.execute_sql(&format!(
                "\r\n SELECT  0 AS ignored;\r\n\t/* copied query */\n SELECT\t'{literal}'   AS   preserved ;\r\n"
            )).await.unwrap();
            assert_eq!(result.data.columns, ["preserved"]);
            assert_eq!(result.data.rows, [vec![CellValue::Text(literal.into())]]);
            println!("{port}: multiple namespaces, scoped catalog, untidy SQL and unchanged literal passed");
            match pool {
                DbPool::PostgreSql(pool) => pool.close().await,
                DbPool::MySql(pool) => pool.close().await,
            }
        }
    });
}

#[test]
#[ignore = "requires Compose PostgreSQL; read-only server browser and separate database pool"]
fn postgres_blank_database_lists_databases_and_opens_schemas() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let mut profile = ConnectionProfile {
            id: "pg-server-browser".into(),
            name: "server".into(),
            kind: DatabaseKind::PostgreSql,
            host: "127.0.0.1".into(),
            port: 55432,
            database: String::new(),
            username: "dbdeck_test".into(),
            environment: EnvironmentTag::Local,
            color: "#4f8fe8".into(),
            read_only: true,
            tls_mode: TlsMode::Disabled,
            password_saved: false,
            gopass_entry: None,
            tls_files: Default::default(),
            native_tools_dir: None,
        };
        let password = std::env::var("DBDECK_PG_PASSWORD").unwrap();
        let root = DbPool::connect(&profile, &password).await.unwrap();
        let databases = root.postgres_databases().await.unwrap();
        assert!(databases.iter().any(|name| name == "dbdeck_test"));
        assert!(databases.iter().any(|name| name == "postgres"));
        assert!(
            !databases
                .iter()
                .any(|name| name == "template0" || name == "template1")
        );
        let selected = root.connect_database("dbdeck_test").await.unwrap();
        profile.database = "dbdeck_test".into();
        let catalog = selected.list_tables(&profile).await.unwrap();
        for schema in ["public", "crm", "audit"] {
            assert!(catalog.schemas.iter().any(|name| name == schema));
        }
        let other = root.connect_database("postgres").await.unwrap();
        for (pool, expected) in [
            (&root, "dbdeck_test"),
            (&selected, "dbdeck_test"),
            (&other, "postgres"),
        ] {
            let current = pool
                .execute_sql("SELECT current_database() AS name")
                .await
                .unwrap();
            assert_eq!(current.data.rows, [vec![CellValue::Text(expected.into())]]);
        }
        profile.database = "dbdeck_absent_29a4d375".into();
        assert!(
            DbPool::connect(&profile, &password).await.is_err(),
            "explicit database must not fall back"
        );
        for pool in [root, selected, other] {
            if let DbPool::PostgreSql(pool) = pool {
                pool.close().await;
            }
        }
    });
}

fn system_catalog_profile(kind: DatabaseKind, port: u16, database: &str) -> ConnectionProfile {
    ConnectionProfile {
        id: format!("system-catalog-{port}"),
        name: "system catalog".into(),
        kind,
        host: "127.0.0.1".into(),
        port,
        database: database.into(),
        username: "dbdeck_test".into(),
        environment: EnvironmentTag::Local,
        color: "#4f8fe8".into(),
        read_only: false,
        tls_mode: TlsMode::Disabled,
        password_saved: false,
        gopass_entry: None,
        tls_files: Default::default(),
        native_tools_dir: None,
    }
}

fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}_{}", std::process::id(), nanos % 1_000_000_000)
}

fn has_schema(catalog: &database::TableCatalog, name: &str) -> bool {
    catalog.schemas.iter().any(|schema| schema == name)
}

async fn pool_close(pool: DbPool) {
    match pool {
        DbPool::PostgreSql(pool) => pool.close().await,
        DbPool::MySql(pool) => pool.close().await,
    }
}

#[test]
#[ignore = "requires Compose PostgreSQL; creates and drops temporary schemas"]
fn postgres_system_schemas_follow_include_system_flag() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let profile = system_catalog_profile(DatabaseKind::PostgreSql, 55432, "dbdeck_test");
        let password = std::env::var("DBDECK_PG_PASSWORD").unwrap();
        let pool = DbPool::connect(&profile, &password).await.unwrap();
        let DbPool::PostgreSql(raw) = &pool else {
            panic!("expected PostgreSQL pool")
        };
        let suffix = unique_suffix();
        // Names mimic the pg_toast / pg_temp_ LIKE filters with the underscore
        // replaced, so an unescaped `_` wildcard would hide them.
        let lookalikes = [format!("pgxtoast_{suffix}"), format!("pgxtemp_{suffix}")];
        let plain = format!("dbdeck_it_user_{suffix}");
        let all: Vec<&String> = lookalikes.iter().chain([&plain]).collect();
        for schema in &all {
            sqlx::query(&format!("CREATE SCHEMA \"{schema}\""))
                .execute(raw)
                .await
                .unwrap();
            sqlx::query(&format!("CREATE TABLE \"{schema}\".t (id int)"))
                .execute(raw)
                .await
                .unwrap();
        }
        let hidden = pool.list_tables_with_system(&profile, false).await;
        let shown = pool.list_tables_with_system(&profile, true).await;
        let default = pool.list_tables(&profile).await;
        for schema in &all {
            let _ = sqlx::query(&format!("DROP SCHEMA \"{schema}\" CASCADE"))
                .execute(raw)
                .await;
        }
        let (hidden, shown, default) = (hidden.unwrap(), shown.unwrap(), default.unwrap());

        for system in ["pg_catalog", "information_schema", "pg_toast"] {
            assert!(!has_schema(&hidden, system), "false must hide {system}");
            assert!(has_schema(&shown, system), "true must show {system}");
        }
        assert!(
            hidden
                .objects
                .iter()
                .all(|o| o.schema != "pg_catalog" && o.schema != "information_schema")
        );
        assert!(shown.objects.iter().any(|o| o.schema == "pg_catalog"));
        assert!(
            shown
                .objects
                .iter()
                .any(|o| o.schema == "information_schema")
        );
        assert_eq!(default.schemas, hidden.schemas);
        for catalog in [&hidden, &shown] {
            assert!(has_schema(catalog, "public"));
            for schema in &all {
                assert!(has_schema(catalog, schema), "{schema} must stay visible");
                assert!(
                    catalog
                        .objects
                        .iter()
                        .any(|o| &o.schema == *schema && o.name == "t")
                );
            }
        }
        pool_close(pool).await;
    });
}

async fn mysql_family_system_check(kind: DatabaseKind, port: u16, key: &str, root_key: &str) {
    let profile = system_catalog_profile(kind, port, "");
    let password = std::env::var(key).unwrap();
    let pool = DbPool::connect(&profile, &password).await.unwrap();
    // dbdeck_test cannot create databases: the fixtures are made by root and granted to it.
    let mut admin_profile = system_catalog_profile(kind, port, "");
    admin_profile.username = "root".into();
    let admin = DbPool::connect(&admin_profile, &std::env::var(root_key).unwrap())
        .await
        .unwrap();
    let DbPool::MySql(raw) = &admin else {
        panic!("expected MySQL pool")
    };
    let suffix = unique_suffix();
    let plain = format!("dbdeck_it_user_{suffix}");
    // Look-alikes of system names that must never be treated as system.
    let lookalikes = [
        format!("mysql_{suffix}"),
        format!("sys_{suffix}"),
        format!("performance_schemax_{suffix}"),
    ];
    let all: Vec<&String> = lookalikes.iter().chain([&plain]).collect();
    for db in &all {
        sqlx::query(&format!("CREATE DATABASE `{db}`"))
            .execute(raw)
            .await
            .unwrap();
        sqlx::query(&format!("CREATE TABLE `{db}`.t (id int)"))
            .execute(raw)
            .await
            .unwrap();
        sqlx::query(&format!("GRANT ALL ON `{db}`.* TO 'dbdeck_test'@'%'"))
            .execute(raw)
            .await
            .unwrap();
    }
    let scoped_user = system_catalog_profile(kind, port, &plain);
    let scoped_system = system_catalog_profile(kind, port, "information_schema");
    let results = (
        pool.list_tables_with_system(&profile, false).await,
        pool.list_tables_with_system(&profile, true).await,
        pool.list_tables(&profile).await,
        pool.list_tables_with_system(&scoped_user, false).await,
        pool.list_tables_with_system(&scoped_user, true).await,
        pool.list_tables_with_system(&scoped_system, false).await,
        pool.list_tables_with_system(&scoped_system, true).await,
    );
    for db in &all {
        let _ = sqlx::query(&format!("DROP DATABASE `{db}`"))
            .execute(raw)
            .await;
    }
    let (hidden, shown, default, user_off, user_on, system_off, system_on) = results;
    let (hidden, shown, default) = (hidden.unwrap(), shown.unwrap(), default.unwrap());

    assert!(
        !has_schema(&hidden, "information_schema"),
        "{port}: false must hide information_schema"
    );
    assert!(
        has_schema(&shown, "information_schema"),
        "{port}: true must show information_schema"
    );
    assert!(
        hidden
            .objects
            .iter()
            .all(|o| o.schema != "information_schema")
    );
    assert_eq!(default.schemas, hidden.schemas);
    for system in ["mysql", "performance_schema", "sys"] {
        assert!(
            !has_schema(&hidden, system),
            "{port}: false must hide {system}"
        );
    }
    for catalog in [&hidden, &shown] {
        for db in &all {
            assert!(has_schema(catalog, db), "{port}: {db} must stay visible");
            assert!(
                catalog
                    .objects
                    .iter()
                    .any(|o| &o.schema == *db && o.name == "t")
            );
        }
    }
    // Bind order: (include_system, database, database).
    for catalog in [user_off.unwrap(), user_on.unwrap()] {
        assert_eq!(
            catalog.schemas,
            [plain.clone()],
            "{port}: scoped database only"
        );
        assert!(catalog.objects.iter().all(|o| o.schema == plain));
        assert_eq!(catalog.objects.len(), 1);
    }
    let system_off = system_off.unwrap();
    assert!(
        system_off.schemas.is_empty(),
        "{port}: false hides an explicitly scoped system schema"
    );
    assert!(system_off.objects.is_empty());
    let system_on = system_on.unwrap();
    // information_schema holds only SYSTEM VIEW tables, which the object query skips.
    assert_eq!(system_on.schemas, ["information_schema"]);
    assert!(system_on.objects.is_empty());
    pool_close(pool).await;
    pool_close(admin).await;
}

#[test]
#[ignore = "requires Compose MariaDB; creates and drops temporary databases"]
fn mariadb_system_schemas_follow_include_system_flag() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(mysql_family_system_check(
        DatabaseKind::MariaDb,
        53306,
        "DBDECK_MARIADB_PASSWORD",
        "DBDECK_MARIADB_ROOT_PASSWORD",
    ));
}

#[test]
#[ignore = "requires Compose MySQL; creates and drops temporary databases"]
fn mysql_system_schemas_follow_include_system_flag() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(mysql_family_system_check(
        DatabaseKind::MySql,
        53307,
        "DBDECK_MYSQL_PASSWORD",
        "DBDECK_MYSQL_ROOT_PASSWORD",
    ));
}

async fn relation_fixture_sql(pool: &DbPool, sql: &str) {
    match pool {
        DbPool::PostgreSql(pool) => {
            sqlx::query(sql).execute(pool).await.unwrap();
        }
        DbPool::MySql(pool) => {
            sqlx::query(sql).execute(pool).await.unwrap();
        }
    }
}

#[test]
#[ignore = "requires local fixtures; DBDECK_RELATIONS_ENGINE can select one engine"]
fn related_rows_follow_composite_foreign_keys() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        for (engine, kind, port, password) in [
            (
                "postgres",
                DatabaseKind::PostgreSql,
                55432,
                "DBDECK_PG_PASSWORD",
            ),
            (
                "mariadb",
                DatabaseKind::MariaDb,
                53306,
                "DBDECK_MARIADB_PASSWORD",
            ),
            ("mysql", DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD"),
        ] {
            if std::env::var("DBDECK_RELATIONS_ENGINE").is_ok_and(|selected| selected != engine) {
                continue;
            }
            let profile = ConnectionProfile {
                id: engine.into(),
                name: engine.into(),
                kind,
                host: "127.0.0.1".into(),
                port,
                database: "dbdeck_test".into(),
                username: "dbdeck_test".into(),
                environment: EnvironmentTag::Local,
                color: "#4f8fe8".into(),
                read_only: false,
                tls_mode: TlsMode::Disabled,
                password_saved: false,
                gopass_entry: None,
                tls_files: Default::default(),
                native_tools_dir: None,
            };
            let pool = match (engine, std::env::var_os("DBDECK_PG_TEST_SOCKET")) {
                ("postgres", Some(socket)) => DbPool::PostgreSql(
                    sqlx::postgres::PgPoolOptions::new()
                        .connect_with(
                            sqlx::postgres::PgConnectOptions::new()
                                .socket(socket)
                                .port(port)
                                .database(&profile.database)
                                .username(&profile.username),
                        )
                        .await
                        .unwrap(),
                ),
                _ => DbPool::connect(&profile, &std::env::var(password).expect(password))
                    .await
                    .unwrap(),
            };
            check_related_rows(&pool, engine).await;
            if engine == "postgres" {
                check_partition_related_rows(&pool).await;
            }
        }
    });
}

async fn check_related_rows(pool: &DbPool, engine: &str) {
    use database::{ForeignKeyLink, quote_mysql_identifier, quote_pg_identifier};
    let pg = engine == "postgres";
    let quote = if pg {
        quote_pg_identifier
    } else {
        quote_mysql_identifier
    };
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let child_schema = if pg { "public" } else { "dbdeck_test" };
    let parent_schema = if pg {
        format!("related-{suffix}")
    } else {
        child_schema.to_owned()
    };
    let parent = DbObject {
        schema: parent_schema.clone(),
        name: format!("parent-{suffix}"),
        kind: DbObjectKind::Table,
    };
    let child = DbObject {
        schema: child_schema.into(),
        name: format!("child-{suffix}"),
        kind: DbObjectKind::Table,
    };
    let parent_sql = format!("{}.{}", quote(&parent.schema), quote(&parent.name));
    let child_sql = format!("{}.{}", quote(&child.schema), quote(&child.name));
    let parent_column = quote("parent code");
    let constraint = quote(&format!("composite_{suffix}"));
    let engine_sql = if pg { "" } else { " ENGINE=InnoDB" };
    if pg {
        relation_fixture_sql(pool, &format!("CREATE SCHEMA {}", quote(&parent_schema))).await;
    }
    relation_fixture_sql(pool, &format!(
        "CREATE TABLE {parent_sql} (id INT PRIMARY KEY, tenant INT, {parent_column} VARCHAR(12), UNIQUE (tenant, {parent_column})){engine_sql}"
    )).await;
    relation_fixture_sql(pool, &format!(
        "CREATE TABLE {child_sql} (id INT PRIMARY KEY, fk_code VARCHAR(12), fk_tenant INT,
         CONSTRAINT {constraint} FOREIGN KEY (fk_tenant, fk_code) REFERENCES {parent_sql} (tenant, {parent_column})){engine_sql}"
    )).await;
    relation_fixture_sql(
        pool,
        &format!("INSERT INTO {parent_sql} VALUES (1, 10, '20'), (2, 20, '10')"),
    )
    .await;
    relation_fixture_sql(pool, &format!("INSERT INTO {child_sql} VALUES (100, '20', 10), (101, '20', 10), (102, '10', 20), (103, '20', NULL)")).await;

    let child_data = pool.read_table_view(&child, &[], None, 0).await.unwrap();
    let parent_data = pool.read_table_view(&parent, &[], None, 0).await.unwrap();
    let key = |data: &database::TableData, id: &str| {
        let row = data
            .rows
            .iter()
            .find(|row| matches!(&row[0], CellValue::Text(value) if value == id))
            .unwrap();
        data.cell_detail_key(row).unwrap()
    };
    let outbound: Vec<ForeignKeyLink> = pool.foreign_key_links(&child).await.unwrap();
    assert_eq!(outbound.len(), 1);
    let outbound = &outbound[0];
    assert!(outbound.outbound());
    assert_eq!(outbound.target(), &parent);
    assert_eq!(
        outbound.columns().collect::<Vec<_>>(),
        [("fk_tenant", "tenant"), ("fk_code", "parent code")]
    );
    let picked = pool
        .read_related_rows(&child, &key(&child_data, "100"), outbound)
        .await
        .unwrap();
    assert!(matches!(picked.rows.as_slice(), [row] if row[0] == CellValue::Text("1".into())));
    assert!(
        pool.read_related_rows(&child, &key(&child_data, "103"), outbound)
            .await
            .unwrap()
            .rows
            .is_empty()
    );

    let inbound = pool.foreign_key_links(&parent).await.unwrap();
    assert_eq!(inbound.len(), 1);
    let inbound = &inbound[0];
    assert!(!inbound.outbound());
    assert_eq!(inbound.target(), &child);
    let picked = pool
        .read_related_rows(&parent, &key(&parent_data, "1"), inbound)
        .await
        .unwrap();
    assert_eq!(picked.rows.len(), 2);
    assert!(
        picked
            .rows
            .iter()
            .all(|row| row[2] == CellValue::Text("10".into()))
    );

    let drop_fk = if pg {
        "DROP CONSTRAINT"
    } else {
        "DROP FOREIGN KEY"
    };
    relation_fixture_sql(
        pool,
        &format!("ALTER TABLE {child_sql} {drop_fk} {constraint}"),
    )
    .await;
    let error = pool
        .read_related_rows(&child, &key(&child_data, "100"), outbound)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("Внешний ключ изменился"),
        "{error}"
    );
    if pg {
        relation_fixture_sql(
            pool,
            &format!(
            "ALTER TABLE {child_sql} ADD CONSTRAINT {constraint} FOREIGN KEY (fk_tenant, fk_code)
             REFERENCES {parent_sql} (tenant, {parent_column})"
        ),
        )
        .await;
        assert!(
            pool.read_related_rows(&child, &key(&child_data, "100"), outbound)
                .await
                .is_err()
        );
    } else {
        relation_fixture_sql(
            pool,
            &format!(
                "ALTER TABLE {child_sql} DROP PRIMARY KEY, ADD UNIQUE (id),
             ADD CONSTRAINT {constraint} FOREIGN KEY (fk_tenant, fk_code)
             REFERENCES {parent_sql} (tenant, {parent_column})"
            ),
        )
        .await;
        let link = &pool.foreign_key_links(&child).await.unwrap()[0];
        let error = pool
            .read_related_rows(&child, &key(&child_data, "100"), link)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("первичный ключ")
                || error.to_string().contains("Первичный ключ"),
            "{error}"
        );
    }
    relation_fixture_sql(pool, &format!("DROP TABLE {child_sql}")).await;
    relation_fixture_sql(pool, &format!("DROP TABLE {parent_sql}")).await;
    if pg {
        relation_fixture_sql(pool, &format!("DROP SCHEMA {}", quote(&parent_schema))).await;
    }
}

async fn check_partition_related_rows(pool: &DbPool) {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let parent = format!("rel_parent_{suffix}");
    let parent_leaf = format!("{parent}_leaf");
    let child = format!("rel_child_{suffix}");
    let child_leaf = format!("{child}_leaf");
    relation_fixture_sql(pool, &format!(
        "CREATE TABLE public.{parent} (part INT, id REGCLASS, PRIMARY KEY (part, id)) PARTITION BY RANGE (part)"
    )).await;
    relation_fixture_sql(pool, &format!(
        "CREATE TABLE public.{parent_leaf} PARTITION OF public.{parent} FOR VALUES FROM (0) TO (10)"
    )).await;
    relation_fixture_sql(
        pool,
        &format!(
            "CREATE TABLE public.{child} (
           part INT, id REGCLASS, parent_id REGCLASS, PRIMARY KEY (part, id),
           FOREIGN KEY (part, parent_id) REFERENCES public.{parent} (part, id)
         ) PARTITION BY RANGE (part)"
        ),
    )
    .await;
    relation_fixture_sql(pool, &format!(
        "CREATE TABLE public.{child_leaf} PARTITION OF public.{child} FOR VALUES FROM (0) TO (10)"
    )).await;
    relation_fixture_sql(
        pool,
        &format!("INSERT INTO public.{parent} VALUES (1, 'public.{parent}'::regclass)"),
    )
    .await;
    relation_fixture_sql(pool, &format!(
        "INSERT INTO public.{child} VALUES (1, 'public.{child}'::regclass, 'public.{parent}'::regclass)"
    )).await;

    for (source, target, outbound, displayed_pk) in [
        (&child_leaf, &parent, true, &child),
        (&parent_leaf, &child, false, &parent),
    ] {
        let object = DbObject {
            schema: "public".into(),
            name: source.clone(),
            kind: DbObjectKind::Table,
        };
        let snapshot = pool.read_table_view(&object, &[], None, 0).await.unwrap();
        assert_eq!(snapshot.rows[0][1], CellValue::Text(displayed_pk.clone()));
        let key = snapshot.cell_detail_key(&snapshot.rows[0]).unwrap();
        let links = pool.foreign_key_links(&object).await.unwrap();
        assert_eq!(links.len(), 1);
        let link = &links[0];
        assert_eq!(link.target().name, *target);
        assert_eq!(link.outbound(), outbound);
        let rows = pool
            .read_related_rows(&object, &key, link)
            .await
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][1], CellValue::Text(target.clone()));
    }
    relation_fixture_sql(pool, &format!("DROP TABLE public.{child}")).await;
    relation_fixture_sql(pool, &format!("DROP TABLE public.{parent}")).await;
}
