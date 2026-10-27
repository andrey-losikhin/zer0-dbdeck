use super::*;
#[derive(Clone)]
pub(super) struct Fields {
    entries: [gtk::Entry; 3],
    legacy_allow: Cell<bool>,
    tools: gtk::Entry,
}
impl Fields {
    pub fn new(grid: &gtk::Grid, row: i32) -> Self {
        let entries = std::array::from_fn(|_| gtk::Entry::new());
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        for (entry, label) in entries.iter().zip([
            "CA-сертификат (PEM)",
            "Клиентский сертификат (PEM)",
            "Закрытый ключ (права 0600)",
        ]) {
            entry.set_placeholder_text(Some(&format!("{label}: абсолютный путь")));
            entry.set_hexpand(true);
            content.append(entry);
        }
        let expander = gtk::Expander::new(Some("TLS: CA и клиентский сертификат"));
        expander.set_child(Some(&content));
        grid.attach(&expander, 0, row, 2, 1);
        let tools = gtk::Entry::new();
        tools.set_placeholder_text(Some("Необязательно: /opt/mysql/bin; по умолчанию PATH"));
        tools.set_hexpand(true);
        tools.set_tooltip_text(Some("Каталог доверенных исполняемых файлов соответствующей СУБД: pg_dump/pg_restore/psql, mysqldump/mysql или mariadb-dump/mariadb. Используйте версию, совместимую с сервером."));
        attach_form_row(grid, row + 1, "Каталог утилит БД", &tools);
        Self {
            entries,
            legacy_allow: Cell::new(false),
            tools,
        }
    }
    pub fn load_tools(&self, path: Option<&std::path::Path>) {
        self.tools.set_text(
            &path
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
    }
    pub fn tools_dir(&self) -> Option<PathBuf> {
        let text = self.tools.text();
        (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()))
    }
    pub fn load(&self, files: &settings::TlsFiles) {
        for (entry, path) in
            self.entries
                .iter()
                .zip([&files.ca, &files.client_cert, &files.client_key])
        {
            entry.set_text(
                &path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
        }
        self.legacy_allow.set(files.allow_unverified_prod);
    }
    pub fn values(&self) -> settings::TlsFiles {
        let path = |i: usize| {
            let value = self.entries[i].text();
            (!value.trim().is_empty()).then(|| PathBuf::from(value.as_str()))
        };
        settings::TlsFiles {
            ca: path(0),
            client_cert: path(1),
            client_key: path(2),
            allow_unverified_prod: self.legacy_allow.get(),
        }
    }
}
