use super::*;
use grid_app_tests::until;

fn button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    if let Some(button) = widget.downcast_ref::<gtk::Button>()
        && button.label().as_deref() == Some(label)
    {
        return Some(button.clone());
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Some(found) = button(&widget, label) {
            return Some(found);
        }
    }
    None
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL"]
fn sql_batch_markdown_ui() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
        "id":"batch-ui", "name":"batch UI", "kind":"postgre_sql", "host":"127.0.0.1", "port":55432,
        "database":"dbdeck_test", "username":"dbdeck_test", "environment":"local",
        "color":"#ffffff", "read_only":false, "tls_mode":"disabled"
    }))
    .unwrap();
    let pool = runtime
        .block_on(DbPool::connect(
            &profile,
            &std::env::var("DBDECK_PG_PASSWORD").unwrap(),
        ))
        .unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.batch-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    {
        let mut store = ui.store.borrow_mut();
        store
            .as_mut()
            .unwrap()
            .unlock_or_create_vault("synthetic batch test key")
            .unwrap();
    }
    let session_key = profile_session_key(&profile);
    ui.sessions
        .borrow_mut()
        .insert(session_key.clone(), (profile.clone(), pool.clone()));
    *ui.current_profile.borrow_mut() = Some(profile.clone());
    create_sql_tab(&ui, &session_key);
    let id = ui.sql_tabs.borrow().keys().next().unwrap().clone();
    let buffer = ui.sql_tabs.borrow()[&id].buffer.clone();
    buffer.set_text("SELECT 1 AS first; SELECT missing_batch_ui; SELECT 3 AS last;");
    buffer.select_range(&buffer.start_iter(), &buffer.iter_at_offset(18));
    let execute = button(ui.window.upcast_ref(), "Выполнить в Markdown…").expect("toolbar button");
    until(|| execute.is_mapped());
    grid_app_tests::layout_snapshot(&ui.window, "target/sql-batch-toolbar.png");
    let root = std::env::temp_dir().join(format!("dbdeck-batch-ui-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    execute.emit_clicked();
    let chooser = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::FileChooserDialog>().ok())
        .find(|w| w.title().as_deref() == Some("Сохранить результаты SQL в Markdown"))
        .unwrap();
    until(|| chooser.is_mapped());
    chooser
        .set_current_folder(Some(&gtk::gio::File::for_path(&root)))
        .unwrap();
    until(|| chooser.current_folder().and_then(|f| f.path()).as_deref() == Some(root.as_path()));
    chooser.set_current_name("report");
    until(|| {
        chooser.file().and_then(|f| f.path()).as_deref() == Some(root.join("report").as_path())
    });
    buffer.set_text("SELECT 999 AS changed_after_dialog;");
    chooser.response(gtk::ResponseType::Accept);
    until(|| root.join("report.md").exists() && ui.transfer_cancel.borrow().is_none());
    let report = fs::read_to_string(root.join("report.md")).unwrap();
    assert!(
        report.contains("## 1. Успешно")
            && report.contains("## 2. Ошибка")
            && report.contains("## 3. Успешно")
    );
    assert!(!report.contains("changed_after_dialog"));
    assert!(ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert!(!ui.transfer_cancel_button.is_visible());
    for w in gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::MessageDialog>().ok())
    {
        w.close();
    }

    // Appending .md must not silently overwrite a path the chooser did not confirm.
    execute.emit_clicked();
    let chooser = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::FileChooserDialog>().ok())
        .find(|w| {
            w.title().as_deref() == Some("Сохранить результаты SQL в Markdown") && w.is_visible()
        })
        .unwrap();
    until(|| chooser.is_mapped());
    chooser
        .set_current_folder(Some(&gtk::gio::File::for_path(&root)))
        .unwrap();
    until(|| chooser.current_folder().and_then(|f| f.path()).as_deref() == Some(root.as_path()));
    chooser.set_current_name("report");
    until(|| {
        chooser.file().and_then(|f| f.path()).as_deref() == Some(root.join("report").as_path())
    });
    chooser.response(gtk::ResponseType::Accept);
    let mut confirmation = None;
    until(|| {
        confirmation = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::MessageDialog>().ok())
            .find(|w| w.text().as_deref() == Some("Заменить Markdown-отчёт?") && w.is_visible());
        confirmation.is_some()
    });
    assert!(ui.transfer_cancel.borrow().is_none());
    confirmation.unwrap().response(gtk::ResponseType::No);
    assert_eq!(fs::read_to_string(root.join("report.md")).unwrap(), report);

    // Cancelling a file chooser never executes SQL or starts a transfer.
    execute.emit_clicked();
    let chooser = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::FileChooserDialog>().ok())
        .find(|w| {
            w.title().as_deref() == Some("Сохранить результаты SQL в Markdown") && w.is_visible()
        })
        .unwrap();
    chooser.response(gtk::ResponseType::Cancel);
    assert!(ui.transfer_cancel.borrow().is_none());

    sql_batch_ui::start_report(
        &ui,
        pool.clone(),
        profile,
        sql_batch::split("SELECT pg_sleep(10); SELECT 2;", false),
        root.join("cancelled.md"),
        15,
        None,
    );
    until(|| ui.status.text().contains("запрос 1 из 2"));
    ui.transfer_cancel_button.emit_clicked();
    until(|| ui.transfer_cancel.borrow().is_none());
    let report = fs::read_to_string(root.join("cancelled.md")).unwrap();
    assert!(report.contains("Остановлено пользователем") && report.contains("### 2. Не выполнен"));
    println!(
        "UI: toolbar button, chooser, whole-editor snapshot, ordered report, cancel chooser, cancel running query and partial report passed; {}",
        root.display()
    );
    ui.window.close();
    runtime.block_on(async {
        match pool {
            DbPool::PostgreSql(p) => p.close().await,
            DbPool::MySql(p) => p.close().await,
        }
    });
}
