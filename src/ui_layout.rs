use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Layout {
    pub sidebar_width: i32,
    pub sidebar_collapsed: bool,
    pub sql_position: i32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_width: 318,
            sidebar_collapsed: false,
            sql_position: 340,
        }
    }
}

fn path() -> Option<PathBuf> {
    crate::app_instance::AppInstance::current()
        .project_dirs()
        .map(|dirs| dirs.config_dir().join("layout.json"))
}

impl Layout {
    pub fn load() -> Self {
        let mut layout: Self = path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        layout.sidebar_width = layout.sidebar_width.clamp(318, 1600);
        layout.sql_position = layout.sql_position.clamp(100, 1600);
        layout
    }

    pub fn save(&self) -> Result<(), String> {
        let path = path().ok_or("Не удалось определить каталог настроек")?;
        let data = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        crate::settings::write_secure_file(&path, &data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_roundtrip_and_missing_fields() {
        let layout = Layout {
            sidebar_width: 480,
            sidebar_collapsed: true,
            sql_position: 220,
        };
        let restored: Layout =
            serde_json::from_slice(&serde_json::to_vec(&layout).unwrap()).unwrap();
        assert_eq!(restored.sidebar_width, 480);
        assert!(restored.sidebar_collapsed);
        assert_eq!(restored.sql_position, 220);
        let old: Layout = serde_json::from_str("{}").unwrap();
        assert_eq!(old.sidebar_width, 318);
        assert_eq!(old.sql_position, 340);
    }
}
