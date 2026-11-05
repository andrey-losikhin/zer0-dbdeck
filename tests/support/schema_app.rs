use super::*;
use grid_app_tests::until;

fn sql_id(ui: &Ui, group: &str) -> String {
    ui.active_table_by_session.borrow()[group]
        .strip_prefix("sql::")
        .unwrap()
        .to_owned()
}

fn query(ui: &Rc<Ui>, id: &str, text: &str) -> Arc<TableData> {
    ui.sql_tabs.borrow()[id].buffer.set_text(text);
    run_sql_tab(ui, id);
    until(|| ui.sql_tabs.borrow()[id].run_button.is_sensitive());
    assert!(
        !ui.sql_tabs.borrow()[id].error_message.is_visible(),
        "{}",
        ui.sql_tabs.borrow()[id].error_message.text()
    );
    ui.sql_tabs.borrow()[id].result.clone().unwrap()
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL; read-only queries"]
fn postgres_schema_groups_sql_and_drafts() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
        "id":"schema-ui", "name":"server", "kind":"postgre_sql", "host":"127.0.0.1", "port":55432,
        "database":"dbdeck_test", "username":"dbdeck_test", "environment":"local", "color":"#94e2d5",
        "read_only":true, "tls_mode":"disabled"
    })).unwrap();
    let password = std::env::var("DBDECK_PG_PASSWORD").unwrap();
    let pool = runtime
        .block_on(DbPool::connect(&profile, &password))
        .unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.schema-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    {
        let mut store = ui.store.borrow_mut();
        let store = store.as_mut().unwrap();
        store
            .unlock_or_create_vault("synthetic schema test key")
            .unwrap();
        store.save_profile(profile.clone(), &password).unwrap();
    }
    let session = profile_session_key(&profile);
    ui.connect_requests
        .borrow_mut()
        .insert(session.clone(), "connect".into());
    handle_event(
        &ui,
        Event::Connected("connect".into(), profile.clone(), Ok(pool.clone())),
    );
    let public = workspace_ui::key(&profile, Some("public"));
    let audit = workspace_ui::key(&profile, Some("audit"));
    for (schema, name) in [("public", "sample_records"), ("audit", "events")] {
        open_table(
            &ui,
            DbObject {
                schema: schema.into(),
                name: name.into(),
                kind: database::DbObjectKind::Table,
            },
            false,
        );
        let key = format!("{session}:{schema}.{name}");
        until(|| ui.table_data.borrow().contains_key(&key));
    }
    assert_eq!(
        ui.sessions.borrow().len(),
        1,
        "schemas must share the database pool"
    );
    assert_eq!(ui.session_tabs.n_pages(), 2);
    assert_eq!(ui.session_table_tabs.borrow()[&public].n_pages(), 1);
    assert_eq!(ui.session_table_tabs.borrow()[&audit].n_pages(), 1);
    assert!(find_table_page(&ui, &format!("{session}:public.sample_records")).is_some());
    assert!(find_table_page(&ui, &format!("{session}:audit.events")).is_some());
    let add = ui.session_table_tabs.borrow()[&audit]
        .action_widget(gtk::PackType::End)
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    add.emit_clicked();
    let audit_id = sql_id(&ui, &audit);
    assert_eq!(
        ui.sql_tabs.borrow()[&audit_id].schema.as_deref(),
        Some("audit")
    );
    let audit_sql = "SELECT current_schema() AS schema_name, description FROM events ORDER BY id;";
    let result = query(&ui, &audit_id, audit_sql);
    assert_eq!(result.rows[0][0], CellValue::Text("audit".into()));
    assert!(matches!(&result.rows[0][1], CellValue::Text(value) if value.starts_with("AUDIT:")));
    workspace_ui::select(&ui, &public);
    create_sql_tab(&ui, &session);
    let public_id = sql_id(&ui, &public);
    let public_sql = "SELECT current_schema() AS schema_name, COUNT(*) FROM sample_records;";
    let result = query(&ui, &public_id, public_sql);
    assert_eq!(result.rows[0][0], CellValue::Text("public".into()));
    assert_eq!(ui.session_table_tabs.borrow()[&audit].n_pages(), 2);
    assert_eq!(ui.session_table_tabs.borrow()[&public].n_pages(), 2);
    workspace_ui::select(&ui, &audit);
    assert_eq!(
        ui.table_tabs.visible_child_name().as_deref(),
        Some(audit.as_str())
    );
    let grid = ui.sql_tabs.borrow()[&audit_id].tabulator.clone();
    until(|| {
        grid_app_tests::js(
            &grid,
            "typeof table !== 'undefined' && table && table.getRows().length === 2",
        ) == true
    });
    grid_app_tests::layout_snapshot(&ui.window, "target/postgres-schema-groups.png");

    // Opening a table's DDL must use that table's schema even from another active group.
    workspace_ui::select(&ui, &public);
    structure_ui::show(
        &ui,
        &profile,
        &DbObject {
            schema: "audit".into(),
            name: "events".into(),
            kind: database::DbObjectKind::Table,
        },
    );
    let inspector = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.widget_name() == "object-structure")
        .unwrap();
    let open_ddl = grid_app_tests::named_widget(inspector.upcast_ref(), "structure-open-sql")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    until(|| open_ddl.is_sensitive());
    open_ddl.emit_clicked();
    let ddl_id = sql_id(&ui, &audit);
    assert_ne!(ddl_id, audit_id);
    assert_eq!(
        ui.sql_tabs.borrow()[&ddl_id].schema.as_deref(),
        Some("audit")
    );
    assert!(
        ui.sql_tabs.borrow()[&ddl_id].result.is_none(),
        "opening DDL must not execute it"
    );
    close_sql_tab(&ui, &ddl_id);
    workspace_ui::select(&ui, &audit);

    // A fresh report connection must inherit the SQL tab's schema, too.
    let path = std::env::temp_dir().join(format!("dbdeck-audit-{}.md", Uuid::new_v4()));
    sql_batch_ui::start_report(
        &ui,
        pool.clone(),
        profile.clone(),
        sql_batch::split(audit_sql, false),
        path.clone(),
        5,
        Some("audit".into()),
    );
    until(|| ui.transfer_cancel.borrow().is_none());
    let report = fs::read_to_string(&path).unwrap();
    assert!(report.contains("Схема: audit") && report.contains("AUDIT:"));
    for window in gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::MessageDialog>().ok())
    {
        window.close();
    }

    let audit_table = format!("{session}:audit.events");
    let edits = ui.table_edit_states.borrow()[&audit_table].clone();
    edits.saving.set(true);
    workspace_ui::close(&ui, &audit);
    assert_eq!(
        ui.session_tabs.n_pages(),
        2,
        "saving must block closing its schema"
    );
    edits.saving.set(false);
    edits
        .pending
        .borrow_mut()
        .insert((0, 1), Some("unsaved UI value".into()));
    workspace_ui::close(&ui, &audit);
    let confirmation = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::MessageDialog>().ok())
        .find(|w| w.text().as_deref() == Some("Отменить несохранённые изменения и закрыть группу?"))
        .unwrap();
    confirmation.response(gtk::ResponseType::No);
    assert_eq!(ui.session_tabs.n_pages(), 2);
    assert!(!edits.pending.borrow().is_empty());
    edits.pending.borrow_mut().clear();

    let audit_session = ui.sql_tabs.borrow()[&audit_id].session.clone();
    runtime.block_on(async {
        audit_session
            .transaction(sql_session::TransactionAction::Manual)
            .await
            .unwrap();
        audit_session
            .execute_with_timeout("SELECT 1", 5)
            .await
            .unwrap();
    });
    workspace_ui::close(&ui, &audit);
    assert_eq!(
        ui.session_tabs.n_pages(),
        2,
        "a schema's own pending transaction must block its close"
    );
    runtime
        .block_on(audit_session.transaction(sql_session::TransactionAction::Rollback))
        .unwrap();

    // Closing another schema must not interfere with a pending transaction.
    let public_session = ui.sql_tabs.borrow()[&public_id].session.clone();
    runtime.block_on(async {
        public_session
            .transaction(sql_session::TransactionAction::Manual)
            .await
            .unwrap();
        public_session
            .execute_with_timeout("SELECT 1", 5)
            .await
            .unwrap();
    });
    assert!(public_session.has_pending_transaction());
    workspace_ui::close(&ui, &audit);
    assert_eq!(ui.session_tabs.n_pages(), 1);
    assert!(ui.sessions.borrow().contains_key(&session));
    assert!(ui.sql_tabs.borrow().contains_key(&public_id));
    assert!(!ui.sql_tabs.borrow().contains_key(&audit_id));
    assert!(public_session.has_pending_transaction());
    workspace_ui::close(&ui, &public);
    assert_eq!(
        ui.session_tabs.n_pages(),
        1,
        "pending transaction must block closing its group"
    );
    runtime
        .block_on(public_session.transaction(sql_session::TransactionAction::Rollback))
        .unwrap();
    workspace_ui::close(&ui, &public);
    assert_eq!(ui.session_tabs.n_pages(), 0);
    assert!(!ui.sessions.borrow().contains_key(&session));
    let drafts = ui
        .store
        .borrow()
        .as_ref()
        .unwrap()
        .sql_workspace()
        .drafts
        .clone();
    assert!(
        drafts
            .iter()
            .any(|d| d.schema.as_deref() == Some("audit") && d.sql == audit_sql)
    );
    assert!(
        drafts
            .iter()
            .any(|d| d.schema.as_deref() == Some("public") && d.sql == public_sql)
    );
    ui.connect_requests
        .borrow_mut()
        .insert(session.clone(), "reconnect".into());
    handle_event(
        &ui,
        Event::Connected("reconnect".into(), profile, Ok(pool.clone())),
    );
    assert_eq!(ui.session_tabs.n_pages(), 2);
    assert_eq!(
        ui.sql_tabs.borrow().len(),
        2,
        "both schema drafts must restore once"
    );
    let restored_audit = ui
        .sql_tabs
        .borrow()
        .iter()
        .find(|(_, t)| t.group_key == audit)
        .map(|(id, _)| id.clone())
        .unwrap();
    workspace_ui::select(&ui, &audit);
    let result = query(&ui, &restored_audit, audit_sql);
    assert_eq!(result.rows[0][0], CellValue::Text("audit".into()));
    assert_eq!(ui.sessions.borrow().len(), 1);
    println!(
        "PostgreSQL: public/audit tables and SQL isolated; schema search_path and report correct; group close preserves sibling transaction; both drafts restore once"
    );
    ui.window.close();
    runtime.block_on(async {
        match pool {
            DbPool::PostgreSql(p) => p.close().await,
            DbPool::MySql(p) => p.close().await,
        }
    });
}
