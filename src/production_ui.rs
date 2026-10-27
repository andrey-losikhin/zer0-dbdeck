//! One-operation production permission. Cancel/close never authorizes a write.
use super::*;
pub(super) fn confirm(
    ui: &Rc<Ui>,
    profile: &ConnectionProfile,
    title: &str,
    details: &str,
    action: impl Fn() + 'static,
) {
    let window = adw::Window::builder()
        .title("Подтверждение PROD")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(620)
        .default_height(420)
        .build();
    window.set_widget_name("production-confirmation");
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
        "PROD · {}\n{} · {}:{} · пользователь {}",
        profile.name, profile.database, profile.host, profile.port, profile.username
    )));
    target.set_wrap(true);
    target.set_selectable(true);
    root.append(&target);
    let text = gtk::TextView::new();
    text.set_editable(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::WordChar);
    text.buffer()
        .set_text(&details.chars().take(12000).collect::<String>());
    if details.chars().count() > 12000 {
        let note = gtk::Label::new(Some(
            "Показаны первые 12 000 символов. Операция выполнится целиком; перед подтверждением проверьте полный текст в редакторе.",
        ));
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
    let entry = gtk::Entry::new();
    entry.set_widget_name("production-target-name");
    entry.set_placeholder_text(Some("Введите точное имя целевой БД"));
    root.append(&entry);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Отмена");
    let accept = gtk::Button::with_label("Подтвердить операцию");
    accept.set_widget_name("production-accept");
    accept.add_css_class("destructive-action");
    accept.set_sensitive(false);
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
    let weak = window.downgrade();
    let entry_read = entry.downgrade();
    let expected_click = profile.database.clone();
    let used = Cell::new(false);
    accept.connect_clicked(move |button| {
        if !button.is_sensitive()
            || entry_read
                .upgrade()
                .is_none_or(|e| e.text().as_str() != expected_click)
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
    entry.grab_focus();
}
