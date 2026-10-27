use super::*;
use std::io::BufRead;

pub(super) fn select_target_database(name: &str) {
    use grid_app_tests::until;
    fn find(widget: &gtk::Widget) -> Option<gtk::ListBox> {
        if let Ok(found) = widget.clone().downcast::<gtk::ListBox>() {
            return Some(found);
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Some(found) = find(&widget) {
                return Some(found);
            }
        }
        None
    }
    let window = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.widget_name() == "database-target-picker" && w.is_visible())
        .unwrap();
    let list = find(window.upcast_ref()).unwrap();
    until(|| list.first_child().is_some());
    assert!(
        list.selected_row().is_none(),
        "target must be chosen explicitly"
    );
    let mut child = list.first_child();
    let mut selected = None;
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.widget_name() == name {
            selected = widget.downcast::<gtk::ListBoxRow>().ok();
            break;
        }
    }
    let row = selected.expect("requested database must be listed");
    list.select_row(Some(&row));
    list.emit_by_name::<()>("row-activated", &[&row]);
    until(|| !window.is_visible());
}

#[test]
#[ignore = "requires Compose PostgreSQL; TEMP views only, million-row streaming file in /tmp"]
fn million_row_csv_streams_and_cancel_preserves_file() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let root = std::env::temp_dir().join(format!("dbdeck-million-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let destination = root.join("million.csv");
    use std::os::unix::fs::PermissionsExt;
    let started = std::time::Instant::now();
    runtime.block_on(async {
        for (cancel_export, existing) in [(false,false), (true,true), (true,false)] {
            let destination = if cancel_export && !existing { root.join("cancelled-new.csv") } else { destination.clone() };
            let options=sqlx::postgres::PgConnectOptions::new().host("127.0.0.1").port(55432).database("dbdeck_test").username("dbdeck_test")
                .password(&std::env::var("DBDECK_PG_PASSWORD").expect("test password from .env"));
            let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).idle_timeout(None).max_lifetime(None).connect_with(options).await.unwrap();
            sqlx::query("CREATE TEMP VIEW dbdeck_million_export AS SELECT n, repeat('x',32) AS payload FROM generate_series(1,1000000) n").execute(&pool).await.unwrap();
            let schema:String=sqlx::query_scalar("SELECT nspname FROM pg_namespace WHERE oid=pg_my_temp_schema()").fetch_one(&pool).await.unwrap();
            let object=DbObject {schema,name:"dbdeck_million_export".into(),kind:database::DbObjectKind::View};
            let cancelled=transfer::Cancellation::new();let mut last=0;
            if cancel_export && existing {fs::write(&destination,"previous").unwrap();}
            let result=write_full_table_csv(&destination,&DbPool::PostgreSql(pool.clone()),&object,&cancelled,|count| {
                last=count;if cancel_export && count>=10000 {cancelled.request();}
            }).await;
            if cancel_export {
                assert!(result.unwrap_err().contains("отменён"));
                if existing {assert_eq!(fs::read_to_string(&destination).unwrap(),"previous");} else {assert!(!destination.exists());}
            } else {
                assert_eq!(result.unwrap(),1000000);assert_eq!(last,1000000);
                assert_eq!(fs::metadata(&destination).unwrap().permissions().mode()&0o777,0o600);
                let mut rows=std::io::BufReader::new(fs::File::open(&destination).unwrap()).lines();
                assert_eq!(rows.next().unwrap().unwrap(),"\"n\",\"payload\"");assert_eq!(rows.count(),1000000);
                println!("Million-row CSV: {} bytes, {:.2}s; one row + 1MiB write buffer",fs::metadata(&destination).unwrap().len(),started.elapsed().as_secs_f64());
            }
            assert_eq!(fs::read_dir(&root).unwrap().count(),1,"temporary outputs must be cleaned");
            pool.close().await;
        }
    });
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires Compose databases/native dump tools; read-only backups, no restore"]
fn native_schema_and_full_backups_postgres_mariadb_mysql() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let root = std::env::temp_dir().join(format!("dbdeck-dumps-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    for (kind, port, key, maria) in [
        (DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD", false),
        (
            DatabaseKind::MariaDb,
            53306,
            "DBDECK_MARIADB_PASSWORD",
            true,
        ),
        (DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD", false),
    ] {
        let profile = ConnectionProfile {
            id: "dump-test".into(),
            name: "test".into(),
            kind,
            host: "127.0.0.1".into(),
            port,
            database: "dbdeck_test".into(),
            username: "dbdeck_test".into(),
            environment: EnvironmentTag::Local,
            color: "#ffffff".into(),
            read_only: true,
            tls_mode: TlsMode::Disabled,
            password_saved: false,
            gopass_entry: None,
            tls_files: Default::default(),
            native_tools_dir: None,
        };
        let password = std::env::var(key).unwrap();
        let _ = runtime
            .block_on(DbPool::connect(&profile, &password))
            .unwrap();
        for mode in [
            native_backup::DumpMode::Schema,
            native_backup::DumpMode::Full,
        ] {
            let path = root.join(format!(
                "{port}-{}.{}",
                if matches!(mode, native_backup::DumpMode::Schema) {
                    "schema"
                } else {
                    "full"
                },
                mode.extension(kind)
            ));
            native_backup::dump(
                &profile,
                &password,
                mode,
                maria,
                &path,
                &transfer::Cancellation::new(),
                |_| {},
            )
            .unwrap();
            let data = fs::read(&path).unwrap();
            assert!(data.len() > 100);
            if matches!(
                (mode, kind),
                (native_backup::DumpMode::Full, DatabaseKind::PostgreSql)
            ) {
                assert!(data.starts_with(b"PGDMP"));
                let contents = std::process::Command::new("pg_restore")
                    .arg("--list")
                    .arg(&path)
                    .output()
                    .unwrap();
                assert!(contents.status.success());
                assert!(
                    String::from_utf8_lossy(&contents.stdout)
                        .contains("TABLE DATA public sample_records")
                );
            } else {
                let text = String::from_utf8(data).unwrap();
                assert!(text.contains("CREATE TABLE"));
                if matches!(mode, native_backup::DumpMode::Schema) {
                    assert!(!text.contains("INSERT INTO"));
                    assert!(!text.contains("COPY public.sample_records"));
                } else {
                    assert!(text.contains("INSERT INTO"));
                }
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn snapshot_csv_cancellation_does_not_publish_and_output_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("dbdeck-snapshot-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let path = root.join("snapshot.csv");
    let data = TableData {
        columns: vec!["value".into()],
        rows: vec![vec![CellValue::Text("x".repeat(128))]; 2048],
        primary_key: vec![],
        truncated: false,
        truncated_columns: false,
    };
    write_table_csv(&path, &data).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for exists in [true, false] {
        if exists {
            fs::write(&path, "previous").unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        let cancel = transfer::Cancellation::new();
        let result = write_snapshot_csv(&path, &data, &cancel, |_| {
            cancel.request();
        });
        assert!(result.unwrap_err().contains("отменён"));
        if exists {
            assert_eq!(fs::read_to_string(&path).unwrap(), "previous");
        } else {
            assert!(!path.exists());
        }
        assert_eq!(fs::read_dir(&root).unwrap().count(), usize::from(exists));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires Compose PostgreSQL; synthetic SELECT only, verifies bounded result memory"]
fn wide_sql_preview_is_memory_bounded() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let options = sqlx::postgres::PgConnectOptions::new()
            .host("127.0.0.1")
            .port(55432)
            .database("dbdeck_test")
            .username("dbdeck_test")
            .password(&std::env::var("DBDECK_PG_PASSWORD").unwrap());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_with(options)
            .await
            .unwrap();
        let columns = (0..128)
            .map(|i| format!("repeat('x',1024) AS c{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let data = DbPool::PostgreSql(pool.clone())
            .execute_sql(&format!("SELECT {columns} FROM generate_series(1,400)"))
            .await
            .unwrap();
        assert_eq!(data.row_count, 400);
        assert!(data.data.truncated);
        assert!(data.data.rows.len() < 400);
        let bytes: usize = data
            .data
            .rows
            .iter()
            .flatten()
            .map(|c| match c {
                CellValue::Text(s) | CellValue::Truncated(s) => s.len(),
                _ => 0,
            })
            .sum();
        assert!(bytes <= database::MAX_PREVIEW_BYTES);
        println!(
            "Wide SQL preview: {} / 400 rows, {} bytes",
            data.data.rows.len(),
            bytes
        );
        // The collector's debug assertion also checks that the previous set
        // is released before allocating this set's preview.
        let multiple = DbPool::PostgreSql(pool.clone())
            .execute_sql(&format!(
                "SELECT {columns} FROM generate_series(1,400); SELECT {columns} FROM generate_series(1,300)"
            ))
            .await
            .unwrap();
        assert_eq!(multiple.row_count, 300);
        assert!(multiple.data.truncated);
        assert_eq!(multiple.data.rows.len(), data.data.rows.len());
        pool.close().await;
    });
}

#[test]
#[ignore = "requires isolated display and Compose PostgreSQL; CSV TEMP view, schema dump read-only"]
fn sql_save_csv_and_backup_ui_handlers() {
    use grid_app_tests::until;
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let password = std::env::var("DBDECK_PG_PASSWORD").unwrap();
    let profile = ConnectionProfile {
        id: "export-ui-test".into(),
        name: "export UI".into(),
        kind: DatabaseKind::PostgreSql,
        host: "127.0.0.1".into(),
        port: 55432,
        database: "dbdeck_test".into(),
        username: "dbdeck_test".into(),
        environment: EnvironmentTag::Local,
        color: "#ffffff".into(),
        read_only: true,
        tls_mode: TlsMode::Disabled,
        password_saved: true,
        gopass_entry: None,
        tls_files: Default::default(),
        native_tools_dir: None,
    };
    let pool = runtime
        .block_on(DbPool::connect(&profile, &password))
        .unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.export-ui-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    {
        let mut store = ui.store.borrow_mut();
        let store = store.as_mut().unwrap();
        store
            .unlock_or_create_vault("synthetic export key")
            .unwrap();
        store.save_profile(profile.clone(), &password).unwrap();
    }
    let session = profile_session_key(&profile);
    ui.sessions
        .borrow_mut()
        .insert(session.clone(), (profile.clone(), pool.clone()));
    *ui.current_profile.borrow_mut() = Some(profile.clone());
    create_sql_tab(&ui, &session);
    let id = ui.sql_tabs.borrow().keys().next().unwrap().clone();
    ui.sql_tabs.borrow()[&id]
        .buffer
        .set_text("SELECT 1 AS saved_query");
    assert!(ui.sql_queries_directory.borrow().is_none());
    let root = std::env::temp_dir().join(format!("dbdeck-export-ui-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fn accept(title: &str, folder: &std::path::Path, name: &str) {
        use grid_app_tests::until;
        let chooser = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::FileChooserDialog>().ok())
            .find(|w| w.title().as_deref() == Some(title) && w.is_visible())
            .unwrap();
        println!("UI chooser: {title} -> {name}");
        until(|| chooser.is_mapped());
        chooser
            .set_current_folder(Some(&gtk::gio::File::for_path(folder)))
            .unwrap();
        until(|| chooser.current_folder().and_then(|f| f.path()).as_deref() == Some(folder));
        chooser.set_current_name(name);
        until(|| {
            chooser.file().and_then(|f| f.path()).as_deref() == Some(folder.join(name).as_path())
        });
        chooser.response(gtk::ResponseType::Accept);
        fn replace_button(widget: &gtk::Widget) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button
                    .label()
                    .is_some_and(|label| label.contains("Replace") || label.contains("Заменить"))
                {
                    return Some(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                if let Some(button) = replace_button(&widget) {
                    return Some(button);
                }
            }
            None
        }
        until(|| {
            if !chooser.is_visible() {
                return true;
            }
            for window in gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Window>().ok())
            {
                if window.is_visible()
                    && window.transient_for().as_ref() == Some(chooser.upcast_ref())
                {
                    if let Some(button) = replace_button(window.upcast_ref()) {
                        button.emit_clicked();
                    }
                }
            }
            false
        });
    }
    save_sql_tab(&ui, &id);
    accept("Сохранить SQL-запрос", &root, "saved.sql");
    assert_eq!(
        fs::read_to_string(root.join("saved.sql")).unwrap(),
        "SELECT 1 AS saved_query"
    );
    ui.sql_tabs.borrow_mut().get_mut(&id).unwrap().result = Some(Arc::new(TableData {
        columns: vec!["value".into()],
        rows: vec![vec![CellValue::Text("shown".into())]],
        primary_key: vec![],
        truncated: false,
        truncated_columns: false,
    }));
    choose_sql_csv_export(&ui, &id);
    accept("Экспорт результата SQL в CSV", &root, "shown.csv");
    assert!(ui.transfer_cancel_button.is_visible());
    until(|| ui.transfer_cancel.borrow().is_none());
    assert!(root.join("shown.csv").exists());
    assert!(!ui.transfer_cancel_button.is_visible());
    assert_eq!(
        ui.store.borrow().as_ref().unwrap().csv_export_directory(),
        Some(root.clone())
    );
    let DbPool::PostgreSql(pg) = &pool else {
        unreachable!()
    };
    // A separate one-connection pool keeps the TEMP view on the export's connection.
    let options = sqlx::postgres::PgConnectOptions::new()
        .host("127.0.0.1")
        .port(55432)
        .database("dbdeck_test")
        .username("dbdeck_test")
        .password(&password);
    let temp = runtime
        .block_on(
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
                .connect_with(options),
        )
        .unwrap();
    runtime
        .block_on(
            sqlx::query(
                "CREATE TEMP VIEW dbdeck_ui_export AS SELECT n FROM generate_series(1,25000) n",
            )
            .execute(&temp),
        )
        .unwrap();
    let schema: String = runtime
        .block_on(
            sqlx::query_scalar("SELECT nspname FROM pg_namespace WHERE oid=pg_my_temp_schema()")
                .fetch_one(&temp),
        )
        .unwrap();
    let temp_object = DbObject {
        schema,
        name: "dbdeck_ui_export".into(),
        kind: database::DbObjectKind::View,
    };
    ui.sessions.borrow_mut().insert(
        session.clone(),
        (profile.clone(), DbPool::PostgreSql(temp.clone())),
    );
    let saw_progress = Rc::new(Cell::new(false));
    let observed = saw_progress.clone();
    let signal = ui.status.connect_label_notify(move |label| {
        if label.text().contains("Экспорт CSV:") {
            observed.set(true);
        }
    });
    choose_full_table_csv_export(&ui, &profile, &temp_object, "ui-export");
    accept("Экспорт всей таблицы в CSV", &root, "full.csv");
    until(|| ui.transfer_cancel.borrow().is_none());
    assert!(root.join("full.csv").exists(), "{}", ui.status.text());
    assert!(saw_progress.get());
    assert!(!ui.transfer_cancel_button.is_visible());
    ui.status.disconnect(signal);
    ui.sessions
        .borrow_mut()
        .insert(session.clone(), (profile.clone(), pool.clone()));
    runtime.block_on(temp.close());
    backup_ui::choose(&ui, &profile, native_backup::DumpMode::Schema);
    select_target_database("dbdeck_test");
    accept("Структура БД без данных", &root, "schema.sql");
    until(|| ui.transfer_cancel.borrow().is_none());
    assert!(root.join("schema.sql").exists(), "{}", ui.status.text());
    assert!(!ui.transfer_cancel_button.is_visible());
    let existing = root.join("cancelled.sql");
    fs::write(&existing, "previous").unwrap();
    backup_ui::choose(&ui, &profile, native_backup::DumpMode::Schema);
    select_target_database("dbdeck_test");
    accept("Структура БД без данных", &root, "cancelled.sql");
    ui.transfer_cancel_button.emit_clicked();
    until(|| ui.transfer_cancel.borrow().is_none());
    assert_eq!(fs::read_to_string(&existing).unwrap(), "previous");
    assert!(!ui.transfer_cancel_button.is_visible());
    // Import UI: explicit target and file, then Cancel must leave the DB untouched.
    let mut writable = profile.clone();
    writable.read_only = false;
    let import_path = root.join("import.sql");
    fs::write(
        &import_path,
        "DO $$ BEGIN IF current_database() <> 'dbdeck_test' THEN RAISE EXCEPTION 'wrong database'; END IF; END $$; CREATE TEMP TABLE ui_import(id integer); INSERT INTO ui_import VALUES(1);",
    )
    .unwrap();
    for response in [gtk::ResponseType::Cancel, gtk::ResponseType::Accept] {
        backup_ui::choose_import(&ui, &writable);
        select_target_database("dbdeck_test");
        let chooser = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::FileChooserDialog>().ok())
            .find(|w| w.is_visible() && w.title().as_deref() == Some("Импорт в dbdeck_test"))
            .unwrap();
        chooser
            .set_file(&gtk::gio::File::for_path(&import_path))
            .unwrap();
        until(|| chooser.file().and_then(|f| f.path()).as_ref() == Some(&import_path));
        chooser.response(gtk::ResponseType::Accept);
        let confirmation = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::MessageDialog>().ok())
            .find(|w| w.is_visible() && w.text().as_deref() == Some("Импорт в БД «dbdeck_test»"))
            .unwrap();
        assert!(
            confirmation
                .secondary_text()
                .unwrap()
                .contains("127.0.0.1:55432")
        );
        confirmation.response(response);
        if response == gtk::ResponseType::Accept {
            until(|| ui.transfer_cancel.borrow().is_none());
            assert!(
                ui.status.text().contains("Импорт завершён"),
                "{}",
                ui.status.text()
            );
        } else {
            assert!(ui.transfer_cancel.borrow().is_none());
        }
    }
    // CSV wizard: preview/mapping, invalidation, target confirmation and Cancel.
    fn widget_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if widget.widget_name() == name {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Some(found) = widget_named(&widget, name) {
                return Some(found);
            }
        }
        None
    }
    let csv_path = root.join("import.csv");
    fs::write(
        &csv_path,
        "id,name,note\n-123456789,Тест,\"строка, с запятой\"\n",
    )
    .unwrap();
    csv_import_ui::choose(
        &ui,
        &writable,
        Some(DbObject {
            schema: "public".into(),
            name: "sample_records".into(),
            kind: database::DbObjectKind::Table,
        }),
    );
    let chooser = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::FileChooserDialog>().ok())
        .find(|w| w.is_visible() && w.title().as_deref() == Some("Импорт CSV · dbdeck_test"))
        .unwrap();
    chooser
        .set_file(&gtk::gio::File::for_path(&csv_path))
        .unwrap();
    until(|| chooser.file().and_then(|f| f.path()).as_ref() == Some(&csv_path));
    chooser.response(gtk::ResponseType::Accept);
    let wizard = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.is_visible() && w.widget_name() == "csv-import-wizard")
        .unwrap();
    let preview = widget_named(wizard.upcast_ref(), "csv-preview")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    let execute = widget_named(wizard.upcast_ref(), "csv-execute")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    until(|| preview.is_sensitive());
    assert!(!execute.is_sensitive());
    preview.emit_clicked();
    until(|| execute.is_sensitive());
    let delimiter = widget_named(wizard.upcast_ref(), "csv-delimiter")
        .unwrap()
        .downcast::<gtk::DropDown>()
        .unwrap();
    delimiter.set_selected(1);
    assert!(!execute.is_sensitive());
    delimiter.set_selected(0);
    preview.emit_clicked();
    until(|| execute.is_sensitive());
    grid_app_tests::layout_snapshot(&wizard, "target/csv-import-wizard.png");
    execute.emit_clicked();
    let confirm = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::MessageDialog>().ok())
        .find(|w| {
            w.is_visible()
                && w.text().as_deref() == Some("Добавить CSV в dbdeck_test.public.sample_records?")
        })
        .unwrap();
    confirm.response(gtk::ResponseType::Cancel);
    assert!(ui.transfer_cancel.borrow().is_none());
    wizard.close();
    // SQL from a server-level profile must request the target database.
    let mut server = profile.clone();
    server.database.clear();
    let server_key = profile_session_key(&server);
    ui.sessions
        .borrow_mut()
        .insert(server_key.clone(), (server.clone(), pool.clone()));
    ensure_session_table_tabs(&ui, &server_key);
    add_session_tab(&ui, &server);
    select_session(&ui, &server_key);
    let before = ui.sql_tabs.borrow().len();
    create_sql_tab(&ui, &server_key);
    assert_eq!(ui.sql_tabs.borrow().len(), before);
    select_target_database("postgres");
    until(|| ui.sql_tabs.borrow().len() == before + 1);
    assert_eq!(
        ui.current_profile.borrow().as_ref().unwrap().database,
        "postgres"
    );
    let actual = runtime
        .block_on(
            ui.pool
                .borrow()
                .as_ref()
                .unwrap()
                .execute_sql("SELECT current_database() AS name"),
        )
        .unwrap();
    assert_eq!(actual.data.rows, [vec![CellValue::Text("postgres".into())]]);
    select_connection_database(&ui, &profile);
    select_target_database("dbdeck_test");
    until(|| ui.current_profile.borrow().as_ref().unwrap().database == "dbdeck_test");
    close_sql_tab(&ui, &id);
    ui.window.close();
    runtime.block_on(pg.close());
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires local PostgreSQL; imports TEMP objects only, no persistent DB changes"]
fn postgres_import_temp_sql_and_error() {
    let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({"id":"restore-test","name":"restore","kind":"postgre_sql","host":"127.0.0.1","port":55432,"database":"dbdeck_test","username":"dbdeck_test","environment":"local","color":"#ffffff","read_only":false,"tls_mode":"disabled"})).unwrap();
    let secret = std::env::var("DBDECK_PG_PASSWORD").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let targets = runtime
        .block_on(DbPool::profile_database_targets(&profile, &secret))
        .unwrap();
    assert_eq!(targets[0].0, "dbdeck_test");
    let mut stale = profile.clone();
    stale.database = format!("missing_{}", Uuid::new_v4().simple());
    let targets = runtime
        .block_on(DbPool::profile_database_targets(&stale, &secret))
        .unwrap();
    assert!(
        targets
            .iter()
            .any(|(name, details)| name == "dbdeck_test" && details.contains("Кодировка"))
    );

    let path = std::env::temp_dir().join(format!("dbdeck-temp-import-{}.sql", Uuid::new_v4()));
    fs::write(&path, "CREATE TEMP TABLE restored_temp(id integer PRIMARY KEY, value text); INSERT INTO restored_temp VALUES(1,'Привет'); DO $$ BEGIN IF (SELECT value FROM restored_temp WHERE id=1) <> 'Привет' THEN RAISE EXCEPTION 'wrong value'; END IF; END $$;").unwrap();
    let result = native_backup::restore(&profile, &secret, &path, &transfer::Cancellation::new());
    fs::write(
        &path,
        "CREATE TEMP TABLE restored_temp(id integer); SELECT missing_column FROM restored_temp;",
    )
    .unwrap();
    let failure = native_backup::restore(&profile, &secret, &path, &transfer::Cancellation::new());
    fs::remove_file(path).unwrap();
    assert!(result.is_ok(), "{result:?}");
    assert!(failure.is_err());
}

#[test]
#[ignore = "requires local MySQL/MariaDB; TEMP tables only, no persistent data changes"]
fn mysql_mariadb_import_temp_sql_and_error() {
    for (kind, port, variable) in [
        (DatabaseKind::MariaDb, 53306, "DBDECK_MARIADB_PASSWORD"),
        (DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD"),
    ] {
        let profile = ConnectionProfile {
            id: "import-temp".into(),
            name: "import-temp".into(),
            kind,
            host: "127.0.0.1".into(),
            port,
            database: "dbdeck_test".into(),
            username: "dbdeck_test".into(),
            environment: EnvironmentTag::Local,
            color: "#ffffff".into(),
            read_only: false,
            tls_mode: TlsMode::Disabled,
            password_saved: false,
            gopass_entry: None,
            tls_files: Default::default(),
            native_tools_dir: None,
        };
        let secret = std::env::var(variable).unwrap();
        let path = std::env::temp_dir().join(format!("dbdeck-temp-import-{}.sql", Uuid::new_v4()));
        fs::write(&path, "CREATE TEMPORARY TABLE restored_temp(id integer PRIMARY KEY, value text); INSERT INTO restored_temp VALUES(1,'Привет'); SELECT value INTO @restored_value FROM restored_temp WHERE id=1; INSERT INTO restored_temp VALUES(IF(@restored_value='Привет',2,NULL), 'verified');").unwrap();
        let result =
            native_backup::restore(&profile, &secret, &path, &transfer::Cancellation::new());
        fs::write(&path, "CREATE TEMPORARY TABLE restored_temp(id integer); SELECT missing_column FROM restored_temp;").unwrap();
        let failure =
            native_backup::restore(&profile, &secret, &path, &transfer::Cancellation::new());
        fs::remove_file(path).unwrap();
        assert!(result.is_ok(), "{kind:?}: {result:?}");
        assert!(failure.is_err());
    }
}
