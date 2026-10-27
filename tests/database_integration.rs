#![allow(dead_code)]

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
        let typed = pool
            .read_table(&DbObject {
                schema: expected_schema.to_owned(),
                name: "typed_values".to_owned(),
                kind: DbObjectKind::Table,
            })
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
    });
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
            for &(schema, name) in namespaces {
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
