//! These tests only run against private disposable servers from run-production-validation.sh.
use super::*;

#[test]
#[ignore = "only tests/run-tls-validation.sh: disposable servers and public rustls test certificates"]
fn isolated_tls_verification() {
    assert_eq!(std::env::var("DBDECK_TLS_VALIDATION").as_deref(), Ok("1"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
        "id":"tls-test", "name":"tls-test", "kind":std::env::var("DBDECK_TLS_KIND").unwrap(),
        "host":"testserver.com", "port":std::env::var("DBDECK_TLS_PORT").unwrap().parse::<u16>().unwrap(),
        "database":"dbdeck_validation", "username":std::env::var("DBDECK_TLS_USER").unwrap(),
        "environment":"prod", "color":"#ffffff", "read_only":true, "tls_mode":"verify_full"
    }))
    .unwrap();
    let root = PathBuf::from(std::env::var_os("DBDECK_TLS_FILES").unwrap());
    profile.tls_files.ca = Some(root.join("ca.crt"));
    profile.tls_files.client_cert = Some(root.join("client.crt"));
    profile.tls_files.client_key = Some(root.join("client.key"));
    let password = std::env::var("DBDECK_TLS_PASSWORD").unwrap();
    runtime.block_on(async {
        let pool = DbPool::connect(&profile, &password)
            .await
            .expect("verified mTLS connection");
        let session = sql_session::SqlSession::with_profile(pool.clone(), &profile);
        assert_eq!(
            session
                .execute("SELECT 42 AS answer")
                .await
                .unwrap()
                .data
                .rows[0][0],
            CellValue::Text("42".into())
        );
        session.close().await;
        match pool {
            DbPool::PostgreSql(pool) => pool.close().await,
            DbPool::MySql(pool) => pool.close().await,
        }
        let mut invalid = profile.clone();
        invalid.host = "sqlx.rs".into();
        assert!(
            DbPool::connect(&invalid, &password).await.is_err(),
            "wrong server identity accepted"
        );
        invalid = profile.clone();
        invalid.tls_files.ca = Some(root.join("wrong-ca.crt"));
        assert!(
            DbPool::connect(&invalid, &password).await.is_err(),
            "wrong CA accepted"
        );
        invalid = profile.clone();
        invalid.tls_files.client_cert = None;
        invalid.tls_files.client_key = None;
        assert!(
            DbPool::connect(&invalid, &password).await.is_err(),
            "missing client certificate accepted"
        );
    });
    println!(
        "Verified TLS/mTLS {:?}: SELECT succeeded, wrong hostname/CA and missing client certificate refused",
        profile.kind
    );
}

