use crate::theme::ThemeMode;
use adw::prelude::*;
use relm4::{adw, gtk};

pub struct PreferencesUi {
    pub window: adw::Window,
    pub export_backup: gtk::Button,
    pub import_backup: gtk::Button,
    pub theme_selector: gtk::ComboBoxText,
    pub remember_unlock: gtk::Button,
    pub unlock_provider: gtk::ComboBoxText,
    pub import_gopass: gtk::Button,
    pub storage_path: gtk::Label,
    pub sql_directory: gtk::Button,
    pub sql_directory_label: gtk::Label,
    pub csv_directory: gtk::Button,
    pub csv_directory_label: gtk::Label,
}

fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class(class);
    label
}

fn page(title: &str, description: &str) -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.set_margin_start(16);
    page.set_margin_end(16);
    page.set_margin_top(16);
    page.set_margin_bottom(16);
    page.append(&label(title, "settings-heading"));
    page.append(&label(description, "settings-description"));
    page
}

fn row(title: &str, description: &str, icon: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("settings-row");
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(18);
    row.append(&image);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
    text.set_hexpand(true);
    text.append(&label(title, "settings-row-title"));
    text.append(&label(description, "settings-description"));
    row.append(&text);
    control.set_valign(gtk::Align::Center);
    row.append(control);
    row
}

fn update_preview(preview: &gtk::Box, mode: ThemeMode) {
    while let Some(child) = preview.first_child() {
        preview.remove(&child);
    }
    for class in ["tokyo-night", "mocha", "graphite", "system"] {
        preview.remove_css_class(class);
    }
    let (class, name, description) = match mode {
        ThemeMode::TokyoNight => (
            "tokyo-night",
            "Tokyo Night",
            "Тёмный индиго, синий акцент и контрастный текст.",
        ),
        ThemeMode::CatppuccinMocha => (
            "mocha",
            "Catppuccin Mocha",
            "Мягкая тёмная палитра с пастельными акцентами.",
        ),
        ThemeMode::Dark => (
            "graphite",
            "Сине-графитовая",
            "Холодный графит и бирюзовый акцент.",
        ),
        ThemeMode::System => (
            "system",
            "Системная",
            "Светлый или тёмный режим и акцент рабочего стола.",
        ),
    };
    let colors = crate::theme::preview_colors(mode);
    preview.add_css_class(class);
    preview.append(&label(name, "settings-row-title"));
    preview.append(&label(description, "settings-description"));
    let swatches = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    for color in &colors {
        let swatch = gtk::Label::new(None);
        swatch.set_markup(&format!(
            "<span foreground=\"{color}\" size=\"large\">●</span>"
        ));
        swatch.set_tooltip_text(Some(color));
        swatches.append(&swatch);
    }
    preview.append(&swatches);
    let sample = gtk::Label::new(None);
    sample.set_xalign(0.0);
    sample.set_markup(&format!("<span font_family=\"monospace\" foreground=\"{}\"><span foreground=\"{}\">SELECT</span> name, amount\n<span foreground=\"{}\">FROM</span> orders\n<span foreground=\"{}\">WHERE</span> status = <span foreground=\"{}\">'active'</span> <span foreground=\"{}\">AND</span> amount &gt; <span foreground=\"{}\">100</span>;</span>", colors[0], colors[1], colors[1], colors[1], colors[2], colors[1], colors[3]));
    preview.append(&sample);
    if mode == ThemeMode::TokyoNight {
        let source = gtk::LinkButton::with_label(
            "https://github.com/tokyo-night/tokyo-night-vscode-theme#color-palette",
            "Палитра и руководство Tokyo Night ↗",
        );
        source.set_halign(gtk::Align::Start);
        preview.append(&source);
    }
}

