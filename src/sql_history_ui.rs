use super::*;
use sql_workspace::{SqlDraft, SqlHistoryEntry};

pub(super) fn schedule_drafts(ui: &Rc<Ui>, profile: &str) {
    if ui.restoring_drafts.get() {
        return;
    }
    if let Some(timer) = ui.sql_draft_timers.borrow_mut().remove(profile) {
        timer.remove();
    }
    let weak = Rc::downgrade(ui);
    let profile = profile.to_owned();
    let key = profile.clone();
    let timer = gtk::glib::timeout_add_local_once(Duration::from_millis(750), move || {
        if let Some(ui) = weak.upgrade() {
            ui.sql_draft_timers.borrow_mut().remove(&profile);
            if let Err(error) = save_drafts(&ui, &profile) {
                ui.status.set_text(&format!("SQL не автосохранён: {error}"));
            }
        }
    });
    ui.sql_draft_timers.borrow_mut().insert(key, timer);
}

pub(super) fn save_drafts(ui: &Ui, profile: &str) -> Result<(), String> {
    save_drafts_except(ui, profile, None)
}

pub(super) fn save_drafts_except(
    ui: &Ui,
    profile: &str,
    excluded: Option<&str>,
) -> Result<(), String> {
    if ui.restoring_drafts.get() {
        return Ok(());
    }
    if let Some(timer) = ui.sql_draft_timers.borrow_mut().remove(profile) {
        timer.remove();
    }
    let mut drafts: Vec<_> = ui
        .sql_tabs
        .borrow()
        .iter()
        .filter(|(id, tab)| tab.profile_id == profile && excluded != Some(id.as_str()))
        .map(|(id, tab)| SqlDraft {
            id: id.clone(),
            profile_id: profile.to_owned(),
            connection_key: tab.session_key.clone(),
            name: tab.query_name.clone(),
            sql: query_text(tab),
        })
        .collect();
    drafts.sort_by(|a, b| a.id.cmp(&b.id));
    let mut store = ui.store.borrow_mut();
    let store = store
        .as_mut()
        .filter(|s| s.vault_is_unlocked())
        .ok_or("Разблокируйте хранилище для сохранения SQL-вкладок")?;
    // Preserve drafts belonging to an earlier endpoint of the same profile.
    let active: std::collections::HashSet<_> = ui.sessions.borrow().keys().cloned().collect();
    drafts.extend(
        store
            .sql_workspace()
            .drafts
            .iter()
            .filter(|d| d.profile_id == profile && !active.contains(&d.connection_key))
            .cloned(),
    );
    store.replace_sql_drafts(profile, drafts)?;
    Ok(())
}

