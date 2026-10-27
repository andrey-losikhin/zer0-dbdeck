//! Private temporary output, published only after successful completion.
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
pub struct AtomicOutput {
    pub file: File,
    temporary: PathBuf,
    destination: PathBuf,
}
impl AtomicOutput {
    pub fn new(destination: &Path) -> Result<Self, String> {
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let temporary = parent.join(format!(".dbdeck-{}.tmp", uuid::Uuid::new_v4()));
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| format!("Не удалось создать файл экспорта: {e}"))?;
        Ok(Self {
            file,
            temporary,
            destination: destination.to_owned(),
        })
    }
    pub fn bytes(&self) -> u64 {
        self.file.metadata().map(|m| m.len()).unwrap_or(0)
    }
    pub fn commit(self, cancel: &crate::transfer::Cancellation) -> Result<(), String> {
        self.file
            .sync_all()
            .map_err(|e| format!("Не удалось завершить запись: {e}"))?;
        cancel.publish(|| {
            std::fs::rename(&self.temporary, &self.destination)
                .map_err(|e| format!("Не удалось сохранить экспорт: {e}"))
        })
    }
}
impl Drop for AtomicOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.temporary);
    }
}
