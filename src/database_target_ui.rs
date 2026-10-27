//! Explicit database choice for connection-level operations.
use super::*;

pub(super) fn password(ui: &Ui, profile: &ConnectionProfile) -> Result<String, String> {
    if profile.gopass_entry.is_some() {
        Ok(String::new())
    } else {
        ui.store
            .borrow()
            .as_ref()
            .ok_or("Хранилище недоступно".into())
            .and_then(|store| store.password(&profile.id))
    }
}

pub(super) fn choose(
    ui: &Rc<Ui>,
    profile: &ConnectionProfile,
    action: impl Fn(&Rc<Ui>, ConnectionProfile) + 'static,
) {
    let owner = ui.clone();
    let profile = profile.clone();
    ensure_vault_unlocked(ui, move || show(&owner, profile, action, false));
}

pub(super) fn navigate(
    ui: &Rc<Ui>,
    profile: &ConnectionProfile,
    action: impl Fn(&Rc<Ui>, ConnectionProfile) + 'static,
) {
    let owner = ui.clone();
    let profile = profile.clone();
    ensure_vault_unlocked(ui, move || show(&owner, profile, action, true));
}

fn show(
    ui: &Rc<Ui>,
    profile: ConnectionProfile,
    action: impl Fn(&Rc<Ui>, ConnectionProfile) + 'static,
    auto_single: bool,
) {
    let action = Rc::new(action);
    let secret = match password(ui, &profile) {
        Ok(secret) => secret,
        Err(error) => {
            show_error_dialog(ui, "Не удалось получить список БД", &error);
            return;
        }
    };
    let window = adw::Window::builder()
        .title("Выберите базу данных")
        .transient_for(&ui.window)
        .default_width(600)
        .default_height(440)
        .build();
    window.set_widget_name("database-target-picker");
    add_escape_to_window(&window);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.append(&adw::HeaderBar::new());
    let description = gtk::Label::new(Some(&format!(
        "{} · {}:{} · {}",
        profile.name, profile.host, profile.port, profile.username
    )));
    description.set_wrap(true);
    root.append(&description);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Найти базу данных"));
    root.append(&search);
    let status = gtk::Label::new(Some("Загрузка списка баз…"));
    status.set_wrap(true);
    root.append(&status);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list)
        .build();
    root.append(&scroll);
    let accept = gtk::Button::with_label("Выбрать БД");
    accept.add_css_class("suggested-action");
    accept.set_sensitive(false);
    root.append(&accept);
    window.set_content(Some(&root));
    let rows = Rc::new(RefCell::new(Vec::<String>::new()));
    let query = search.downgrade();
    list.set_filter_func(move |row| {
        query.upgrade().is_none_or(|query| {
            row.widget_name()
                .to_lowercase()
                .contains(&query.text().to_lowercase())
        })
    });
    let filtered_list = list.downgrade();
    search.connect_search_changed(move |_| {
        if let Some(list) = filtered_list.upgrade() {
            list.invalidate_filter();
            list.unselect_all();
        }
    });
    let button = accept.downgrade();
    list.connect_row_selected(move |_, row| {
        if let Some(button) = button.upgrade() {
            button.set_sensitive(row.is_some());
        }
    });
    let button = accept.downgrade();
    list.connect_row_activated(move |_, _| {
        if let Some(button) = button.upgrade() {
            button.emit_clicked();
        }
    });
    let button = accept.downgrade();
    search.connect_activate(move |_| {
        if let Some(button) = button.upgrade().filter(|b| b.is_sensitive()) {
            button.emit_clicked();
        }
    });
    let weak = window.downgrade();
    let owner = Rc::downgrade(ui);
    let selected_rows = rows.clone();
    let selected_list = list.downgrade();
    let selected_profile = profile.clone();
    let selected_action = action.clone();
    accept.connect_clicked(move |_| {
        let Some(row) = selected_list
            .upgrade()
            .and_then(|list| list.selected_row())
            .filter(|r| r.is_child_visible())
        else {
            return;
        };
        let Some(name) = selected_rows.borrow().get(row.index() as usize).cloned() else {
            return;
        };
        if let (Some(ui), Some(window)) = (owner.upgrade(), weak.upgrade()) {
            let mut profile = selected_profile.clone();
            profile.database = name;
            window.close();
            selected_action(&ui, profile);
        }
    });
    let (sender, receiver) = mpsc::channel();
    let navigate_profile = profile.clone();
    ui.runtime.spawn(async move {
        let result = async {
            let secret = resolve_profile_password(&profile, secret).await?;
            // A restricted account may connect only to its configured database.
            DbPool::profile_database_targets(&profile, &secret).await
        }
        .await;
        let _ = sender.send(result);
    });
    let weak = window.downgrade();
    let owner = Rc::downgrade(ui);
    gtk::glib::timeout_add_local(Duration::from_millis(40), move || {
        let Some(window) = weak.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        if !window.is_visible() {
            return gtk::glib::ControlFlow::Break;
        }
        match receiver.try_recv() {
            Ok(Ok(mut databases)) => {
                if auto_single && databases.len() == 1 {
                    let mut target = navigate_profile.clone();
                    target.database = databases[0].0.clone();
                    window.close();
                    if let Some(ui) = owner.upgrade() {
                        action(&ui, target);
                    }
                    return gtk::glib::ControlFlow::Break;
                }
                let truncated = databases.len() > 1000;
                databases.truncate(1000);
                status.set_text(if databases.is_empty() {
                    "Нет доступных БД"
                } else if truncated {
                    "Показаны первые 1000 БД. Для остальных укажите имя БД в отдельном подключении."
                } else {
                    "Выберите целевую БД; Enter — продолжить"
                });
                for (name, info) in databases {
                    let row = gtk::ListBoxRow::new();
                    row.set_widget_name(&name);
                    let content = gtk::Box::new(gtk::Orientation::Vertical, 3);
                    content.set_margin_start(8);
                    content.set_margin_end(8);
                    content.set_margin_top(5);
                    content.set_margin_bottom(5);
                    let title = gtk::Label::new(Some(&name));
                    title.set_xalign(0.0);
                    title.add_css_class("heading");
                    let details = gtk::Label::new(Some(&info));
                    details.set_xalign(0.0);
                    details.set_wrap(true);
                    details.add_css_class("dim-label");
                    content.append(&title);
                    content.append(&details);
                    row.set_child(Some(&content));
                    rows.borrow_mut().push(name);
                    list.append(&row);
                }
                list.unselect_all();
                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                status.set_text(&error);
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                status.set_text("Не удалось загрузить список БД");
                gtk::glib::ControlFlow::Break
            }
            Err(_) => gtk::glib::ControlFlow::Continue,
        }
    });
    window.present();
    search.grab_focus();
}
