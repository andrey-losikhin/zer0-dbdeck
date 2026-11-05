use directories::ProjectDirs;

#[derive(Clone, Copy)]
pub enum AppInstance {
    Stable,
    Test,
    Dev,
}

impl AppInstance {
    pub fn current() -> Self {
        let value = std::env::var_os("DBDECK_INSTANCE");
        match value.as_deref().and_then(std::ffi::OsStr::to_str) {
            None if value.is_none() => Self::Stable,
            Some("test") => Self::Test,
            Some("dev") => Self::Dev,
            _ => {
                eprintln!(
                    "DBDECK_INSTANCE: допустимы test и dev. Для обычного запуска уберите переменную."
                );
                std::process::exit(2);
            }
        }
    }

    pub fn app_id(self) -> &'static str {
        match self {
            Self::Stable => "io.github.andrey-losikhin.zer0-dbdeck",
            Self::Test => "io.github.andrey-losikhin.zer0-dbdeck.test",
            Self::Dev => "io.github.andrey-losikhin.zer0-dbdeck.dev",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Stable => "DBDeck",
            Self::Test => "DBDeck Test",
            Self::Dev => "DBDeck Dev",
        }
    }

    pub fn project_dirs(self) -> Option<ProjectDirs> {
        let name = match self {
            Self::Stable => "zer0-dbdeck",
            Self::Test => "zer0-dbdeck-test",
            Self::Dev => "zer0-dbdeck-dev",
        };
        ProjectDirs::from("io", "zer0", name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_keep_distinct_paths_and_application_ids() {
        let stable = AppInstance::Stable.project_dirs().unwrap();
        let test = AppInstance::Test.project_dirs().unwrap();
        let dev = AppInstance::Dev.project_dirs().unwrap();
        let existing = ProjectDirs::from("io", "zer0", "zer0-dbdeck").unwrap();

        assert_eq!(stable.data_dir(), existing.data_dir());
        assert_eq!(stable.config_dir(), existing.config_dir());
        assert_eq!(stable.cache_dir(), existing.cache_dir());
        for (left, right) in [(&stable, &test), (&stable, &dev), (&test, &dev)] {
            assert_ne!(left.data_dir(), right.data_dir());
            assert_ne!(left.config_dir(), right.config_dir());
            assert_ne!(left.cache_dir(), right.cache_dir());
        }
        assert_ne!(AppInstance::Stable.app_id(), AppInstance::Test.app_id());
        assert_ne!(AppInstance::Stable.app_id(), AppInstance::Dev.app_id());
        assert_ne!(AppInstance::Test.app_id(), AppInstance::Dev.app_id());
    }
}
