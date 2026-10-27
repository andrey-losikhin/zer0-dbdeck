use super::*;
use native_backup::DumpMode;

pub(super) fn install(
    ui: &Rc<Ui>,
    card: &gtk::Box,
    actions: &gtk::Box,
    profile: &ConnectionProfile,
) {
    let button = gtk::MenuButton::new();
    button.set_icon_name("view-more-symbolic");
    button.set_tooltip_text(Some("Структура и резервная копия БД (Menu)"));
    button.add_css_class("flat");
    let popover = gtk::Popover::new();
    popover.set_autohide(true);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let select = sql_menu_action(
        &content,
        &popover,
        "Открыть базу данных…",
        "drive-harddisk-symbolic",
    );
    let owner = Rc::downgrade(ui);
    let target = profile.clone();
    select.connect_clicked(move |_| {
        if let Some(ui) = owner.upgrade() {
            select_connection_database(&ui, &target);
        }
    });
    shortcuts::button(card, &select, "<Alt>d");
    let schema = sql_menu_action(
        &content,
        &popover,
        "Структура БД без данных…",
        "view-list-symbolic",
    );
    let full = sql_menu_action(
        &content,
        &popover,
        "Полная резервная копия БД…",
        "document-save-symbolic",
    );
    let import = sql_menu_action(
        &content,
        &popover,
        "Импорт SQL / восстановление БД…",
        "document-open-symbolic",
    );
    let weak = Rc::downgrade(ui);
    let import_profile = profile.clone();
    import.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            choose_import(&ui, &import_profile);
        }
    });
    shortcuts::button(card, &import, "<Alt>i");
    let csv = sql_menu_action(
        &content,
        &popover,
        "Импорт CSV в таблицу…",
        "x-office-spreadsheet-symbolic",
    );
    let owner = Rc::downgrade(ui);
    let csv_profile = profile.clone();
    csv.connect_clicked(move |_| {
        if let Some(ui) = owner.upgrade() {
            csv_import_ui::choose(&ui, &csv_profile, None);
        }
    });
    shortcuts::button(card, &csv, "<Alt><Shift>i");
    popover.set_child(Some(&content));
    button.set_popover(Some(&popover));
    actions.append(&button);
    for (action, mode) in [
        (schema.clone(), DumpMode::Schema),
        (full.clone(), DumpMode::Full),
    ] {
        let weak = Rc::downgrade(ui);
        let profile = profile.clone();
        action.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                choose(&ui, &profile, mode);
            }
        });
    }
    shortcuts::button(card, &schema, "<Alt>b");
    shortcuts::button(card, &full, "<Alt><Shift>b");
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    let menu = button.clone();
    gesture.connect_pressed(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        menu.popup();
    });
    card.add_controller(gesture);
    let keys = gtk::EventControllerKey::new();
    let menu = button.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        if key == gtk::gdk::Key::Menu
            || (key == gtk::gdk::Key::F10 && state == gtk::gdk::ModifierType::SHIFT_MASK)
        {
            menu.popup();
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    card.add_controller(keys);
}

pub(super) fn choose(ui: &Rc<Ui>, profile: &ConnectionProfile, mode: DumpMode) {
    database_target_ui::choose(ui, profile, move |ui, profile| {
        choose_unlocked(ui, &profile, mode)
    });
}

fn choose_unlocked(ui: &Rc<Ui>, profile: &ConnectionProfile, mode: DumpMode) {
    let chooser = dialogs::file_chooser(
        Some(mode.title()),
        Some(&ui.window),
        gtk::FileChooserAction::Save,
        Some("Сохранить"),
        Some("Отмена"),
    );
    let base = if profile.database.is_empty() {
        &profile.name
    } else {
        &profile.database
    };
    let suffix = if matches!(mode, DumpMode::Schema) {
        "schema"
    } else {
        "backup"
    };
    chooser.set_current_name(&format!(
        "{}-{suffix}.{}",
        safe_query_file_component(base),
        mode.extension(profile.kind)
    ));
    let weak = Rc::downgrade(ui);
    let profile = profile.clone();
    chooser.connect_response(move |dialog, response| {
        if response != gtk::ResponseType::Accept {
            dialog.destroy();
            return;
        }
        let Some(ui) = weak.upgrade() else {
            dialog.destroy();
            return;
        };
        let Some(path) = dialog.file().and_then(|f| f.path()) else {
            dialog.destroy();
            return;
        };
        dialog.destroy();
        let password = if profile.gopass_entry.is_some() {
            Ok(String::new())
        } else {
            ui.store
                .borrow()
                .as_ref()
                .ok_or("Хранилище недоступно".into())
                .and_then(|s| s.password(&profile.id))
        };
        let password = match password {
            Ok(value) => value,
            Err(e) => {
                show_error_dialog(&ui, "Бэкап не запущен", &e);
                return;
            }
        };
        let Some(cancel) = start_transfer(&ui) else {
            return;
        };
        ui.status
            .set_text(&format!("{}: {}…", mode.title(), profile.name));
        let profile = profile.clone();
        let sender = ui.sender.clone();
        let runtime = ui.runtime.clone();
        let task_runtime = runtime.clone();
        runtime.spawn_blocking(move || {
            let result = task_runtime.block_on(async {
                let secret = resolve_profile_password(&profile, password).await?;
                if cancel.is_cancelled() {
                    return Err("Экспорт отменён".into());
                }
                let maria = if matches!(profile.kind, DatabaseKind::MySql | DatabaseKind::MariaDb) {
                    let pool = DbPool::connect(&profile, &secret).await?;
                    let DbPool::MySql(pool) = pool else {
                        unreachable!()
                    };
                    let version: String = sqlx::query_scalar("SELECT VERSION()")
                        .fetch_one(&pool)
                        .await
                        .map_err(|e| e.to_string())?;
                    pool.close().await;
                    version.to_ascii_lowercase().contains("mariadb")
                } else {
                    false
                };
                let progress = sender.clone();
                native_backup::dump(
                    &profile,
                    &secret,
                    mode,
                    maria,
                    &path,
                    &cancel,
                    move |bytes| {
                        let _ = progress.send(Event::DumpProgress(bytes));
                    },
                )
            });
            let _ = sender.send(Event::DumpFinished(path, profile.kind, result));
        });
    });
    chooser.show();
}

