use super::*;
use grid_app_tests::until;

/// Menu actions carry an icon and a gtk::Label instead of a button label.
fn has_label(widget: &gtk::Widget, text: &str) -> bool {
    if widget
        .downcast_ref::<gtk::Label>()
        .is_some_and(|label| label.text() == text)
    {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if has_label(&widget, text) {
            return true;
        }
    }
    false
}

fn button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    if let Some(button) = widget.downcast_ref::<gtk::Button>()
        && (button.label().as_deref() == Some(label) || has_label(button.upcast_ref(), label))
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

fn task_cancel(widget: &gtk::Widget, title: &str) -> Option<gtk::Button> {
    if let Some(stop) = widget
        .last_child()
        .and_then(|child| child.downcast::<gtk::Button>().ok())
        .filter(|stop| stop.label().as_deref() == Some("Отменить") && has_label(widget, title))
    {
        return Some(stop);
    }
    std::iter::successors(widget.first_child(), gtk::Widget::next_sibling)
        .find_map(|child| task_cancel(&child, title))
}

#[test]
#[ignore = "requires headless GTK"]
fn background_tasks_retains_recent_outcomes() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let tasks = BackgroundTasks::new();
    let popover = tasks.button.popover().unwrap();
    assert!(has_label(popover.upcast_ref(), "Задач пока нет"));
    tasks.start("ongoing", "Текущая задача", || {});
    for index in 0..12 {
        let id = format!("Задача {index}");
        tasks.start(&id, &id, || {});
        tasks.finish(&id, Outcome::Completed);
    }
    assert!(tasks.can_cancel("ongoing"));
    assert!(task_cancel(popover.upcast_ref(), "Текущая задача").is_some());
    assert!(!has_label(popover.upcast_ref(), "Задача 1"));
    assert!(has_label(popover.upcast_ref(), "Задача 2"));
    assert!(has_label(popover.upcast_ref(), "Задача 11"));
    assert!(task_cancel(popover.upcast_ref(), "Задача 11").is_none());
    tasks.cancel_status("ongoing", false);
    assert!(!tasks.progress("ongoing", "Устаревший прогресс"));
    assert!(has_label(popover.upcast_ref(), "Завершается…"));
    tasks.finish("ongoing", Outcome::Completed);
    assert!(!tasks.can_cancel("ongoing"));
}

