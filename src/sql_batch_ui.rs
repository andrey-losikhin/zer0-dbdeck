//! The report uses a snapshot of the whole editor and a separate read-only session.
use super::*;

pub(super) fn choose_report(ui: &Rc<Ui>, id: &str) {
    if ui.transfer_cancel.borrow().is_some() {
        show_error_dialog(
            ui,
            "Операция уже выполняется",
            "Дождитесь завершения или отмените текущую операцию.",
        );
        return;
    }
    let Some((session_key, sql, timeout, schema)) = ui.sql_tabs.borrow().get(id).map(|tab| {
        (
            tab.session_key.clone(),
            query_text(tab),
            tab.timeout.value_as_int() as u64,
            tab.schema.clone(),
        )
    }) else {
        return;
    };
    let Some((profile, pool)) = ui.sessions.borrow().get(&session_key).cloned() else {
        return;
    };
    let statements = sql_batch::split(
        &sql,
        matches!(profile.kind, DatabaseKind::MySql | DatabaseKind::MariaDb),
    );
    if statements.is_empty() {
        ui.status.set_text("Нет SQL-запросов для отчёта");
        return;
    }
    let chooser = dialogs::file_chooser(
        Some("Сохранить результаты SQL в Markdown"),
        Some(&ui.window),
        gtk::FileChooserAction::Save,
        Some("Выполнить и сохранить"),
        Some("Отмена"),
    );
    chooser.set_current_name("sql-investigation.md");
    if let Ok(directory) = query_directory_for_tab(ui, id) {
        let _ = chooser.set_current_folder(Some(&gtk::gio::File::for_path(directory)));
    }
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Markdown (*.md)"));
    filter.add_pattern("*.md");
    chooser.add_filter(&filter);
    let weak = Rc::downgrade(ui);
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept
            && let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
        {
            let add_extension = path.extension().is_none();
            let path = if add_extension {
                path.with_extension("md")
            } else {
                path
            };
            if add_extension && path.exists() {
                // The file chooser confirmed the typed path, not the appended .md path.
                let confirm = gtk::MessageDialog::builder()
                    .transient_for(&ui.window)
                    .buttons(gtk::ButtonsType::YesNo)
                    .text("Заменить Markdown-отчёт?")
                    .secondary_text(format!(
                        "Файл {} уже существует. Заменить его новым отчётом?",
                        path.display()
                    ))
                    .build();
                dialogs::install(&confirm);
                let owner = Rc::downgrade(&ui);
                let pool = pool.clone();
                let profile = profile.clone();
                let statements = statements.clone();
                let schema = schema.clone();
                confirm.connect_response(move |dialog, response| {
                    if response == gtk::ResponseType::Yes
                        && let Some(ui) = owner.upgrade()
                    {
                        start_report(
                            &ui,
                            pool.clone(),
                            profile.clone(),
                            statements.clone(),
                            path.clone(),
                            timeout,
                            schema.clone(),
                        );
                    }
                    dialog.close();
                });
                confirm.present();
            } else {
                start_report(
                    &ui,
                    pool.clone(),
                    profile.clone(),
                    statements.clone(),
                    path,
                    timeout,
                    schema.clone(),
                );
            }
        }
        dialog.destroy();
    });
    chooser.show();
}

pub(super) fn start_report(
    ui: &Rc<Ui>,
    pool: DbPool,
    profile: ConnectionProfile,
    statements: Vec<String>,
    path: PathBuf,
    timeout: u64,
    schema: Option<String>,
) {
    let Some(cancel) = start_transfer(ui) else {
        return;
    };
    ui.status.set_text("Готовлю Markdown-отчёт…");
    let sender = ui.sender.clone();
    ui.runtime.spawn(async move {
        let progress = sender.clone();
        let result = sql_batch::run_with_schema(
            pool,
            &profile,
            &statements,
            &path,
            timeout,
            cancel,
            move |index, total| {
                let _ = progress.send(Event::SqlBatchProgress(index, total));
            },
            schema,
        )
        .await;
        let _ = sender.send(Event::SqlBatchFinished(path, result));
    });
}
