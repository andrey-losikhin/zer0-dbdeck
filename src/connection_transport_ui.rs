use super::*;
use settings::Transport;

#[derive(Clone)]
pub(super) struct Fields {
    expander: gtk::Expander,
    mode: gtk::DropDown,
    gateway: gtk::Entry,
    port: gtk::SpinButton,
    username: gtk::Entry,
    identity: gtk::Entry,
    known_hosts: gtk::Entry,
}

impl Fields {
    pub fn new(grid: &gtk::Grid, row: i32) -> Self {
        let fields = Self {
            expander: gtk::Expander::new(Some("SSH и прокси")),
            mode: gtk::DropDown::from_strings(&[
                "Прямое подключение",
                "SSH",
                "SOCKS5",
                "HTTP CONNECT",
            ]),
            gateway: gtk::Entry::new(),
            port: gtk::SpinButton::with_range(1.0, 65535.0, 1.0),
            username: gtk::Entry::new(),
            identity: gtk::Entry::new(),
            known_hosts: gtk::Entry::new(),
        };
        fields
            .identity
            .set_placeholder_text(Some("Необязательно: ключ SSH, абсолютный путь"));
        fields
            .known_hosts
            .set_placeholder_text(Some("Необязательно: абсолютный путь"));
        let form = gtk::Grid::builder()
            .row_spacing(6)
            .column_spacing(12)
            .build();
        attach_form_row(&form, 0, "Маршрут", &fields.mode);
        attach_form_row(&form, 1, "SSH-сервер или прокси", &fields.gateway);
        attach_form_row(&form, 2, "Порт", &fields.port);
        attach_form_row(&form, 3, "Пользователь SSH", &fields.username);
        attach_form_row(&form, 4, "Файл ключа SSH", &fields.identity);
        attach_form_row(&form, 5, "Файл known_hosts", &fields.known_hosts);
        let note = gtk::Label::new(Some(
            "SSH использует ключ или агент и проверяет known_hosts. SOCKS5 и HTTP CONNECT работают без авторизации. Внешние утилиты бэкапа доступны для прямых подключений.",
        ));
        note.set_wrap(true);
        note.set_max_width_chars(65);
        note.set_xalign(0.0);
        note.add_css_class("dim-label");
        form.attach(&note, 0, 6, 2, 1);
        fields.expander.set_child(Some(&form));
        grid.attach(&fields.expander, 0, row, 2, 1);

        let gateway = fields.gateway.clone();
        let port = fields.port.clone();
        let username = fields.username.clone();
        let identity = fields.identity.clone();
        let known_hosts = fields.known_hosts.clone();
        let update = move |mode: &gtk::DropDown| {
            let selected = mode.selected();
            gateway.set_sensitive(selected != 0);
            port.set_sensitive(selected != 0);
            username.set_sensitive(selected == 1);
            identity.set_sensitive(selected == 1);
            known_hosts.set_sensitive(selected == 1);
            port.set_value(match selected {
                2 => 1080.0,
                3 => 8080.0,
                _ => 22.0,
            });
        };
        update(&fields.mode);
        fields.mode.connect_selected_notify(update);
        fields
    }

    pub fn load(&self, transport: &Transport) {
        let (mode, host, port) = match transport {
            Transport::Direct => return,
            Transport::Ssh {
                host,
                port,
                username,
                identity_file,
                known_hosts,
            } => {
                self.username.set_text(username);
                if let Some(path) = identity_file {
                    self.identity.set_text(&path.to_string_lossy());
                }
                if let Some(path) = known_hosts {
                    self.known_hosts.set_text(&path.to_string_lossy());
                }
                (1, host, port)
            }
            Transport::Socks5 { host, port } => (2, host, port),
            Transport::HttpConnect { host, port } => (3, host, port),
        };
        self.mode.set_selected(mode);
        self.gateway.set_text(host);
        self.port.set_value(f64::from(*port));
        self.expander.set_expanded(true);
    }

    pub fn values(&self) -> Transport {
        let host = self.gateway.text().trim().to_owned();
        let port = self.port.value_as_int() as u16;
        match self.mode.selected() {
            1 => {
                let path = |entry: &gtk::Entry| {
                    let text = entry.text();
                    (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()))
                };
                Transport::Ssh {
                    host,
                    port,
                    username: self.username.text().trim().to_owned(),
                    identity_file: path(&self.identity),
                    known_hosts: path(&self.known_hosts),
                }
            }
            2 => Transport::Socks5 { host, port },
            3 => Transport::HttpConnect { host, port },
            _ => Transport::Direct,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires GTK/Wayland; use tests/run-headless.sh"]
    fn transport_form_round_trips_profiles() {
        gtk::init().unwrap();
        let fields = Fields::new(&gtk::Grid::new(), 0);
        assert_eq!(fields.values(), Transport::Direct);
        assert!(!fields.gateway.is_sensitive());

        for route in [
            Transport::Ssh {
                host: "gateway.test".into(),
                port: 2222,
                username: "tester".into(),
                identity_file: Some("/tmp/test-id".into()),
                known_hosts: Some("/tmp/test-known-hosts".into()),
            },
            Transport::Socks5 {
                host: "proxy.test".into(),
                port: 9001,
            },
            Transport::HttpConnect {
                host: "proxy.test".into(),
                port: 9002,
            },
        ] {
            fields.load(&route);
            assert_eq!(fields.values(), route);
            assert!(fields.gateway.is_sensitive());
            assert_eq!(
                fields.username.is_sensitive(),
                matches!(route, Transport::Ssh { .. })
            );
        }

        fields.mode.set_selected(0);
        assert_eq!(fields.values(), Transport::Direct);
        assert!(!fields.gateway.is_sensitive());
    }
}