pub(super) fn choose_import(ui: &Rc<Ui>, profile: &ConnectionProfile) {
    if profile.read_only {
        show_error_dialog(
            ui,
            "Импорт запрещён",
            "Подключение доступно только для чтения.",
        );
        return;
    }
    database_target_ui::choose(ui, profile, |ui, profile| {
        let chooser = dialogs::file_chooser(
            Some(&format!("Импорт в {}", profile.database)),
            Some(&ui.window),
            gtk::FileChooserAction::Open,
            Some("Открыть"),
            Some("Отмена"),
        );
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("SQL / PostgreSQL custom dump"));
        filter.add_pattern("*.sql");
        filter.add_pattern("*.SQL");
        filter.add_pattern("*.dump");
        filter.add_pattern("*.backup");
        chooser.add_filter(&filter);
        let owner = Rc::downgrade(ui);
        chooser.connect_response(move |dialog, response| {
            let path = dialog.file().and_then(|file| file.path());
            dialog.destroy();
            if response == gtk::ResponseType::Accept {
                if let (Some(ui), Some(path)) = (owner.upgrade(), path) {
                    confirm_import(&ui, profile.clone(), path);
                }
            }
        });
        chooser.show();
    });
}

fn confirm_import(ui: &Rc<Ui>, profile: ConnectionProfile, path: PathBuf) {
    if matches!(profile.environment, EnvironmentTag::Prod) {
        let owner = Rc::downgrade(ui);
        let target = profile.clone();
        let details = format!(
            "Файл: {}\nИмпорт выполняет SQL/DDL и команды штатного клиента из файла. Они могут заменить/удалить данные и явно обратиться к другой БД; выбранная БД — только начальная. Используйте только проверенные файлы. В MySQL DDL и нетранзакционные объекты могут не откатываться. Используйте проверенную резервную копию.",
            path.display()
        );
        production_ui::confirm(
            ui,
            &profile,
            "Импорт/восстановление PROD",
            &details,
            move || {
                if let Some(ui) = owner.upgrade() {
                    start_import(&ui, target.clone(), path.clone());
                }
            },
        );
        return;
    }
    let dialog = gtk::MessageDialog::builder().transient_for(&ui.window)
        .message_type(gtk::MessageType::Warning).buttons(gtk::ButtonsType::None)
        .text(format!("Импорт в БД «{}»", profile.database))
        .secondary_text(format!("{} · {}:{} · пользователь {}\nФайл: {}\n\nБудут выполнены команды из файла. Используйте только доверенные файлы: SQL может содержать удаление данных и явные ссылки на другие БД. Выбранная БД используется как начальная. Автоматическое удаление существующих объектов не добавляется.\n\nMySQL/MariaDB: ошибка или отмена может оставить частично импортированные данные. PostgreSQL: одна транзакция, если SQL-файл сам не управляет транзакциями.", profile.name, profile.host, profile.port, profile.username, path.display()))
        .build();
    dialog.add_button("Отмена", gtk::ResponseType::Cancel);
    dialog.add_button("Импортировать", gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Cancel);
    dialogs::install(&dialog);
    let owner = Rc::downgrade(ui);
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response != gtk::ResponseType::Accept {
            return;
        }
        let Some(ui) = owner.upgrade() else {
            return;
        };
        start_import(&ui, profile.clone(), path.clone());
    });
    dialog.present();
}

fn start_import(ui: &Rc<Ui>, profile: ConnectionProfile, path: PathBuf) {
    let password = match database_target_ui::password(&ui, &profile) {
        Ok(secret) => secret,
        Err(error) => {
            show_error_dialog(&ui, "Импорт не запущен", &error);
            return;
        }
    };
    let Some(cancel) = start_transfer(&ui) else {
        return;
    };
    ui.status.set_text(&format!(
        "Импорт: {} · {}…",
        profile.database,
        path.display()
    ));
    let profile = profile.clone();
    let path = path.clone();
    let sender = ui.sender.clone();
    let runtime = ui.runtime.clone();
    ui.runtime.spawn_blocking(move || {
        let result = runtime.block_on(async {
            let password = resolve_profile_password(&profile, password).await?;
            native_backup::restore(&profile, &password, &path, &cancel)
        });
        let _ = sender.send(Event::RestoreFinished(profile, result));
    });
}