#[test]
#[ignore = "only tests/run-production-validation.sh: isolated tmpfs servers, no user DBs"]
fn isolated_backup_restore_fidelity() {
    assert_eq!(
        std::env::var("DBDECK_ISOLATED_VALIDATION").as_deref(),
        Ok("1")
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let kind: DatabaseKind = serde_json::from_value(serde_json::Value::String(
        std::env::var("DBDECK_ISOLATED_KIND").unwrap(),
    ))
    .unwrap();
    let mode = match std::env::var("DBDECK_ISOLATED_MODE").unwrap().as_str() {
        "schema" => native_backup::DumpMode::Schema,
        "full" => native_backup::DumpMode::Full,
        _ => panic!("unknown test mode"),
    };
    let secret = std::env::var("DBDECK_ISOLATED_PASSWORD").unwrap();
    let source_port = std::env::var("DBDECK_ISOLATED_SOURCE_PORT")
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let target_port = std::env::var("DBDECK_ISOLATED_TARGET_PORT")
        .unwrap()
        .parse::<u16>()
        .unwrap();
    assert_ne!(source_port, target_port);
    assert!(![55432, 53306, 53307].contains(&source_port));
    assert!(![55432, 53306, 53307].contains(&target_port));
    let mut source:ConnectionProfile=serde_json::from_value(serde_json::json!({"id":"isolated","name":"isolated","kind":kind,"host":"127.0.0.1","port":source_port,"database":"dbdeck_validation","username":std::env::var("DBDECK_ISOLATED_USER").unwrap(),"environment":"local","color":"#ffffff","read_only":false,"tls_mode":"disabled"})).unwrap();
    source.native_tools_dir = std::env::var_os("DBDECK_ISOLATED_TOOLS").map(PathBuf::from);
    let mut target = source.clone();
    target.port = target_port;
    let (source_pool, target_pool) = runtime.block_on(async {
        (
            DbPool::connect(&source, &secret).await.unwrap(),
            DbPool::connect(&target, &secret).await.unwrap(),
        )
    });
    let pg = matches!(kind, DatabaseKind::PostgreSql);
    let has_seed = runtime
        .block_on(source_pool.list_tables(&source))
        .unwrap()
        .objects
        .iter()
        .any(|o| o.name == "validation_parent");
    if !has_seed {
        let setup = if pg {
            "CREATE DOMAIN validation_amount AS NUMERIC(12,2) CHECK(VALUE>=0); CREATE TABLE validation_parent(id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,label TEXT NOT NULL,amount validation_amount DEFAULT 1.25,note TEXT); CREATE TABLE validation_child(id BIGINT PRIMARY KEY,parent_id BIGINT NOT NULL REFERENCES validation_parent(id)); CREATE INDEX validation_label_idx ON validation_parent(label); CREATE TABLE validation_audit(id BIGSERIAL PRIMARY KEY,label TEXT); CREATE FUNCTION validation_log() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN INSERT INTO validation_audit(label) VALUES(NEW.label); RETURN NEW; END$$; CREATE TRIGGER validation_insert AFTER INSERT ON validation_parent FOR EACH ROW EXECUTE FUNCTION validation_log(); CREATE VIEW validation_view AS SELECT id,label FROM validation_parent; CREATE ROLE dbdeck_validation_reader NOLOGIN; GRANT USAGE ON SCHEMA public TO dbdeck_validation_reader; GRANT SELECT ON validation_parent TO dbdeck_validation_reader; INSERT INTO validation_parent(label,note) VALUES('Привет, мир','кавычки и Unicode 🚀'),('second',NULL); INSERT INTO validation_child VALUES(1,1);"
        } else {
            "CREATE TABLE validation_parent(id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,label VARCHAR(255) NOT NULL,amount DECIMAL(12,2) DEFAULT 1.25,note TEXT) ENGINE=InnoDB; CREATE TABLE validation_child(id BIGINT PRIMARY KEY,parent_id BIGINT NOT NULL,FOREIGN KEY(parent_id) REFERENCES validation_parent(id)) ENGINE=InnoDB; CREATE INDEX validation_label_idx ON validation_parent(label); CREATE TABLE validation_audit(id BIGINT AUTO_INCREMENT PRIMARY KEY,label VARCHAR(255)) ENGINE=InnoDB; CREATE TRIGGER validation_insert AFTER INSERT ON validation_parent FOR EACH ROW INSERT INTO validation_audit(label) VALUES(NEW.label); CREATE VIEW validation_view AS SELECT id,label FROM validation_parent; CREATE PROCEDURE validation_read() SELECT label FROM validation_parent ORDER BY id; INSERT INTO validation_parent(label,note) VALUES('Привет, мир','кавычки и Unicode 🚀'),('second',NULL); INSERT INTO validation_child VALUES(1,1);"
        };
        runtime.block_on(source_pool.execute_sql(setup)).unwrap();
    }
    if pg {
        runtime
            .block_on(target_pool.execute_sql("CREATE ROLE dbdeck_validation_reader NOLOGIN"))
            .unwrap();
    }
    let path = std::env::temp_dir().join(format!(
        "dbdeck-fidelity-{}.{}",
        Uuid::new_v4(),
        mode.extension(kind)
    ));
    native_backup::dump(
        &source,
        &secret,
        mode,
        matches!(kind, DatabaseKind::MariaDb),
        &path,
        &transfer::Cancellation::new(),
        |_| {},
    )
    .unwrap();
    if !pg {
        let mut wrong = target.clone();
        wrong.database = "another_database".into();
        assert!(
            native_backup::restore(&wrong, &secret, &path, &transfer::Cancellation::new())
                .unwrap_err()
                .contains("другой БД")
        );
    }
    let result = native_backup::restore(&target, &secret, &path, &transfer::Cancellation::new());
    assert!(
        result.is_ok(),
        "{kind:?}/{mode:?}: {result:?}; dump: {}",
        path.display()
    );
    fs::remove_file(path).unwrap();
    let structure = runtime.block_on(target_pool.list_tables(&target)).unwrap();
    for name in [
        "validation_parent",
        "validation_child",
        "validation_view",
        "validation_audit",
    ] {
        assert!(
            structure.objects.iter().any(|o| o.name == name),
            "missing {name}"
        );
    }
    let data = runtime
        .block_on(target_pool.execute_sql("SELECT label,note FROM validation_parent ORDER BY id"))
        .unwrap();
    if matches!(mode, native_backup::DumpMode::Schema) {
        assert!(data.data.rows.is_empty());
    } else {
        assert_eq!(
            data.data.rows,
            [
                vec![
                    CellValue::Text("Привет, мир".into()),
                    CellValue::Text("кавычки и Unicode 🚀".into())
                ],
                vec![CellValue::Text("second".into()), CellValue::Null]
            ]
        );
        let view = runtime
            .block_on(target_pool.execute_sql("SELECT label FROM validation_view ORDER BY id"))
            .unwrap();
        assert_eq!(view.data.rows.len(), 2);
        let trigger = runtime
            .block_on(target_pool.execute_sql("SELECT label FROM validation_audit ORDER BY id"))
            .unwrap();
        assert_eq!(trigger.data.rows.len(), 2);
    }
    // Schema-only must preserve the same definitions and privileges as a full dump.
    for name in ["validation_parent", "validation_child"] {
        let object = DbObject {
            schema: if pg {
                "public".into()
            } else {
                "dbdeck_validation".into()
            },
            name: name.into(),
            kind: database::DbObjectKind::Table,
        };
        let before = runtime
            .block_on(source_pool.inspect_object(&object))
            .unwrap();
        let after = runtime
            .block_on(target_pool.inspect_object(&object))
            .unwrap();
        assert_eq!(
            before.columns.rows, after.columns.rows,
            "{name}: column definition mismatch"
        );
        assert_eq!(
            before.constraints.rows, after.constraints.rows,
            "{name}: constraint definition mismatch"
        );
        assert_eq!(
            before.indexes.rows, after.indexes.rows,
            "{name}: index definition mismatch"
        );
    }
    if !pg {
        assert_eq!(
            runtime
                .block_on(target_pool.execute_sql("CALL validation_read()"))
                .unwrap()
                .data
                .rows
                .len(),
            data.data.rows.len()
        );
    } else {
        let query = "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE contypid='validation_amount'::regtype ORDER BY conname";
        let before = runtime.block_on(source_pool.execute_sql(query)).unwrap();
        let after = runtime.block_on(target_pool.execute_sql(query)).unwrap();
        assert!(!before.data.rows.is_empty());
        assert_eq!(
            before.data.rows, after.data.rows,
            "domain constraint mismatch"
        );
        let viewer = sql_session::SqlSession::new(target_pool.clone());
        runtime
            .block_on(viewer.execute("SET ROLE dbdeck_validation_reader"))
            .unwrap();
        assert_eq!(
            runtime
                .block_on(viewer.execute("SELECT label FROM validation_parent ORDER BY id"))
                .unwrap()
                .data
                .rows
                .len(),
            data.data.rows.len()
        );
        assert!(
            runtime
                .block_on(viewer.execute("UPDATE validation_parent SET label='forbidden'"))
                .is_err()
        );
        runtime.block_on(viewer.close());
    }
    // Test constraints and triggers inside a transaction, then roll back even on the successful path.
    runtime.block_on(async {
        let session = sql_session::SqlSession::new(target_pool.clone());
        session
            .transaction(sql_session::TransactionAction::Manual)
            .await
            .unwrap();
        session
            .execute("INSERT INTO validation_parent(label) VALUES('trigger-check')")
            .await
            .unwrap();
        let audit = session
            .execute("SELECT label FROM validation_audit WHERE label='trigger-check'")
            .await
            .unwrap();
        assert_eq!(audit.data.rows.len(), 1);
        assert!(
            session
                .execute("INSERT INTO validation_child(id,parent_id) VALUES(99999,-99999)")
                .await
                .is_err()
        );
        session
            .transaction(sql_session::TransactionAction::Rollback)
            .await
            .unwrap();
        session.close().await;
    });
    if matches!(mode, native_backup::DumpMode::Full) {
        if pg {
            runtime.block_on(target_pool.execute_sql("CREATE FUNCTION validation_mutation() RETURNS integer LANGUAGE plpgsql AS $$BEGIN INSERT INTO validation_parent(label) VALUES('must-not-write'); RETURN 1; END$$")).unwrap();
            runtime
                .block_on(target_pool.execute_sql(
                    "CREATE VIEW validation_mutating_view AS SELECT validation_mutation() AS value",
                ))
                .unwrap();
            let view = DbObject {
                schema: "public".into(),
                name: "validation_mutating_view".into(),
                kind: database::DbObjectKind::View,
            };
            assert!(
                runtime.block_on(target_pool.read_table(&view)).is_err(),
                "browse view allowed a hidden write"
            );
            let mut read_profile = target.clone();
            read_profile.read_only = true;
            let session = sql_session::SqlSession::with_profile(target_pool.clone(), &read_profile);
            assert!(
                runtime
                    .block_on(session.execute("SELECT validation_mutation()"))
                    .is_err()
            );
            runtime.block_on(session.close());
            assert!(
                runtime
                    .block_on(target_pool.execute_sql(
                        "SELECT id FROM validation_parent WHERE label='must-not-write'"
                    ))
                    .unwrap()
                    .data
                    .rows
                    .is_empty()
            );
        }
        runtime.block_on(async {
            let session = sql_session::SqlSession::new(target_pool.clone());
            let pid_sql = if pg {
                "SELECT pg_backend_pid()"
            } else {
                "SELECT CONNECTION_ID()"
            };
            let pid = session.execute(pid_sql).await.unwrap().data.rows[0][0].clone();
            let CellValue::Text(pid) = pid else {
                panic!("backend id")
            };
            let pid = pid.parse::<u64>().unwrap();
            session
                .transaction(sql_session::TransactionAction::Manual)
                .await
                .unwrap();
            session
                .execute("INSERT INTO validation_parent(label) VALUES('network-loss-probe')")
                .await
                .unwrap();
            let kill = if pg {
                format!("SELECT pg_terminate_backend({pid})")
            } else {
                format!("KILL CONNECTION {pid}")
            };
            target_pool.execute_sql(&kill).await.unwrap();
            assert!(session.execute("SELECT 1").await.is_err());
            session.close().await;
            assert!(
                target_pool
                    .execute_sql(
                        "SELECT id FROM validation_parent WHERE label='network-loss-probe'"
                    )
                    .await
                    .unwrap()
                    .data
                    .rows
                    .is_empty()
            );
        });
        use std::io::Write;
        let rows = if pg { 1_000_000u64 } else { 100_000 };
        let csv =
            std::env::temp_dir().join(format!("dbdeck-production-large-{}.csv", Uuid::new_v4()));
        {
            let mut writer = std::io::BufWriter::new(fs::File::create(&csv).unwrap());
            writeln!(writer, "id,payload").unwrap();
            for i in 1..=rows {
                writeln!(writer, "{i},строка-{i}").unwrap();
            }
            writer.flush().unwrap();
        }
        let ddl = if pg {
            "CREATE TABLE validation_csv_large(id BIGINT PRIMARY KEY,payload TEXT NOT NULL)"
        } else {
            "CREATE TABLE validation_csv_large(id BIGINT PRIMARY KEY,payload TEXT NOT NULL) ENGINE=InnoDB"
        };
        runtime.block_on(target_pool.execute_sql(ddl)).unwrap();
        let object = DbObject {
            schema: if pg { "public" } else { &target.database }.into(),
            name: "validation_csv_large".into(),
            kind: database::DbObjectKind::Table,
        };
        let options = csv_input::Options::default();
        let mapping = [Some("id".into()), Some("payload".into())];
        let cancelled = transfer::Cancellation::new();
        let cancel_result = runtime.block_on(target_pool.import_csv(
            &target,
            &object,
            &csv,
            &options,
            &mapping,
            &cancelled,
            |n| {
                if n >= 10_000 {
                    cancelled.request();
                }
            },
        ));
        assert!(cancel_result.is_err());
        assert_eq!(
            runtime
                .block_on(target_pool.execute_sql("SELECT COUNT(*) FROM validation_csv_large"))
                .unwrap()
                .data
                .rows[0][0],
            CellValue::Text("0".into())
        );
        let start = std::time::Instant::now();
        let result = runtime.block_on(target_pool.import_csv(
            &target,
            &object,
            &csv,
            &options,
            &mapping,
            &transfer::Cancellation::new(),
            |_| {},
        ));
        fs::remove_file(csv).unwrap();
        assert_eq!(result.unwrap(), rows);
        assert_eq!(
            runtime
                .block_on(target_pool.execute_sql("SELECT COUNT(*) FROM validation_csv_large"))
                .unwrap()
                .data
                .rows[0][0],
            CellValue::Text(rows.to_string())
        );
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        let rss = unsafe {
            libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
            usage.assume_init().ru_maxrss
        };
        println!(
            "CSV {kind:?}: committed {rows} rows in {:.2}s, client peak RSS {:.1} MiB; cancellation left 0 rows",
            start.elapsed().as_secs_f64(),
            rss as f64 / 1024.0
        );
    }
    println!(
        "Native restore fidelity {kind:?}/{mode:?}: data, schema, FK, index, view, trigger and engine-specific objects passed"
    );
}

#[test]
#[ignore = "requires headless GTK and local PostgreSQL; TEMP tables only"]
fn production_confirmation_snapshot_and_readonly_ui() {
    use grid_app_tests::until;
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let mut profile:ConnectionProfile=serde_json::from_value(serde_json::json!({"id":"production-ui","name":"production-ui","kind":"postgre_sql","host":"127.0.0.1","port":55432,"database":"dbdeck_test","username":"dbdeck_test","environment":"local","color":"#ffffff","read_only":false,"tls_mode":"disabled"})).unwrap();
    let pool = runtime
        .block_on(DbPool::connect(
            &profile,
            &std::env::var("DBDECK_PG_PASSWORD").unwrap(),
        ))
        .unwrap();
    profile.environment = EnvironmentTag::Prod;
    profile.tls_files.allow_unverified_prod = true;
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.production-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    let key = profile_session_key(&profile);
    ui.sessions
        .borrow_mut()
        .insert(key.clone(), (profile.clone(), pool.clone()));
    *ui.current_profile.borrow_mut() = Some(profile.clone());
    *ui.pool.borrow_mut() = Some(pool.clone());
    ui.landing.set_visible(false);
    create_sql_tab(&ui, &key);
    let id = ui.sql_tabs.borrow().keys().next().unwrap().clone();
    let session = ui.sql_tabs.borrow()[&id].session.clone();
    runtime.block_on(session.execute_authorized("CREATE TEMP TABLE production_ui_temp(id INT PRIMARY KEY,value TEXT);INSERT INTO production_ui_temp VALUES(1,'original')",5,true)).unwrap();
    fn named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if widget.widget_name() == name {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            child = w.next_sibling();
            if let Some(found) = named(&w, name) {
                return Some(found);
            }
        }
        None
    }
    fn confirmation() -> (gtk::Window, gtk::Entry, gtk::Button) {
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.is_visible() && w.widget_name() == "production-confirmation")
            .unwrap();
        let entry = named(window.upcast_ref(), "production-target-name")
            .unwrap()
            .downcast::<gtk::Entry>()
            .unwrap();
        let accept = named(window.upcast_ref(), "production-accept")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        (window, entry, accept)
    }
    ui.sql_tabs.borrow()[&id]
        .buffer
        .set_text("UPDATE production_ui_temp SET value='approved'");
    run_sql_tab(&ui, &id);
    let (window, entry, accept) = confirmation();
    assert!(!accept.is_sensitive());
    accept.emit_clicked();
    assert!(window.is_visible());
    entry.set_text("wrong-db");
    assert!(!accept.is_sensitive());
    entry.set_text("dbdeck_test");
    assert!(accept.is_sensitive());
    ui.sql_tabs.borrow()[&id]
        .buffer
        .set_text("UPDATE production_ui_temp SET value='changed-after-confirmation'");
    accept.emit_clicked();
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert_eq!(
        runtime
            .block_on(session.execute("SELECT value FROM production_ui_temp"))
            .unwrap()
            .data
            .rows[0][0],
        CellValue::Text("approved".into())
    );
    ui.sql_tabs.borrow()[&id]
        .buffer
        .set_text("SELECT id,value FROM production_ui_temp");
    run_sql_tab(&ui, &id);
    let table_key = format!("sql::{id}");
    until(|| {
        ui.table_data
            .borrow()
            .get(&table_key)
            .is_some_and(|d| d.rows.len() == 1)
    });
    let state = ui.table_edit_states.borrow()[&table_key].clone();
    state
        .pending
        .borrow_mut()
        .insert((0, 1), Some("grid-approved".into()));
    save_table_edits(&ui, &table_key);
    let (_, entry, accept) = confirmation();
    entry.set_text("dbdeck_test");
    accept.emit_clicked();
    until(|| !state.saving.get());
    assert!(
        state.pending.borrow().is_empty(),
        "save failed: {}",
        state.status.text()
    );
    assert_eq!(
        runtime
            .block_on(session.execute("SELECT value FROM production_ui_temp"))
            .unwrap()
            .data
            .rows[0][0],
        CellValue::Text("grid-approved".into())
    );
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    ui.sql_tabs.borrow()[&id]
        .buffer
        .set_text("DELETE FROM production_ui_temp");
    run_sql_tab(&ui, &id);
    let (window, entry, accept) = confirmation();
    entry.set_text("dbdeck_test");
    window.close();
    accept.emit_clicked();
    assert_eq!(
        runtime
            .block_on(session.execute("SELECT id FROM production_ui_temp"))
            .unwrap()
            .data
            .rows
            .len(),
        1
    );
    let mut readonly = profile.clone();
    readonly.id = "read-only-ui".into();
    readonly.read_only = true;
    let ro_key = profile_session_key(&readonly);
    ui.sessions
        .borrow_mut()
        .insert(ro_key.clone(), (readonly.clone(), pool));
    *ui.current_profile.borrow_mut() = Some(readonly);
    create_sql_tab(&ui, &ro_key);
    let ro_id = ui
        .sql_tabs
        .borrow()
        .iter()
        .find(|(_, t)| t.session_key == ro_key)
        .unwrap()
        .0
        .clone();
    ui.sql_tabs.borrow()[&ro_id]
        .buffer
        .set_text("UPDATE production_ui_temp SET value='forbidden'");
    run_sql_tab(&ui, &ro_id);
    until(|| ui.sql_tabs.borrow()[&ro_id].run_button.is_sensitive());
    assert!(
        ui.sql_tabs.borrow()[&ro_id]
            .error_message
            .text()
            .contains("Только чтение")
    );
    reset_sql_session(&ui, &ro_id);
    let reset = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::MessageDialog>().ok())
        .find(|w| w.is_visible())
        .unwrap();
    reset.response(gtk::ResponseType::Yes);
    assert!(
        runtime
            .block_on(ui.sql_tabs.borrow()[&ro_id].session.execute_authorized(
                "DELETE FROM production_ui_temp",
                5,
                true
            ))
            .unwrap_err()
            .contains("Только чтение")
    );
    ui.window.close();
}