fn batch_test_window() -> (Rc<Ui>, ConnectionProfile, DbPool, String) {
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
    (ui, profile, pool, id)
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL"]
fn sql_estimated_plan_uses_selection_and_preserves_editor() {
    let (ui, _, _, id) = batch_test_window();
    let buffer = ui.sql_tabs.borrow()[&id].buffer.clone();
    let selected = "SELECT * FROM (SELECT 1 UNION ALL SELECT 2) AS branches;";
    let script = format!("{selected}\nSELECT bad_column;");
    buffer.set_text(&script);
    buffer.select_range(
        &buffer.start_iter(),
        &buffer.iter_at_offset(selected.len() as i32),
    );
    let menu_action = button(
        ui.window.upcast_ref(),
        "План запроса (EXPLAIN, Ctrl+Shift+G)",
    )
    .expect("EXPLAIN menu action");
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_mapped());
    menu_action.emit_clicked();
    until(|| {
        ui.sql_tabs.borrow()[&id]
            .result_stack
            .visible_child_name()
            .as_deref()
            == Some("plan")
    });
    {
        let tabs = ui.sql_tabs.borrow();
        let tab = &tabs[&id];
        assert!(!tab.plan_view.is_editable() && tab.export_button.is_sensitive());
        assert!(tab.status.text().starts_with("Оценочный план (EXPLAIN)"));
        let plan = tab.plan_view.buffer();
        let text = plan.text(&plan.start_iter(), &plan.end_iter(), false);
        assert!(text.contains("Append") && text.contains("\n  ->"), "{text}");
        assert!(!text.contains("actual time"));
        assert!(tab.executed_sql.is_none());
    }
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        script
    );
    let (start, end) = buffer.selection_bounds().unwrap();
    assert_eq!(buffer.text(&start, &end, true), selected);

    buffer.set_text("SELECT 3;");
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::g,
        gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK
    ));
    until(|| {
        ui.sql_tabs.borrow()[&id]
            .result_stack
            .visible_child_name()
            .as_deref()
            == Some("plan")
    });
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::_2,
        gtk::gdk::ModifierType::CONTROL_MASK
    ));
    let plan = ui.sql_tabs.borrow()[&id].plan_view.clone();
    assert_eq!(
        gtk::prelude::RootExt::focus(&ui.window).as_ref(),
        Some(plan.upcast_ref())
    );

    run_sql_tab(&ui, &id);
    until(|| {
        ui.sql_tabs.borrow()[&id]
            .result_stack
            .visible_child_name()
            .as_deref()
            == Some("data")
    });
    assert_eq!(
        ui.sql_tabs.borrow()[&id].result.as_ref().unwrap().rows[0][0],
        database::CellValue::Text("3".into())
    );

    buffer.set_text("BEGIN; LOCK TABLE sample_records IN ACCESS EXCLUSIVE MODE;");
    run_sql_tab(&ui, &id);
    until(|| {
        let tabs = ui.sql_tabs.borrow();
        tabs[&id].run_button.is_sensitive() && tabs[&id].transaction_state.pending
    });
    buffer.set_text("SELECT * FROM sample_records");
    explain_sql_tab(&ui, &id);
    assert!(ui.sql_tabs.borrow()[&id].running_plan.is_some());
    stop_sql_tab(&ui, &id, None);
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    {
        let tabs = ui.sql_tabs.borrow();
        assert!(tabs[&id].transaction_state.pending);
        let error = tabs[&id].error_message.text();
        assert!(
            error.contains("остановлен") || error.contains("отменён"),
            "{error}"
        );
    }
    buffer.set_text("SELECT 4;");
    run_sql_tab(&ui, &id);
    until(|| {
        ui.sql_tabs.borrow()[&id]
            .result_stack
            .visible_child_name()
            .as_deref()
            == Some("data")
    });
    assert_eq!(
        ui.sql_tabs.borrow()[&id].result.as_ref().unwrap().rows[0][0],
        database::CellValue::Text("4".into())
    );
    change_sql_transaction(&ui, &id, Some(sql_session::TransactionAction::Rollback));
    until(|| !ui.sql_tabs.borrow()[&id].transaction_state.pending);
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL"]
fn sql_batch_markdown_ui() {
    let (ui, _, pool, id) = batch_test_window();
    let buffer = ui.sql_tabs.borrow()[&id].buffer.clone();
    buffer.set_text("SELECT 1 AS first; SELECT missing_batch_ui; SELECT 3 AS last;");
    buffer.select_range(&buffer.start_iter(), &buffer.iter_at_offset(18));
    let execute = button(ui.window.upcast_ref(), "Выполнить в Markdown…").expect("menu action");
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_mapped());
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
    assert!(!ui.background_tasks.can_cancel(TRANSFER));
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

    ui.window.close();
    if let DbPool::PostgreSql(pool) = pool {
        ui.runtime.block_on(pool.close());
    }
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL"]
fn background_tasks_cancel_scoped_queries_and_report() {
    let (ui, profile, pool, id) = batch_test_window();
    let root = std::env::temp_dir().join(format!("dbdeck-tasks-ui-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let buffer = ui.sql_tabs.borrow()[&id].buffer.clone();
    buffer.set_text("SELECT pg_sleep(30)");
    run_sql_tab(&ui, &id);
    let sql_title = format!(
        "{} · batch UI · dbdeck_test",
        ui.sql_tabs.borrow()[&id].query_name
    );
    let tasks = &ui.background_tasks;
    let popover = tasks.button.popover().unwrap();
    let task_list = popover.upcast_ref();
    let old_stop = task_cancel(task_list, &sql_title).expect("first SQL task");
    replace_tab_session(&ui, &id);
    assert!(!old_stop.is_sensitive());
    run_sql_tab(&ui, &id);
    old_stop.emit_clicked();
    assert!(ui.sql_tabs.borrow()[&id].stop_button.is_sensitive());
    assert!(task_cancel(task_list, &sql_title).unwrap().is_sensitive());
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
    tasks.button.popup();
    until(|| popover.is_mapped());
    assert!(has_label(task_list, "Markdown: запрос 1 из 2…"));
    let stop_sql = task_cancel(task_list, &sql_title).expect("SQL cancel in task list");
    stop_sql.emit_clicked();
    assert!(!stop_sql.is_sensitive());
    assert!(has_label(task_list, "Запрошена отмена…"));
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert!(has_label(task_list, "Не завершено · запрошена отмена"));
    assert!(tasks.can_cancel(TRANSFER));
    task_cancel(task_list, "SQL-отчёт Markdown · cancelled.md")
        .expect("Markdown cancel in task list")
        .emit_clicked();
    until(|| ui.transfer_cancel.borrow().is_none());
    assert!(has_label(task_list, "Частичный отчёт сохранён"));
    let report = fs::read_to_string(root.join("cancelled.md")).unwrap();
    assert!(report.contains("Остановлено пользователем") && report.contains("### 2. Не выполнен"));

    buffer.set_text("SELECT 1 AS finished_before_stop");
    run_sql_tab(&ui, &id);
    let late_stop = task_cancel(task_list, &sql_title).expect("completed SQL awaiting GTK");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while ui.activity.active() != 0 {
        assert!(std::time::Instant::now() < deadline);
        // Do not dispatch the GTK completion until after the late click.
        std::thread::sleep(Duration::from_millis(10));
    }
    late_stop.emit_clicked();
    assert!(!late_stop.is_sensitive());
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert!(ui.sql_tabs.borrow()[&id].result.is_some());
    buffer.set_text("SELECT 2 AS after_late_stop FROM pg_sleep(0.2)");
    run_sql_tab(&ui, &id);
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert_eq!(
        ui.sql_tabs.borrow()[&id].result.as_ref().unwrap().columns,
        ["after_late_stop"]
    );
    ui.window.close();
    if let DbPool::PostgreSql(pool) = pool {
        ui.runtime.block_on(pool.close());
    }
}