pub(super) fn restore_drafts(ui: &Rc<Ui>, profile: &ConnectionProfile) {
    if ui
        .sql_tabs
        .borrow()
        .values()
        .any(|tab| tab.profile_id == profile.id)
    {
        return;
    }
    let drafts: Vec<_> = ui
        .store
        .borrow()
        .as_ref()
        .filter(|s| s.vault_is_unlocked())
        .map(|s| {
            s.sql_workspace()
                .drafts
                .iter()
                .filter(|d| {
                    d.profile_id == profile.id && d.connection_key == profile_session_key(profile)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if drafts.is_empty() {
        return;
    }
    ui.restoring_drafts.set(true);
    let session = profile_session_key(profile);
    for draft in drafts {
        let before = ui.sql_tabs.borrow().len();
        create_sql_tab(ui, &session);
        if ui.sql_tabs.borrow().len() == before {
            break;
        }
        if let Some(key) = ui.active_table_by_session.borrow().get(&session) {
            if let Some(id) = key.strip_prefix("sql::") {
                if let Some(tab) = ui.sql_tabs.borrow_mut().get_mut(id) {
                    tab.buffer.set_text(&draft.sql);
                    tab.query_name = draft.name;
                    tab.title.set_text(&tab.query_name);
                }
            }
        }
    }
    ui.restoring_drafts.set(false);
    if let Err(error) = save_drafts(ui, &profile.id) {
        ui.status.set_text(&error);
    }
}

pub(super) fn record(
    ui: &Ui,
    tab: &SqlTab,
    result: &Result<database::QueryResult, String>,
    elapsed: u128,
) {
    let Some(sql) = tab.executed_sql.as_ref() else {
        return;
    };
    let entry = SqlHistoryEntry {
        id: Uuid::new_v4().to_string(),
        profile_id: tab.profile_id.clone(),
        connection_key: tab.session_key.clone(),
        name: tab.query_name.clone(),
        sql: sql.clone(),
        executed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        duration_ms: elapsed,
        success: result.is_ok(),
        row_count: result.as_ref().map(|r| r.row_count).unwrap_or(0),
        error: result
            .as_ref()
            .err()
            .map(|e| e.chars().take(4096).collect()),
    };
    if let Some(store) = ui
        .store
        .borrow_mut()
        .as_mut()
        .filter(|s| s.vault_is_unlocked())
    {
        if let Err(error) = store.record_sql(entry) {
            ui.status
                .set_text(&format!("История SQL не сохранена: {error}"));
        }
    }
}

pub(super) fn show(ui: &Rc<Ui>) {
    let owner = ui.clone();
    ensure_vault_unlocked(ui, move || show_unlocked(&owner));
}
fn show_unlocked(ui: &Rc<Ui>) {
    let entries: Vec<_> = ui
        .store
        .borrow()
        .as_ref()
        .map(|s| {
            let mut entries: Vec<_> = s.sql_workspace().history.iter().rev().cloned().collect();
            entries.extend(
                s.sql_workspace()
                    .drafts
                    .iter()
                    .map(|draft| SqlHistoryEntry {
                        id: draft.id.clone(),
                        profile_id: draft.profile_id.clone(),
                        connection_key: draft.connection_key.clone(),
                        name: draft.name.clone(),
                        sql: draft.sql.clone(),
                        executed_at: 0,
                        duration_ms: 0,
                        success: true,
                        row_count: 0,
                        error: None,
                    }),
            );
            entries
        })
        .unwrap_or_default();
    let entries = Rc::new(entries);
    let window = adw::Window::builder()
        .title("История SQL")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(900)
        .default_height(600)
        .build();
    window.set_widget_name("sql-history");
    add_escape_to_window(&window);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some("История SQL"))));
    root.append(&header);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Поиск по запросам, именам и подключениям…")
        .build();
    search.set_widget_name("sql-history-search");
    root.append(&search);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    for entry in entries.iter() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.set_margin_start(8);
        row.set_margin_end(8);
        let profile = ui
            .store
            .borrow()
            .as_ref()
            .and_then(|s| {
                s.profiles
                    .iter()
                    .find(|p| p.id == entry.profile_id)
                    .map(|p| p.name.clone())
            })
            .unwrap_or_else(|| entry.profile_id.clone());
        let date = if entry.executed_at == 0 {
            "Черновик".to_owned()
        } else {
            gtk::glib::DateTime::from_unix_local(entry.executed_at as i64)
                .and_then(|t| t.format("%d.%m %H:%M"))
                .map(|s| s.to_string())
                .unwrap_or_default()
        };
        let icon = gtk::Image::from_icon_name(if entry.success {
            "emblem-ok-symbolic"
        } else {
            "dialog-error-symbolic"
        });
        icon.add_css_class(if entry.success {
            "status-success"
        } else {
            "status-error"
        });
        row.append(&icon);
        let sql = entry
            .sql
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("--"))
            .unwrap_or(&entry.sql);
        let label = gtk::Label::new(Some(&format!(
            "{date} · {profile} · {} · {sql}",
            entry.name
        )));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        row.append(&label);
        row.append(&gtk::Label::new(Some(&format!("{} мс", entry.duration_ms))));
        list.append(&row);
    }
    let buffer = sourceview5::Buffer::new(None);
    buffer.set_language(sourceview5::LanguageManager::new().language("sql").as_ref());
    apply_sql_style_scheme(
        &buffer,
        &theme::sql_style_schemes(),
        ui.active_theme.get(),
        adw::StyleManager::default().is_dark(),
    );
    let preview = sourceview5::View::with_buffer(&buffer);
    preview.set_widget_name("sql-history-preview");
    preview.set_editable(false);
    preview.set_monospace(true);
    preview.set_show_line_numbers(true);
    let status = gtk::Label::new(Some(if entries.is_empty() {
        "История пуста. Запросы сохраняются в зашифрованном хранилище."
    } else {
        "Enter / Ctrl+Enter — открыть выбранный запрос без выполнения. Ctrl+Shift+C — скопировать SQL."
    }));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.set_selectable(true);
    let split = gtk::Paned::new(gtk::Orientation::Vertical);
    split.set_vexpand(true);
    split.set_start_child(Some(
        &gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .build(),
    ));
    split.set_end_child(Some(
        &gtk::ScrolledWindow::builder()
            .child(&preview)
            .vexpand(true)
            .build(),
    ));
    split.set_position(240);
    root.append(&split);
    root.append(&status);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.set_halign(gtk::Align::End);
    let clear = gtk::Button::with_label("Очистить всю историю…");
    let remove = gtk::Button::with_label("Удалить черновик");
    remove.set_widget_name("sql-draft-remove");
    remove.set_tooltip_text(Some("Удалить выбранный сохранённый черновик (Ctrl+Shift+Backspace). Открытые вкладки сохранятся."));
    remove.set_sensitive(false);
    let copy = gtk::Button::with_label("Копировать SQL");
    let open = gtk::Button::with_label("Открыть в SQL");
    open.add_css_class("suggested-action");
    for button in [&clear, &remove, &copy, &open] {
        actions.append(button);
    }
    copy.set_sensitive(!entries.is_empty());
    open.set_sensitive(!entries.is_empty());
    clear.set_sensitive(!entries.is_empty());
    root.append(&actions);
    window.set_content(Some(&root));
    shortcuts::button(&window, &copy, "<Control><Shift>c");
    shortcuts::button(&window, &open, "<Control>Return");
    shortcuts::button(&window, &clear, "<Control><Shift>Delete");
    shortcuts::button(&window, &remove, "<Control><Shift>BackSpace");
    // Row kind is determined by its section, never by an ID shared with history.
    let history_count = ui
        .store
        .borrow()
        .as_ref()
        .map(|s| s.sql_workspace().history.len())
        .unwrap_or(0);
    let weak = Rc::downgrade(ui);
    let selected = list.clone();
    let values = entries.clone();
    let weak_window = window.downgrade();
    remove.connect_clicked(move |_| {
        let Some(entry) = selected
            .selected_row()
            .and_then(|r| values.get(r.index() as usize))
        else {
            return;
        };
        if selected
            .selected_row()
            .is_none_or(|r| (r.index() as usize) < history_count)
        {
            return;
        }
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let result = ui
            .store
            .borrow_mut()
            .as_mut()
            .ok_or("Хранилище недоступно".into())
            .and_then(|store| store.remove_sql_draft(&entry.id));
        match result {
            Ok(()) => {
                if let Some(window) = weak_window.upgrade() {
                    window.close();
                }
                show_unlocked(&ui);
            }
            Err(error) => show_error_dialog(&ui, "Черновик не удалён", &error),
        }
    });
    let remove_button = remove.clone();
    let values = entries.clone();
    let text = buffer.clone();
    let detail = status.clone();
    let open_button = open.clone();
    let copy_button = copy.clone();
    list.connect_row_selected(move |_, row| {
        let entry = row.and_then(|r| values.get(r.index() as usize));
        remove_button.set_sensitive(row.is_some_and(|r| (r.index() as usize) >= history_count));
        open_button.set_sensitive(entry.is_some());
        copy_button.set_sensitive(entry.is_some());
        if let Some(entry) = entry {
            text.set_text(&entry.sql);
            detail.set_text(
                &entry
                    .error
                    .as_ref()
                    .map(|e| format!("Ошибка · {} мс · {e}", entry.duration_ms))
                    .unwrap_or_else(|| {
                        format!(
                            "{} строк · {} мс · Ctrl+Enter — открыть без выполнения",
                            entry.row_count, entry.duration_ms
                        )
                    }),
            );
        } else {
            text.set_text("");
        }
    });
    let filtered = list.clone();
    let values = entries.clone();
    let weak_ui = Rc::downgrade(ui);
    search.connect_search_changed(move |search| {
        let query = search.text().to_lowercase();
        let mut first = None;
        let profiles: HashMap<_, _> = weak_ui
            .upgrade()
            .and_then(|ui| {
                ui.store.borrow().as_ref().map(|s| {
                    s.profiles
                        .iter()
                        .map(|p| (p.id.clone(), p.name.to_lowercase()))
                        .collect()
                })
            })
            .unwrap_or_default();
        for (index, entry) in values.iter().enumerate() {
            if let Some(row) = filtered.row_at_index(index as i32) {
                row.set_visible(
                    entry.sql.to_lowercase().contains(&query)
                        || entry.name.to_lowercase().contains(&query)
                        || profiles
                            .get(&entry.profile_id)
                            .is_some_and(|p| p.contains(&query)),
                );
                if row.is_visible() && first.is_none() {
                    first = Some(row);
                }
            }
        }
        filtered.select_row(first.as_ref());
    });
    let text = buffer.clone();
    copy.connect_clicked(move |button| {
        button
            .clipboard()
            .set_text(&text.text(&text.start_iter(), &text.end_iter(), true))
    });
    let weak = Rc::downgrade(ui);
    let values = entries.clone();
    let selected = list.clone();
    let weak_window = window.downgrade();
    let detail = status.clone();
    open.connect_clicked(move |_| {
        let Some(entry) = selected.selected_row().and_then(|r| values.get(r.index() as usize)) else { return };
        let Some(ui) = weak.upgrade() else { return };
        let session = ui.sessions.borrow().iter().find(|(_, (profile, _))| profile.id == entry.profile_id && profile_session_key(profile) == entry.connection_key).map(|(key, _)| key.clone());
        let Some(session) = session else { detail.set_text("Сначала подключите исходную БД с теми же параметрами подключения. Запрос можно скопировать через Ctrl+Shift+C."); return };
        select_session(&ui, &session);
        let before = ui.sql_tabs.borrow().len();
        create_sql_tab(&ui, &session);
        if ui.sql_tabs.borrow().len() == before { return; }
        if let Some(key) = ui.active_table_by_session.borrow().get(&session) { if let Some(id) = key.strip_prefix("sql::") { if let Some(tab) = ui.sql_tabs.borrow_mut().get_mut(id) { tab.buffer.set_text(&entry.sql); tab.query_name = entry.name.clone(); tab.title.set_text(&tab.query_name); } } }
        if let Some(window) = weak_window.upgrade() { window.close(); }
    });
    let open_selected = open.clone();
    search.connect_activate(move |_| {
        if open_selected.is_sensitive() {
            open_selected.emit_clicked();
        }
    });
    let open_selected = open.clone();
    list.connect_row_activated(move |_, _| {
        if open_selected.is_sensitive() {
            open_selected.emit_clicked();
        }
    });
    let weak = Rc::downgrade(ui);
    let weak_window = window.downgrade();
    clear.connect_clicked(move |_| {
        let Some(parent) = weak_window.upgrade() else {
            return;
        };
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&parent)
            .modal(true)
            .buttons(gtk::ButtonsType::YesNo)
            .message_type(gtk::MessageType::Question)
            .text("Очистить историю SQL всех подключений?")
            .secondary_text("Сохранённые SQL-файлы и открытые вкладки сохранятся.")
            .build();
        let weak = weak.clone();
        let weak_window = weak_window.clone();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response != gtk::ResponseType::Yes {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                let result = ui
                    .store
                    .borrow_mut()
                    .as_mut()
                    .ok_or_else(|| "Хранилище недоступно".to_owned())
                    .and_then(SettingsStore::clear_sql_history);
                match result {
                    Ok(()) => {
                        if let Some(window) = weak_window.upgrade() {
                            window.close();
                        }
                        show_unlocked(&ui);
                    }
                    Err(error) => show_error_dialog(&ui, "История не очищена", &error),
                }
            }
        });
        dialogs::install(&dialog);
        dialog.present();
    });
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let focused_list = list.clone();
    let focused_search = search.clone();
    let weak = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if modifiers == gtk::gdk::ModifierType::CONTROL_MASK && key == gtk::gdk::Key::l {
            focused_search.grab_focus();
            return gtk::glib::Propagation::Stop;
        }
        if modifiers == gtk::gdk::ModifierType::CONTROL_MASK && key == gtk::gdk::Key::w {
            if let Some(window) = weak.upgrade() {
                window.close();
            }
            return gtk::glib::Propagation::Stop;
        }
        let search_focused = weak
            .upgrade()
            .and_then(|window| gtk::prelude::RootExt::focus(&window))
            .is_some_and(|focus| {
                focus == focused_search.clone().upcast::<gtk::Widget>()
                    || focus.is_ancestor(&focused_search)
            });
        if key == gtk::gdk::Key::Down && modifiers.is_empty() && search_focused {
            if let Some(row) = focused_list.selected_row() {
                row.grab_focus();
            }
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    window.add_controller(keys);
    list.select_row(list.row_at_index(0).as_ref());
    window.present();
    search.grab_focus();
}