pub fn build(parent: &adw::ApplicationWindow) -> PreferencesUi {
    let window = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Настройки DBDeck")
        .default_width(720)
        .default_height(510)
        .build();
    window.add_css_class("dbdeck-settings");
    window.connect_close_request(|window| {
        window.set_visible(false);
        gtk::glib::Propagation::Stop
    });
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some("Настройки DBDeck"))));
    root.append(&header);
    let pages = gtk::Stack::new();
    pages.set_vexpand(true);
    pages.set_hhomogeneous(false);
    pages.set_vhomogeneous(false);
    pages.set_widget_name("settings-pages");
    let tabs = gtk::StackSwitcher::new();
    tabs.set_stack(Some(&pages));
    tabs.set_halign(gtk::Align::Start);
    tabs.set_margin_start(16);
    tabs.set_margin_top(8);
    tabs.add_css_class("settings-tabs");
    root.append(&tabs);
    let general = page(
        "Подключения и резервные копии",
        "Переносите подключения между устройствами в зашифрованном файле.",
    );
    let export_backup = gtk::Button::with_label("Экспортировать…");
    let import_backup = gtk::Button::with_label("Импортировать…");
    let storage_path = label("", "settings-description");
    storage_path.set_selectable(true);
    storage_path.set_xalign(0.0);
    general.append(&row("Файл подключений", "Профили и локальные пароли зашифрованы вместе. Для переноса скопируйте этот файл и восстановите его на другом компьютере.", "folder-symbolic", &storage_path));
    let unlock_provider = gtk::ComboBoxText::new();
    unlock_provider.set_widget_name("unlock-provider");
    unlock_provider.append(Some("gopass"), "gopass / GPG-agent");
    unlock_provider.append(Some("system"), "Системная связка ключей");
    general.append(&row("Менеджер ключа", "gopass хранит отдельную зашифрованную запись dbdeck-unlock. Системная связка может разблокироваться при входе в систему. Для смены менеджера сначала отключите автоматическую разблокировку.", "dialog-password-symbolic",&unlock_provider));
    let remember_unlock = gtk::Button::with_label("Включить…");
    general.append(&row("Автоматическая разблокировка", "Ключ хранится в выбранном менеджере паролей. На новом компьютере потребуется исходный мастер-пароль. Если связка заблокирована, она может запросить вход.", "changes-prevent-symbolic", &remember_unlock));
    let import_gopass = gtk::Button::with_label("Добавить из gopass…");
    general.append(&row("Общие пароли", "Записи zer0-gopass-tui / zer0-waypass. Импорт параметров БД; пароль читается из gopass при каждом подключении и не дублируется в DBDeck.", "dialog-password-symbolic", &import_gopass));
    general.append(&row(
        "Резервная копия",
        "Сохранить профили и пароли подключений",
        "document-save-symbolic",
        &export_backup,
    ));
    general.append(&row(
        "Восстановление",
        "Импортировать профили из резервной копии",
        "document-open-symbolic",
        &import_backup,
    ));
    let sql_directory = gtk::Button::with_label("Выбрать…");
    sql_directory.set_widget_name("settings-sql-directory");
    let sql_directory_label = label(
        "Не выбрана — папку можно указать при сохранении",
        "settings-description",
    );
    general.append(&row(
        "Папка SQL-запросов",
        "Корневая папка сохранённых запросов",
        "folder-symbolic",
        &sql_directory,
    ));
    general.append(&sql_directory_label);
    let csv_directory = gtk::Button::with_label("Выбрать…");
    csv_directory.set_widget_name("settings-csv-directory");
    let csv_directory_label = label(
        "Последняя папка выбирается автоматически",
        "settings-description",
    );
    general.append(&row(
        "Папка CSV",
        "Запоминается после выбора файла экспорта",
        "folder-symbolic",
        &csv_directory,
    ));
    general.append(&csv_directory_label);
    let general_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&general)
        .build();
    pages.add_titled(&general_scroll, Some("general"), "Общие");
    let appearance = page(
        "Оформление",
        "Одна палитра для настроек, рабочего окна, таблиц и SQL-редактора.",
    );
    let theme_selector = gtk::ComboBoxText::new();
    theme_selector.set_widget_name("settings-theme-selector");
    theme_selector.append(Some("system"), "Системная");
    theme_selector.append(Some("tokyo_night"), "Tokyo Night");
    theme_selector.append(Some("dark"), "Сине-графитовая");
    theme_selector.append(Some("catppuccin_mocha"), "Catppuccin Mocha");
    appearance.append(&row(
        "Цветовая схема",
        "Применяется сразу ко всем открытым вкладкам",
        "weather-clear-night-symbolic",
        &theme_selector,
    ));
    let preview = gtk::Box::new(gtk::Orientation::Vertical, 10);
    preview.add_css_class("settings-preview");
    let weak_preview = preview.downgrade();
    theme_selector.connect_changed(move |selector| {
        if let Some(preview) = weak_preview.upgrade() {
            update_preview(
                &preview,
                ThemeMode::from_id(selector.active_id().as_deref()),
            );
        }
    });
    appearance.append(&preview);
    let appearance_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&appearance)
        .build();
    pages.add_titled(&appearance_scroll, Some("appearance"), "Оформление");
    root.append(&pages);
    let footer = label(
        "Настройки сохраняются автоматически · Esc — закрыть",
        "settings-description",
    );
    footer.set_margin_start(16);
    footer.set_margin_end(16);
    footer.set_margin_bottom(12);
    root.append(&footer);
    window.set_content(Some(&root));
    PreferencesUi {
        window,
        export_backup,
        import_backup,
        theme_selector,
        remember_unlock,
        unlock_provider,
        import_gopass,
        storage_path,
        sql_directory,
        sql_directory_label,
        csv_directory,
        csv_directory_label,
    }
}
