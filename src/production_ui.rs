//! One-operation production permission. Cancel/close never authorizes a write.
use super::*;
pub(super) fn confirm(
    ui: &Rc<Ui>,
    profile: &ConnectionProfile,
    title: &str,
    details: &str,
    action: impl Fn() + 'static,
) {
    let mut chars = details.chars();
    let excerpt = chars.by_ref().take(12000).collect::<String>();
    let note = chars.next().map(|_| {
        "Показаны первые 12 000 символов. Операция выполнится целиком; перед подтверждением проверьте полный текст в редакторе."
    });
    show_confirmation(ui, profile, title, &excerpt, note, false, action);
}

pub(super) fn confirm_table_edits(
    ui: &Rc<Ui>,
    profile: &ConnectionProfile,
    details: &str,
    action: impl Fn() + 'static,
) {
    show_confirmation(
        ui,
        profile,
        "Проверьте изменения таблицы",
        details,
        None,
        true,
        action,
    );
}

fn show_confirmation(
    ui: &Rc<Ui>,
    profile: &ConnectionProfile,
    title: &str,
    details: &str,
    note: Option<&str>,
    table_edits: bool,
    action: impl Fn() + 'static,
) {
    let production = !table_edits || matches!(profile.environment, EnvironmentTag::Prod);
    let window = adw::Window::builder()
        .title(if table_edits {
            "Предпросмотр правок"
        } else {
            "Подтверждение PROD"
        })
        .transient_for(&ui.window)
        .modal(true)
        .default_width(620)
        .default_height(420)
        .build();
    window.set_widget_name(if table_edits && !production {
        "table-edit-preview"
    } else {
        "production-confirmation"
    });
    add_escape_to_window(&window);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_bottom(12);
    root.append(&adw::HeaderBar::new());
    let heading = gtk::Label::new(Some(title));
    heading.set_wrap(true);
    heading.add_css_class("heading");
    root.append(&heading);
    let target = gtk::Label::new(Some(&format!(
        "{}{}\n{} · {}:{} · пользователь {}",
        if production { "PROD · " } else { "" },
        profile.name,
        profile.database,
        profile.host,
        profile.port,
        profile.username
    )));
    target.set_wrap(true);
    target.set_selectable(true);
    root.append(&target);
    let text = gtk::TextView::new();
    text.set_editable(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::WordChar);
    text.set_widget_name("confirmation-details");
    text.buffer().set_text(details);
    if let Some(note) = note {
        let note = gtk::Label::new(Some(note));
        note.set_wrap(true);
        note.add_css_class("warning");
        root.append(&note);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .min_content_height(140)
        .child(&text)
        .build();
    root.append(&scroll);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Отмена");
    let accept = gtk::Button::with_label(if table_edits {
        "Применить изменения"
    } else {
        "Подтвердить операцию"
    });
    accept.set_widget_name(if production {
        "production-accept"
    } else {
        "table-edit-accept"
    });
    accept.add_css_class(if production {
        "destructive-action"
    } else {
        "suggested-action"
    });
    let entry = production.then(|| {
        let entry = gtk::Entry::new();
        entry.set_widget_name("production-target-name");
        entry.set_placeholder_text(Some("Введите точное имя целевой БД"));
        root.append(&entry);
        accept.set_sensitive(false);
        let expected = profile.database.clone();
        let button = accept.downgrade();
        entry.connect_changed(move |entry| {
            if let Some(button) = button.upgrade() {
                button.set_sensitive(!expected.is_empty() && entry.text().as_str() == expected);
            }
        });
        let button = accept.downgrade();
        entry.connect_activate(move |_| {
            if let Some(button) = button.upgrade().filter(|b| b.is_sensitive()) {
                button.emit_clicked();
            }
        });
        entry
    });
    buttons.append(&cancel);
    buttons.append(&accept);
    root.append(&buttons);
    window.set_content(Some(&root));
    let weak = window.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(w) = weak.upgrade() {
            w.close();
        }
    });
    let weak = window.downgrade();
    let entry_read = entry.as_ref().map(gtk::Entry::downgrade);
    let expected_click = profile.database.clone();
    let used = Cell::new(false);
    accept.connect_clicked(move |button| {
        if !button.is_sensitive()
            || entry_read.as_ref().is_some_and(|entry| {
                expected_click.is_empty()
                    || entry
                        .upgrade()
                        .is_none_or(|e| e.text().as_str() != expected_click)
            })
        {
            return;
        }
        if used.replace(true) {
            return;
        }
        if let Some(w) = weak.upgrade().filter(|w| w.is_visible()) {
            w.close();
            action();
        }
    });
    window.present();
    if let Some(entry) = entry {
        entry.grab_focus();
    } else {
        cancel.grab_focus();
    }
}
