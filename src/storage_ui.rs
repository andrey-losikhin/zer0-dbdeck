use super::*;

pub(super) fn install(ui: &Rc<Ui>, import_button: &gtk::Button) {
    update(ui);
    let weak = Rc::downgrade(ui);
    ui.unlock_provider.connect_changed(move |selector| {
        let Some(ui) = weak.upgrade() else { return };
        let provider = if selector.active_id().as_deref() == Some("system") {
            secret_store::UnlockProvider::System
        } else {
            secret_store::UnlockProvider::Gopass
        };
        if provider != secret_store::unlock_provider() {
            if let Err(error) = secret_store::set_unlock_provider(provider) {
                show_error_dialog(&ui, "Менеджер ключа не изменён", &error);
                update(&ui);
            }
        }
    });
    let weak = Rc::downgrade(ui);
    ui.remember_unlock_button.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        if ui.keyring_busy.get() {
            return;
        }
        if secret_store::remember_enabled() {
            // Disable lookup first even if a locked system wallet refuses deletion.
            if let Err(error) = secret_store::set_remember_enabled(false) {
                show_error_dialog(&ui, "Настройка не сохранена", &error);
                return;
            }
            let id = ui
                .store
                .borrow()
                .as_ref()
                .and_then(|s| s.keyring_id().ok())
                .and_then(|id| secret_store::scoped_key_id(&id, false).ok());
            update(&ui);
            if let Some(id) = id {
                ui.keyring_busy.set(true);
                ui.remember_unlock_button.set_sensitive(false);
                let sender = ui.sender.clone();
                ui.unlock_provider.set_sensitive(false);
                let provider = secret_store::unlock_provider();
                ui.runtime.spawn_blocking(move || {
                    let result = secret_store::Cli::default().forget_key(&id, provider);
                    let _ = sender.send(Event::KeyringChanged(false, result));
                });
            }
        } else {
            let after = ui.clone();
            ensure_vault_unlocked(&ui, move || remember(&after));
        }
    });
    let weak = Rc::downgrade(ui);
    import_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let after = ui.clone();
            ensure_vault_unlocked(&ui, move || import_dialog(&after));
        }
    });
    // Do not show an empty connection list while a portable file is still locked.
    let exists = ui
        .store
        .borrow()
        .as_ref()
        .is_some_and(|s| s.vault_exists() && !s.vault_is_unlocked());
    if exists {
        let weak = Rc::downgrade(ui);
        gtk::glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                ensure_vault_unlocked(&ui, || {});
            }
        });
    }
}
fn update(ui: &Ui) {
    ui.remember_unlock_button
        .set_label(if secret_store::remember_enabled() {
            "Отключить и забыть ключ"
        } else {
            "Включить на этом компьютере…"
        });
    ui.remember_unlock_button
        .set_sensitive(!ui.keyring_busy.get() && !ui.unlock_busy.get());
    ui.unlock_provider
        .set_active_id(Some(secret_store::unlock_provider().id()));
    ui.unlock_provider.set_sensitive(
        !secret_store::remember_enabled() && !ui.unlock_busy.get() && !ui.keyring_busy.get(),
    );
    if let Some(store) = ui.store.borrow().as_ref() {
        let path = store.storage_path().display().to_string();
        ui.storage_path_label.set_text(&path);
        ui.storage_path_label.set_tooltip_text(Some(&path));
        ui.storage_path_label.set_max_width_chars(26);
        ui.storage_path_label
            .set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    }
}
pub(super) fn unlocked(ui: &Rc<Ui>) {
    *ui.sql_queries_directory.borrow_mut() = ui
        .store
        .borrow()
        .as_ref()
        .and_then(|s| s.sql_queries_directory());
    refresh_profiles(ui);
    update(ui);
}
pub(super) fn auto_unlock<F: FnOnce() + 'static>(ui: &Rc<Ui>, after: F) {
    if ui.unlock_busy.get() {
        ui.status.set_text("Хранилище уже разблокируется…");
        return;
    }
    let id = ui
        .store
        .borrow()
        .as_ref()
        .and_then(|s| s.keyring_id().ok())
        .and_then(|id| secret_store::scoped_key_id(&id, false).ok());
    let Some(id) = id else {
        let exists = ui.store.borrow().as_ref().is_some_and(|s| s.vault_exists());
        show_vault_dialog(ui, exists, after);
        return;
    };
    ui.unlock_busy.set(true);
    *ui.after_auto_unlock.borrow_mut() = Some(Box::new(after));
    ui.status
        .set_text("Разблокирую через выбранный менеджер ключей…");
    let sender = ui.sender.clone();
    ui.unlock_provider.set_sensitive(false);
    let provider = secret_store::unlock_provider();
    ui.runtime.spawn_blocking(move || {
        let result = secret_store::Cli::default().lookup_key(&id, provider);
        let _ = sender.send(Event::VaultAutoUnlock(result));
    });
}
pub(super) fn auto_unlock_finished(ui: &Rc<Ui>, result: Result<secret_store::Secret, String>) {
    ui.unlock_busy.set(false);
    let opened = match result {
        Ok(key) => ui
            .store
            .borrow_mut()
            .as_mut()
            .ok_or("Хранилище недоступно".to_owned())
            .and_then(|s| s.unlock_with_key(&key.0)),
        Err(error) => Err(error),
    };
    let after = ui.after_auto_unlock.borrow_mut().take();
    if opened.is_ok() {
        unlocked(ui);
        ui.status
            .set_text("Хранилище разблокировано менеджером ключей");
        if let Some(after) = after {
            after();
        }
    } else {
        ui.status
            .set_text("Автоматическая разблокировка недоступна. Введите мастер-пароль");
        let exists = ui.store.borrow().as_ref().is_some_and(|s| s.vault_exists());
        show_vault_dialog(ui, exists, move || {
            if let Some(after) = after {
                after();
            }
        });
    }
}
pub(super) fn remember(ui: &Rc<Ui>) {
    if ui.keyring_busy.get() {
        return;
    }
    let material = ui
        .store
        .borrow()
        .as_ref()
        .ok_or("Хранилище недоступно".to_owned())
        .and_then(|s| {
            Ok((
                secret_store::scoped_key_id(&s.keyring_id()?, true)?,
                s.unlock_key()?,
            ))
        });
    let (id, key) = match material {
        Ok(m) => m,
        Err(error) => {
            show_error_dialog(ui, "Не удалось запомнить ключ", &error);
            return;
        }
    };
    ui.keyring_busy.set(true);
    ui.remember_unlock_button.set_sensitive(false);
    let sender = ui.sender.clone();
    ui.unlock_provider.set_sensitive(false);
    let provider = secret_store::unlock_provider();
    ui.runtime.spawn_blocking(move || {
        let result = secret_store::Cli::default().remember_key(&id, &key, provider);
        let _ = sender.send(Event::KeyringChanged(true, result));
    });
}
pub(super) fn keyring_finished(ui: &Rc<Ui>, enabled: bool, result: Result<(), String>) {
    ui.keyring_busy.set(false);
    let result = result.and_then(|()| secret_store::set_remember_enabled(enabled));
    update(ui);
    match result {
        Ok(()) => ui.status.set_text(if enabled {
            "Автоматическая разблокировка включена на этом компьютере"
        } else {
            "Автоматическая разблокировка отключена"
        }),
        Err(error) => show_error_dialog(
            ui,
            "Не удалось изменить менеджер ключей",
            &format!("{error}. Зашифрованный файл остаётся доступен по мастер-паролю."),
        ),
    }
}
fn import_dialog(ui: &Rc<Ui>) {
    let dialog = adw::Window::builder()
        .title("Подключение из gopass")
        .transient_for(&settings_dialog_parent(ui))
        .modal(true)
        .default_width(500)
        .build();
    dialog.add_css_class("dbdeck-settings");
    add_escape_to_window(&dialog);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    let description = gtk::Label::new(Some(
        "Путь к записи с полями Engine, Host, Port, Database, Username и Password из zer0-gopass-tui. Параметры будут импортированы, пароль останется в gopass. Для подключения нужен gopass, GPG-ключ и разблокированный GPG-agent.",
    ));
    description.set_wrap(true);
    description.set_xalign(0.0);
    content.append(&description);
    let entry = gtk::Entry::builder()
        .placeholder_text("databases/production")
        .hexpand(true)
        .build();
    content.append(&entry);
    let import = gtk::Button::with_label("Импортировать параметры");
    import.add_css_class("suggested-action");
    content.append(&import);
    dialog.set_content(Some(&content));
    shortcuts::default_button(&dialog, &import);
    let weak = Rc::downgrade(ui);
    let close = dialog.downgrade();
    import.connect_clicked(move |button| {
        let Some(ui) = weak.upgrade() else { return };
        let path = entry.text().trim().to_owned();
        if let Err(error) = secret_store::validate_entry(&path) {
            show_error_dialog(&ui, "Проверьте путь", &error);
            return;
        }
        button.set_sensitive(false);
        ui.status.set_text("Читаю параметры подключения из gopass…");
        let sender = ui.sender.clone();
        ui.runtime.spawn_blocking(move || {
            let result = secret_store::Cli::default().import_connection(&path);
            let _ = sender.send(Event::GopassImported(result));
        });
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    dialog.present();
}
pub(super) fn import_finished(ui: &Rc<Ui>, result: Result<ConnectionProfile, String>) {
    let result = result.and_then(|profile| {
        let reference = profile.gopass_entry.clone();
        if ui
            .store
            .borrow()
            .as_ref()
            .is_some_and(|s| s.profiles.iter().any(|p| p.gopass_entry == reference))
        {
            return Err("Эта запись gopass уже привязана к подключению".to_owned());
        }
        ui.store
            .borrow_mut()
            .as_mut()
            .ok_or("Хранилище недоступно".to_owned())?
            .save_profile(profile, "")
            .map_err(|e| match e {
                ProfileSaveError::Secret(e) | ProfileSaveError::Settings(e) => e,
            })
    });
    match result {
        Ok(()) => {
            refresh_profiles(ui);
            ui.status.set_text(
                "Подключение импортировано. Проверьте параметры перед первым подключением",
            );
        }
        Err(error) => show_error_dialog(ui, "Импорт из gopass не выполнен", &error),
    }
}
