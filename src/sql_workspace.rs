//! Bounded SQL history and drafts, serialized only inside the encrypted bundle.
use serde::{Deserialize, Serialize};

pub const MAX_SQL_BYTES: usize = 128 * 1024;
const MAX_COLLECTION_BYTES: usize = 512 * 1024;
const MAX_HISTORY: usize = 100;
pub const MAX_DRAFTS: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SqlHistoryEntry {
    pub id: String,
    pub profile_id: String,
    #[serde(default)]
    pub connection_key: String,
    pub name: String,
    pub sql: String,
    pub executed_at: u64,
    pub duration_ms: u128,
    pub success: bool,
    pub row_count: usize,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SqlDraft {
    pub id: String,
    pub profile_id: String,
    #[serde(default)]
    pub connection_key: String,
    pub name: String,
    pub sql: String,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct SqlWorkspace {
    #[serde(default)]
    pub history: Vec<SqlHistoryEntry>,
    #[serde(default)]
    pub drafts: Vec<SqlDraft>,
}
impl SqlWorkspace {
    pub fn validate(&self) -> Result<(), String> {
        if self.history.len() > MAX_HISTORY || self.drafts.len() > MAX_DRAFTS {
            return Err("Превышен лимит истории или SQL-вкладок".into());
        }
        let mut ids = std::collections::HashSet::new();
        for entry in &self.history {
            if entry.connection_key.len() > 4096 {
                return Err("Некорректное подключение истории SQL".into());
            }
            validate_record(&entry.id, &entry.profile_id, &entry.name, &entry.sql)?;
            if !ids.insert(&entry.id) || entry.error.as_ref().is_some_and(|s| s.len() > 16 * 1024) {
                return Err("Некорректная история SQL".into());
            }
        }
        ids.clear();
        for draft in &self.drafts {
            if draft.connection_key.len() > 4096 {
                return Err("Некорректное подключение SQL-вкладки".into());
            }
            validate_record(&draft.id, &draft.profile_id, &draft.name, &draft.sql)?;
            if !ids.insert(&draft.id) {
                return Err("Повторяющийся ID SQL-вкладки".into());
            }
        }
        if self.history.iter().map(|e| e.sql.len()).sum::<usize>() > MAX_COLLECTION_BYTES
            || self.drafts.iter().map(|e| e.sql.len()).sum::<usize>() > MAX_COLLECTION_BYTES
        {
            return Err("Слишком много SQL для автосохранения. Сохраните большие запросы отдельными файлами (Ctrl+S).".into());
        }
        Ok(())
    }
    pub fn record(&mut self, entry: SqlHistoryEntry) -> Result<(), String> {
        validate_record(&entry.id, &entry.profile_id, &entry.name, &entry.sql)?;
        self.history.retain(|old| old.id != entry.id);
        self.history.push(entry);
        while self.history.len() > MAX_HISTORY
            || self.history.iter().map(|e| e.sql.len()).sum::<usize>() > MAX_COLLECTION_BYTES
        {
            self.history.remove(0);
        }
        self.validate()
    }
    pub fn replace_drafts(
        &mut self,
        profile_id: &str,
        drafts: Vec<SqlDraft>,
    ) -> Result<(), String> {
        if drafts.iter().any(|d| d.profile_id != profile_id) {
            return Err("SQL-вкладка относится к другому подключению".into());
        }
        let previous = self.drafts.clone();
        self.drafts.retain(|old| old.profile_id != profile_id);
        self.drafts.extend(drafts);
        if let Err(error) = self.validate() {
            self.drafts = previous;
            return Err(error);
        }
        Ok(())
    }
    pub fn merge(&mut self, imported: Self) -> Result<(), String> {
        imported.validate()?;
        for entry in imported.history {
            if !self.history.iter().any(|e| e.id == entry.id) {
                self.record(entry)?;
            }
        }
        for draft in imported.drafts {
            if !self.drafts.iter().any(|d| d.id == draft.id) {
                self.drafts.push(draft);
            }
        }
        self.validate()
    }
}
fn validate_record(id: &str, profile: &str, name: &str, sql: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || profile.is_empty()
        || profile.len() > 256
        || name.len() > 256
        || sql.len() > MAX_SQL_BYTES
    {
        return Err("SQL-вкладка слишком большая для автосохранения или имеет некорректные метаданные. Используйте Ctrl+S для сохранения в файл.".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn entry(id: usize) -> SqlHistoryEntry {
        SqlHistoryEntry {
            id: id.to_string(),
            profile_id: "test".into(),
            connection_key: "test-key".into(),
            name: "SQL".into(),
            sql: "SELECT 1".into(),
            executed_at: 0,
            duration_ms: 1,
            success: true,
            row_count: 1,
            error: None,
        }
    }
    #[test]
    fn history_is_bounded_and_invalid_drafts_do_not_replace_valid_work() {
        let mut workspace = SqlWorkspace::default();
        for i in 0..120 {
            workspace.record(entry(i)).unwrap();
        }
        assert_eq!(workspace.history.len(), MAX_HISTORY);
        assert_eq!(workspace.history[0].id, "20");
        workspace
            .replace_drafts(
                "test",
                vec![SqlDraft {
                    id: "draft".into(),
                    profile_id: "test".into(),
                    connection_key: "test-key".into(),
                    name: "SQL".into(),
                    sql: "SELECT 1".into(),
                }],
            )
            .unwrap();
        let mut large = workspace.drafts[0].clone();
        large.sql = "x".repeat(MAX_SQL_BYTES + 1);
        assert!(workspace.replace_drafts("test", vec![large]).is_err());
        assert_eq!(workspace.drafts[0].sql, "SELECT 1");
        let mut foreign = workspace.drafts[0].clone();
        foreign.profile_id = "other".into();
        assert!(workspace.replace_drafts("test", vec![foreign]).is_err());
        assert_eq!(workspace.drafts.len(), 1);
        assert_eq!(workspace.drafts[0].profile_id, "test");
        let old: SqlWorkspace = serde_json::from_str("{}").unwrap();
        assert!(old.history.is_empty() && old.drafts.is_empty());
    }
}
