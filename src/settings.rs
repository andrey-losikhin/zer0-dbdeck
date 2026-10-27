use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};

use directories::ProjectDirs;
use ring::{
    aead, pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use zeroize::{Zeroize, Zeroizing};

const VAULT_VERSION: u32 = 1;
const PBKDF2_ITERATIONS: u32 = 600_000;
const KEY_LEN: usize = 32;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseKind {
    PostgreSql,
    MySql,
    MariaDb,
}

impl DatabaseKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::PostgreSql => "PostgreSQL",
            Self::MySql => "MySQL",
            Self::MariaDb => "MariaDB",
        }
    }

    pub fn default_port(&self) -> u16 {
        match self {
            Self::PostgreSql => 5432,
            Self::MySql | Self::MariaDb => 3306,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentTag {
    Prod,
    Stage,
    Local,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    Disabled,
    Require,
    #[default]
    VerifyFull,
}

impl TlsMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Disabled => "Без TLS (для локальной/доверенной сети)",
            Self::Require => "TLS без проверки сертификата",
            Self::VerifyFull => "TLS с проверкой сертификата и имени",
        }
    }
}

impl EnvironmentTag {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Prod => "PROD",
            Self::Stage => "STAGE",
            Self::Local => "LOCAL",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsFiles {
    pub ca: Option<PathBuf>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
    // Retained for compatibility with saved session identities; no longer gates PROD.
    #[serde(default)]
    pub allow_unverified_prod: bool,
}
impl TlsFiles {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    pub kind: DatabaseKind,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub environment: EnvironmentTag,
    pub color: String,
    pub read_only: bool,
    #[serde(default)]
    pub tls_mode: TlsMode,
    #[serde(default = "legacy_profile_may_have_password")]
    pub password_saved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gopass_entry: Option<String>,
    #[serde(default)]
    pub tls_files: TlsFiles,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_tools_dir: Option<PathBuf>,
}

fn legacy_profile_may_have_password() -> bool {
    true
}

impl ConnectionProfile {
    pub fn validate_transport(&self) -> Result<(), String> {
        if self.tls_files.client_cert.is_some() != self.tls_files.client_key.is_some() {
            return Err("Укажите вместе клиентский TLS-сертификат и закрытый ключ".into());
        }
        let paths = [
            self.tls_files.ca.as_ref(),
            self.tls_files.client_cert.as_ref(),
            self.tls_files.client_key.as_ref(),
        ];
        if matches!(self.tls_mode, TlsMode::Disabled) && paths.iter().any(|p| p.is_some()) {
            return Err("Файлы сертификатов заданы, но TLS выключен".into());
        }
        for path in paths.into_iter().flatten() {
            if !path.is_absolute() || !path.is_file() {
                return Err(format!(
                    "Укажите абсолютный путь к существующему файлу TLS: {}",
                    path.display()
                ));
            }
        }
        if let Some(path) = &self.tls_files.client_key {
            let permissions = fs::metadata(path)
                .map_err(|e| e.to_string())?
                .permissions()
                .mode();
            if permissions & 0o077 != 0 {
                return Err(
                    "Закрытый TLS-ключ должен быть доступен только владельцу (права 0600)".into(),
                );
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct ProfilesFile {
    version: u32,
    connections: Vec<ConnectionProfile>,
    #[serde(default)]
    sql_queries_directory: Option<PathBuf>,
}

pub struct SettingsStore {
    sql_workspace: crate::sql_workspace::SqlWorkspace,
    path: PathBuf,
    vault: CredentialVault,
    pub profiles: Vec<ConnectionProfile>,
    sql_queries_directory: Option<PathBuf>,
    csv_export_directory: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Serialize, Deserialize)]
struct PortableBackup {
    version: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Serialize, Deserialize)]
struct PortablePayload {
    #[serde(default)]
    sql_workspace: crate::sql_workspace::SqlWorkspace,
    connections: Vec<ConnectionProfile>,
    passwords: HashMap<String, String>,
    #[serde(default)]
    sql_queries_directory: Option<PathBuf>,
    #[serde(default)]
    csv_export_directory: Option<PathBuf>,
}

struct CredentialVault {
    path: PathBuf,
    key: Option<Zeroizing<Vec<u8>>>,
    salt: Option<[u8; 16]>,
    values: HashMap<String, String>,
}

#[derive(Debug)]
pub enum ProfileSaveError {
    Secret(String),
    Settings(String),
}

impl SettingsStore {
    pub fn load() -> Result<Self, String> {
        let project_dirs = ProjectDirs::from("io", "zer0", "zer0-dbdeck")
            .ok_or_else(|| "Не удалось определить каталог конфигурации XDG".to_owned())?;
        Self::load_from(
            project_dirs.data_dir().join("connections.dbdeck"),
            project_dirs.config_dir().join("connections.json"),
            project_dirs.data_dir().join("credentials.vault"),
        )
    }

    fn load_from(
        path: PathBuf,
        legacy_path: PathBuf,
        legacy_vault: PathBuf,
    ) -> Result<Self, String> {
        let canonical_exists = match fs::symlink_metadata(&path) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err("Не удалось проверить файл подключений".to_owned()),
        };
        let (profiles, sql_queries_directory) = if canonical_exists {
            (Vec::new(), None)
        } else {
            match fs::read(&legacy_path) {
                Ok(data) => {
                    let saved: ProfilesFile = serde_json::from_slice(&data)
                        .map_err(|error| format!("Не удалось прочитать профили: {error}"))?;
                    if saved.version != 1 {
                        return Err(format!(
                            "Формат конфигурации версии {} не поддерживается",
                            saved.version
                        ));
                    }
                    let mut ids = std::collections::HashSet::new();
                    for profile in &saved.connections {
                        validate_profile(profile)?;
                        if !ids.insert(&profile.id) {
                            return Err("Повторяющийся ID старого подключения".to_owned());
                        }
                    }
                    (saved.connections, saved.sql_queries_directory)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
                Err(error) => return Err(format!("Не удалось открыть профили: {error}")),
            }
        };

        Ok(Self {
            sql_workspace: crate::sql_workspace::SqlWorkspace::default(),
            path,
            vault: CredentialVault {
                path: legacy_vault,
                key: None,
                salt: None,
                values: HashMap::new(),
            },
            profiles,
            sql_queries_directory,
            csv_export_directory: None,
        })
    }

    pub fn sql_workspace(&self) -> &crate::sql_workspace::SqlWorkspace {
        &self.sql_workspace
    }

    pub fn record_sql(
        &mut self,
        entry: crate::sql_workspace::SqlHistoryEntry,
    ) -> Result<(), String> {
        let mut workspace = self.sql_workspace.clone();
        workspace.record(entry)?;
        self.commit(|store| store.sql_workspace = workspace)
    }
    pub fn replace_sql_drafts(
        &mut self,
        profile_id: &str,
        drafts: Vec<crate::sql_workspace::SqlDraft>,
    ) -> Result<(), String> {
        let mut workspace = self.sql_workspace.clone();
        workspace.replace_drafts(profile_id, drafts)?;
        self.commit(|store| store.sql_workspace = workspace)
    }
    pub fn remove_sql_draft(&mut self, id: &str) -> Result<(), String> {
        self.commit(|store| store.sql_workspace.drafts.retain(|draft| draft.id != id))
    }

    pub fn clear_sql_history(&mut self) -> Result<(), String> {
        self.commit(|store| store.sql_workspace.history.clear())
    }

    pub fn csv_export_directory(&self) -> Option<PathBuf> {
        self.csv_export_directory.clone()
    }
    pub fn set_csv_export_directory(&mut self, directory: PathBuf) -> Result<(), String> {
        self.commit(|store| store.csv_export_directory = Some(directory))
    }

    pub fn sql_queries_directory(&self) -> Option<PathBuf> {
        self.sql_queries_directory.clone()
    }

    pub fn set_sql_queries_directory(&mut self, directory: PathBuf) -> Result<(), String> {
        let previous = self.sql_queries_directory.replace(directory);
        if let Err(error) = self.write_profiles() {
            self.sql_queries_directory = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn vault_is_unlocked(&self) -> bool {
        self.vault.key.is_some()
    }

    pub fn vault_exists(&self) -> bool {
        self.path.symlink_metadata().is_ok() || self.vault.path.exists()
    }

    pub fn storage_path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn unlock_or_create_vault(&mut self, master_password: &str) -> Result<bool, String> {
        if self.path.symlink_metadata().is_ok() {
            let saved = read_bundle(&self.path)?;
            let salt = bundle_salt(&saved)?;
            let key = derive_key(master_password, &salt)?;
            self.unlock_with_key(&key)?;
            return Ok(false);
        }
        if master_password.chars().count() < 4 {
            return Err("Мастер-пароль должен содержать не менее 4 символов".to_owned());
        }
        let exists = self.vault.path.exists();
        if !exists && master_password.chars().count() < 12 {
            return Err(
                "Для нового хранилища нужен мастер-пароль не короче 12 символов".to_owned(),
            );
        }
        if exists {
            self.vault.unlock(master_password)?;
        } else {
            let mut salt = [0; 16];
            SystemRandom::new()
                .fill(&mut salt)
                .map_err(|_| "Не удалось получить соль шифрования")?;
            self.vault.key = Some(derive_key(master_password, &salt)?);
            self.vault.salt = Some(salt);
        }
        if let Err(error) = self.write_profiles() {
            self.vault.key = None;
            self.vault.salt = None;
            self.vault.values.values_mut().for_each(Zeroize::zeroize);
            self.vault.values.clear();
            return Err(error);
        }
        Ok(!exists)
    }

    pub fn keyring_id(&self) -> Result<String, String> {
        let salt = if self.path.symlink_metadata().is_ok() {
            bundle_salt(&read_bundle(&self.path)?)?
        } else {
            self.vault.salt.ok_or("Сначала разблокируйте хранилище")?
        };
        Ok(hex_encode(
            ring::digest::digest(&ring::digest::SHA256, &salt).as_ref(),
        ))
    }

    pub fn unlock_key(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        self.vault
            .key
            .clone()
            .ok_or_else(|| "Хранилище заблокировано".to_owned())
    }

    pub fn unlock_with_key(&mut self, key: &[u8]) -> Result<(), String> {
        let saved = read_bundle(&self.path)?;
        let salt = bundle_salt(&saved)?;
        let payload = decrypt_bundle(&saved, key)?;
        self.vault.key = Some(Zeroizing::new(key.to_vec()));
        self.vault.salt = Some(salt);
        self.vault.values.values_mut().for_each(Zeroize::zeroize);
        self.vault.values = payload.passwords;
        self.profiles = payload.connections;
        self.sql_workspace = payload.sql_workspace;
        self.sql_queries_directory = payload.sql_queries_directory;
        self.csv_export_directory = payload.csv_export_directory;
        Ok(())
    }

    fn commit(&mut self, change: impl FnOnce(&mut Self)) -> Result<(), String> {
        if !self.vault_is_unlocked() {
            return Err("Хранилище заблокировано".to_owned());
        }
        let workspace = self.sql_workspace.clone();
        let profiles = self.profiles.clone();
        let directory = self.sql_queries_directory.clone();
        let csv_directory = self.csv_export_directory.clone();
        let mut values = self.vault.values.clone();
        change(self);
        if let Err(error) = self.write_profiles() {
            self.profiles = profiles;
            self.sql_workspace = workspace;
            self.sql_queries_directory = directory;
            self.csv_export_directory = csv_directory;
            self.vault.values.values_mut().for_each(Zeroize::zeroize);
            self.vault.values = values;
            return Err(error);
        }
        values.values_mut().for_each(Zeroize::zeroize);
        Ok(())
    }

    pub fn save_profile(
        &mut self,
        mut profile: ConnectionProfile,
        password: &str,
    ) -> Result<(), ProfileSaveError> {
        validate_profile(&profile).map_err(ProfileSaveError::Settings)?;
        if self.profiles.iter().any(|p| p.id == profile.id) {
            return Err(ProfileSaveError::Settings(
                "Профиль уже существует".to_owned(),
            ));
        }
        self.commit(|store| {
            profile.password_saved = profile.gopass_entry.is_some() || !password.is_empty();
            if profile.gopass_entry.is_none() && !password.is_empty() {
                store
                    .vault
                    .values
                    .insert(profile.id.clone(), password.to_owned());
            }
            store.profiles.push(profile);
        })
        .map_err(ProfileSaveError::Settings)
    }

    pub fn update_profile(
        &mut self,
        mut profile: ConnectionProfile,
        password: Option<&str>,
    ) -> Result<(), ProfileSaveError> {
        validate_profile(&profile).map_err(ProfileSaveError::Settings)?;
        let index = self
            .profiles
            .iter()
            .position(|p| p.id == profile.id)
            .ok_or_else(|| ProfileSaveError::Settings("Профиль не найден".to_owned()))?;
        self.commit(|store| {
            profile.password_saved = profile.gopass_entry.is_some()
                || password.is_some()
                || store.profiles[index].password_saved;
            if profile.gopass_entry.is_some() {
                if let Some(mut value) = store.vault.values.remove(&profile.id) {
                    value.zeroize();
                }
            } else if let Some(password) = password {
                store
                    .vault
                    .values
                    .insert(profile.id.clone(), password.to_owned());
            }
            // Switching from an external entry to local storage requires a new password.
            if profile.gopass_entry.is_none()
                && password.is_none()
                && store.profiles[index].gopass_entry.is_some()
            {
                profile.password_saved = false;
            }
            store.profiles[index] = profile;
        })
        .map_err(ProfileSaveError::Settings)
    }

    pub fn delete_profile(&mut self, id: &str) -> Result<(), String> {
        if !self.profiles.iter().any(|p| p.id == id) {
            return Err("Профиль не найден".to_owned());
        }
        self.commit(|store| {
            store.profiles.retain(|p| p.id != id);
            if let Some(mut value) = store.vault.values.remove(id) {
                value.zeroize();
            }
        })
    }

    pub fn password(&self, id: &str) -> Result<String, String> {
        if !self.vault_is_unlocked() {
            return Err("Хранилище заблокировано".to_owned());
        }
        if self
            .profiles
            .iter()
            .any(|p| p.id == id && p.gopass_entry.is_some())
        {
            return Err("Пароль этого подключения хранится в gopass".to_owned());
        }
        Ok(self.vault.values.get(id).cloned().unwrap_or_default())
    }

    pub fn save_password(&mut self, id: &str, password: &str) -> Result<(), ProfileSaveError> {
        let index = self
            .profiles
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| ProfileSaveError::Settings("Профиль не найден".to_owned()))?;
        if self.profiles[index].gopass_entry.is_some() {
            return Err(ProfileSaveError::Secret(
                "Пароль изменяется в gopass".to_owned(),
            ));
        }
        self.commit(|store| {
            store
                .vault
                .values
                .insert(id.to_owned(), password.to_owned());
            store.profiles[index].password_saved = true;
        })
        .map_err(ProfileSaveError::Settings)
    }

    pub fn export_backup(&self, path: &PathBuf) -> Result<(), String> {
        let key = self
            .vault
            .key
            .as_ref()
            .ok_or_else(|| "Сначала разблокируйте хранилище".to_owned())?;
        let salt = self
            .vault
            .salt
            .ok_or_else(|| "Не задана соль хранилища".to_owned())?;
        let mut nonce = [0_u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "Не удалось получить случайный nonce для резервной копии".to_owned())?;
        let payload = PortablePayload {
            sql_workspace: self.sql_workspace.clone(),
            connections: self.profiles.clone(),
            passwords: self.vault.values.clone(),
            sql_queries_directory: self.sql_queries_directory.clone(),
            csv_export_directory: self.csv_export_directory.clone(),
        };
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&payload).map_err(|_| "Не удалось собрать резервную копию")?,
        );
        let ciphertext =
            encrypt_with_aad(key, &nonce, &plaintext, b"zer0-dbdeck portable backup v1")?;
        let backup = PortableBackup {
            version: VAULT_VERSION,
            salt: hex_encode(&salt),
            nonce: hex_encode(&nonce),
            ciphertext: hex_encode(&ciphertext),
        };
        let data = serde_json::to_vec_pretty(&backup)
            .map_err(|error| format!("Не удалось сериализовать резервную копию: {error}"))?;
        if data.len() > 16 * 1024 * 1024 {
            return Err("Зашифрованное хранилище превысило лимит 16 МБ".into());
        }
        write_secure_file(path, &data)
    }

    pub fn import_backup(&mut self, path: &PathBuf, master_password: &str) -> Result<(), String> {
        let saved = read_bundle(path)?;
        let salt = bundle_salt(&saved)?;
        let key = derive_key(master_password, &salt)?;
        let payload = decrypt_bundle(&saved, &key)?;
        if self.vault_exists() && !self.vault_is_unlocked() {
            return Err("Сначала разблокируйте текущее хранилище".to_owned());
        }
        let mut workspace = self.sql_workspace.clone();
        workspace.merge(payload.sql_workspace)?;
        let created = self.vault.key.is_none();
        if created {
            self.vault.key = Some(key);
            self.vault.salt = Some(salt);
        }
        let result = self.commit(|store| {
            store.sql_workspace = workspace;
            if store.sql_queries_directory.is_none() {
                store.sql_queries_directory = payload.sql_queries_directory;
            }
            if store.csv_export_directory.is_none() {
                store.csv_export_directory = payload.csv_export_directory;
            }
            store.vault.values.extend(payload.passwords);
            for profile in payload.connections {
                // Replacing a local profile must also replace its password source.
                if profile.gopass_entry.is_some() || !profile.password_saved {
                    if let Some(mut password) = store.vault.values.remove(&profile.id) {
                        password.zeroize();
                    }
                }
                if let Some(index) = store.profiles.iter().position(|p| p.id == profile.id) {
                    store.profiles[index] = profile;
                } else {
                    store.profiles.push(profile);
                }
            }
        });
        if result.is_err() && created {
            self.vault.key = None;
            self.vault.salt = None;
        }
        result
    }

    fn write_profiles(&self) -> Result<(), String> {
        self.export_backup(&self.path)
    }
}

fn validate_profile(profile: &ConnectionProfile) -> Result<(), String> {
    if profile.id.is_empty()
        || profile.name.trim().is_empty()
        || profile.host.trim().is_empty()
        || profile.port == 0
    {
        return Err("Некорректный профиль подключения".to_owned());
    }
    if let Some(entry) = &profile.gopass_entry {
        crate::secret_store::validate_entry(entry)?;
    }
    Ok(())
}

fn read_bundle(path: &std::path::Path) -> Result<PortableBackup, String> {
    let file =
        fs::File::open(path).map_err(|_| "Не удалось открыть зашифрованный файл подключений")?;
    use std::io::Read;
    let mut data = Vec::new();
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(|_| "Не удалось прочитать хранилище")?;
    if data.len() > 16 * 1024 * 1024 {
        return Err("Хранилище слишком большое".to_owned());
    }
    let saved: PortableBackup =
        serde_json::from_slice(&data).map_err(|_| "Файл хранилища повреждён")?;
    if saved.version != VAULT_VERSION {
        return Err("Неподдерживаемая версия хранилища".to_owned());
    }
    Ok(saved)
}
fn bundle_salt(saved: &PortableBackup) -> Result<[u8; 16], String> {
    hex_decode(&saved.salt)?
        .try_into()
        .map_err(|_| "Некорректная соль хранилища".to_owned())
}
fn decrypt_bundle(saved: &PortableBackup, key: &[u8]) -> Result<PortablePayload, String> {
    let nonce: [u8; 12] = hex_decode(&saved.nonce)?
        .try_into()
        .map_err(|_| "Некорректный nonce хранилища")?;
    let plain = Zeroizing::new(
        decrypt_with_aad(
            key,
            &nonce,
            &hex_decode(&saved.ciphertext)?,
            b"zer0-dbdeck portable backup v1",
        )
        .map_err(|_| "Неверный ключ/мастер-пароль или повреждён файл")?,
    );
    let payload: PortablePayload =
        serde_json::from_slice(&plain).map_err(|_| "Данные хранилища повреждены")?;
    payload.sql_workspace.validate()?;
    let mut ids = std::collections::HashSet::new();
    for profile in &payload.connections {
        validate_profile(profile)?;
        if !ids.insert(&profile.id) {
            return Err("Повторяющийся ID подключения".to_owned());
        }
    }
    Ok(payload)
}

impl CredentialVault {
    #[cfg(test)]
    fn password(&self, profile_id: &str) -> Result<String, String> {
        if self.key.is_none() && self.path.exists() {
            return Err("Зашифрованное хранилище заблокировано".to_owned());
        }
        Ok(self.values.get(profile_id).cloned().unwrap_or_default())
    }

    #[cfg(test)]
    fn create(&mut self, master_password: &str) -> Result<(), String> {
        let mut salt = [0_u8; 16];
        SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| "Не удалось получить случайные данные для шифрования".to_owned())?;
        self.salt = Some(salt);
        self.key = Some(derive_key(master_password, &salt)?);
        if let Err(error) = self.persist() {
            self.key = None;
            self.salt = None;
            return Err(error);
        }
        Ok(())
    }

    fn unlock(&mut self, master_password: &str) -> Result<(), String> {
        let data = fs::read(&self.path)
            .map_err(|error| format!("Не удалось открыть хранилище паролей: {error}"))?;
        let saved: VaultFile = serde_json::from_slice(&data)
            .map_err(|error| format!("Файл хранилища повреждён: {error}"))?;
        if saved.version != VAULT_VERSION {
            return Err(format!(
                "Версия хранилища {} не поддерживается",
                saved.version
            ));
        }
        let salt: [u8; 16] = hex_decode(&saved.salt)?
            .try_into()
            .map_err(|_| "Соль хранилища имеет неправильную длину".to_owned())?;
        let nonce: [u8; 12] = hex_decode(&saved.nonce)?
            .try_into()
            .map_err(|_| "Nonce хранилища имеет неправильную длину".to_owned())?;
        let key = derive_key(master_password, &salt)?;
        let plaintext = Zeroizing::new(
            decrypt(&key, &nonce, &hex_decode(&saved.ciphertext)?)
                .map_err(|_| "Неверный мастер-пароль или повреждено хранилище".to_owned())?,
        );
        self.values = serde_json::from_slice(&plaintext)
            .map_err(|_| "Данные старого хранилища повреждены")?;
        self.key = Some(key);
        self.salt = Some(salt);
        Ok(())
    }

    #[cfg(test)]
    fn save(&mut self, profile_id: &str, password: &str) -> Result<(), String> {
        if self.key.is_none() {
            return Err("Сначала разблокируйте хранилище паролей".to_owned());
        }
        let previous = self
            .values
            .insert(profile_id.to_owned(), password.to_owned());
        if let Err(error) = self.persist() {
            if let Some(previous) = previous {
                self.values.insert(profile_id.to_owned(), previous);
            } else {
                self.values.remove(profile_id);
            }
            return Err(error);
        }
        Ok(())
    }

    #[cfg(test)]
    fn persist(&self) -> Result<(), String> {
        let key = self
            .key
            .as_ref()
            .ok_or_else(|| "Хранилище заблокировано".to_owned())?;
        let salt = self
            .salt
            .ok_or_else(|| "Не задана соль хранилища".to_owned())?;
        let mut nonce = [0_u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "Не удалось получить случайный nonce для шифрования".to_owned())?;
        let plaintext = serde_json::to_vec(&self.values)
            .map_err(|error| format!("Не удалось сериализовать пароли: {error}"))?;
        let ciphertext = encrypt(key, &nonce, &plaintext)?;
        let saved = VaultFile {
            version: VAULT_VERSION,
            salt: hex_encode(&salt),
            nonce: hex_encode(&nonce),
            ciphertext: hex_encode(&ciphertext),
        };
        let data = serde_json::to_vec_pretty(&saved)
            .map_err(|error| format!("Не удалось сериализовать хранилище: {error}"))?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "Некорректный путь хранилища".to_owned())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("Не удалось создать каталог хранилища: {error}"))?;
        let temporary_path = self.path.with_extension("vault.tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary_path)
            .map_err(|error| format!("Не удалось создать файл хранилища: {error}"))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("Не удалось защитить файл хранилища: {error}"))?;
        file.write_all(&data)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("Не удалось записать хранилище: {error}"))?;
        fs::rename(&temporary_path, &self.path)
            .map_err(|error| format!("Не удалось обновить хранилище: {error}"))
    }
}

fn derive_key(password: &str, salt: &[u8; 16]) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut key = Zeroizing::new(vec![0_u8; KEY_LEN]);
    let iterations = std::num::NonZeroU32::new(PBKDF2_ITERATIONS).expect("non-zero iterations");
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        password.as_bytes(),
        &mut key,
    );
    Ok(key)
}

#[cfg(test)]
fn encrypt(key: &[u8], nonce: &[u8; 12], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    encrypt_with_aad(key, nonce, plaintext, b"zer0-dbdeck credentials v1")
}

fn encrypt_with_aad(
    key: &[u8],
    nonce: &[u8; 12],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, String> {
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map_err(|_| "Не удалось подготовить шифрование".to_owned())?;
    let cipher = aead::LessSafeKey::new(unbound);
    let mut output = plaintext.to_vec();
    cipher
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(*nonce),
            aead::Aad::from(aad),
            &mut output,
        )
        .map_err(|_| "Не удалось зашифровать пароли".to_owned())?;
    Ok(output)
}

fn decrypt(key: &[u8], nonce: &[u8; 12], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    decrypt_with_aad(key, nonce, ciphertext, b"zer0-dbdeck credentials v1")
}

fn decrypt_with_aad(
    key: &[u8],
    nonce: &[u8; 12],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, String> {
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map_err(|_| "Не удалось подготовить расшифровку".to_owned())?;
    let cipher = aead::LessSafeKey::new(unbound);
    let mut output = ciphertext.to_vec();
    let plaintext = cipher
        .open_in_place(
            aead::Nonce::assume_unique_for_key(*nonce),
            aead::Aad::from(aad),
            &mut output,
        )
        .map_err(|_| "Не удалось расшифровать хранилище".to_owned())?;
    Ok(plaintext.to_vec())
}

pub(crate) fn write_secure_file(path: &PathBuf, data: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    let parent = path.parent().ok_or("Некорректный путь хранилища")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|_| "Не удалось создать каталог хранилища")?;
    let temporary = parent.join(format!(".dbdeck-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary)
        .map_err(|_| "Не удалось создать временный файл хранилища")?;
    let write = file
        .write_all(data)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temporary, path));
    if write.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err("Не удалось атомарно сохранить хранилище".to_owned());
    }
    // Publication already succeeded: don't roll back in-memory state after rename.
    let _ = fs::File::open(parent).and_then(|file| file.sync_all());
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn hex_decode(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err("Зашифрованные данные имеют неправильную длину".to_owned());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_digit(pair[0])?;
            let low = hex_digit(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_digit(digit: u8) -> Result<u8, String> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        b'A'..=b'F' => Ok(digit - b'A' + 10),
        _ => Err("Зашифрованные данные имеют неправильный формат".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    use super::{
        ConnectionProfile, CredentialVault, DatabaseKind, EnvironmentTag, SettingsStore, TlsMode,
        decrypt, derive_key, encrypt,
    };

    fn test_dir() -> PathBuf {
        std::env::temp_dir().join(format!("zer0-dbdeck-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn production_allows_each_transport_and_tls_key_permissions_are_checked() {
        use super::*;
        let mut profile:ConnectionProfile=serde_json::from_value(serde_json::json!({"id":"t","name":"t","kind":"postgre_sql","host":"localhost","port":5432,"database":"test","username":"u","environment":"prod","color":"#ffffff","read_only":true,"tls_mode":"disabled"})).unwrap();
        for kind in [
            DatabaseKind::PostgreSql,
            DatabaseKind::MySql,
            DatabaseKind::MariaDb,
        ] {
            profile.kind = kind;
            for mode in [TlsMode::Disabled, TlsMode::Require, TlsMode::VerifyFull] {
                profile.tls_mode = mode;
                profile.validate_transport().unwrap();
            }
        }
        let root = std::env::temp_dir().join(format!("dbdeck-tls-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let cert = root.join("test-cert.pem");
        let key = root.join("test-key.pem");
        fs::write(
            &cert,
            "synthetic path-validation fixture, not a certificate",
        )
        .unwrap();
        fs::write(&key, "synthetic path-validation fixture, not a private key").unwrap();
        profile.tls_files.client_cert = Some(cert);
        assert!(profile.validate_transport().is_err());
        profile.tls_files.client_key = Some(key.clone());
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(profile.validate_transport().is_err());
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        profile.validate_transport().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn engine_names_remain_compatible_and_mariadb_is_distinct() {
        assert!(matches!(
            serde_json::from_str::<DatabaseKind>(r#""my_sql""#).unwrap(),
            DatabaseKind::MySql
        ));
        for kind in [
            DatabaseKind::PostgreSql,
            DatabaseKind::MySql,
            DatabaseKind::MariaDb,
        ] {
            let encoded = serde_json::to_string(&kind).unwrap();
            let decoded: DatabaseKind = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded.label(), kind.label());
            assert_eq!(decoded.default_port(), kind.default_port());
        }
        assert_eq!(DatabaseKind::MariaDb.label(), "MariaDB");
        assert_ne!(
            serde_json::to_string(&DatabaseKind::MariaDb).unwrap(),
            serde_json::to_string(&DatabaseKind::MySql).unwrap()
        );
    }

    #[test]
    fn encrypted_payload_roundtrips_and_rejects_wrong_password() {
        let salt = [7_u8; 16];
        let nonce = [9_u8; 12];
        let key = derive_key("correct horse battery staple", &salt).unwrap();
        let ciphertext = encrypt(&key, &nonce, b"secret payload").unwrap();
        assert_eq!(
            decrypt(&key, &nonce, &ciphertext).unwrap(),
            b"secret payload"
        );
        let wrong_key = derive_key("wrong password", &salt).unwrap();
        assert!(decrypt(&wrong_key, &nonce, &ciphertext).is_err());
    }

    #[test]
    fn vault_roundtrips_across_lock_and_unlock() {
        let root = test_dir();
        let vault_path = root.join("credentials.vault");
        let mut first = CredentialVault {
            path: vault_path.clone(),
            key: None,
            salt: None,
            values: Default::default(),
        };
        first.create("correct horse battery staple").unwrap();
        first.save("profile-1", "db-password").unwrap();
        let mut reopened = CredentialVault {
            path: vault_path,
            key: None,
            salt: None,
            values: Default::default(),
        };
        assert!(reopened.unlock("incorrect master password").is_err());
        reopened.unlock("correct horse battery staple").unwrap();
        assert_eq!(reopened.password("profile-1").unwrap(), "db-password");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn portable_backup_restores_profiles_and_passwords() {
        let root = test_dir();
        let mut source = SettingsStore {
            sql_workspace: crate::sql_workspace::SqlWorkspace::default(),
            path: root.join("source/config/connections.json"),
            vault: CredentialVault {
                path: root.join("source/data/credentials.vault"),
                key: None,
                salt: None,
                values: Default::default(),
            },
            profiles: Vec::new(),
            sql_queries_directory: None,
            csv_export_directory: None,
        };
        source
            .unlock_or_create_vault("correct horse battery staple")
            .unwrap();
        let profile = ConnectionProfile {
            id: "profile-1".to_owned(),
            name: "Local test".to_owned(),
            kind: DatabaseKind::PostgreSql,
            host: "localhost".to_owned(),
            port: 5432,
            database: "test".to_owned(),
            username: "test".to_owned(),
            environment: EnvironmentTag::Local,
            color: "#e2c94f".to_owned(),
            read_only: false,
            tls_mode: TlsMode::Disabled,
            password_saved: false,
            gopass_entry: None,
            tls_files: Default::default(),
            native_tools_dir: None,
        };
        source.save_profile(profile, "db-password").unwrap();
        let backup = root.join("dbdeck-backup.json");
        source.export_backup(&backup).unwrap();
        let backup_text = fs::read_to_string(&backup).unwrap();
        assert!(!backup_text.contains("db-password"));
        assert!(!backup_text.contains("localhost"));
        assert_eq!(
            fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let vault_text = fs::read_to_string(&source.path).unwrap();
        assert!(!vault_text.contains("db-password"));

        let mut restored = SettingsStore {
            sql_workspace: crate::sql_workspace::SqlWorkspace::default(),
            path: root.join("restored/config/connections.json"),
            vault: CredentialVault {
                path: root.join("restored/data/credentials.vault"),
                key: None,
                salt: None,
                values: Default::default(),
            },
            profiles: Vec::new(),
            sql_queries_directory: None,
            csv_export_directory: None,
        };
        assert!(
            restored
                .import_backup(&backup, "incorrect master password")
                .is_err()
        );
        assert!(restored.profiles.is_empty());
        restored
            .import_backup(&backup, "correct horse battery staple")
            .unwrap();
        assert_eq!(restored.profiles.len(), 1);
        assert_eq!(restored.password("profile-1").unwrap(), "db-password");
        let _ = fs::remove_dir_all(root);
    }
    fn profile(id: &str) -> ConnectionProfile {
        ConnectionProfile {
            id: id.to_owned(),
            name: "Synthetic database".to_owned(),
            kind: super::DatabaseKind::PostgreSql,
            host: "private.example.test".to_owned(),
            port: 5432,
            database: "private_db".to_owned(),
            username: "synthetic-user".to_owned(),
            environment: super::EnvironmentTag::Local,
            color: "#4f8fe8".to_owned(),
            read_only: false,
            tls_mode: super::TlsMode::VerifyFull,
            password_saved: false,
            gopass_entry: None,
            tls_files: Default::default(),
            native_tools_dir: None,
        }
    }
    fn isolated(root: &std::path::Path) -> SettingsStore {
        SettingsStore::load_from(
            root.join("connections.dbdeck"),
            root.join("legacy/connections.json"),
            root.join("legacy/credentials.vault"),
        )
        .unwrap()
    }
    #[test]
    fn sql_workspace_is_encrypted_portable_and_atomic() {
        use crate::sql_workspace::{SqlDraft, SqlHistoryEntry};
        let root = test_dir();
        let mut source = isolated(&root.join("source"));
        source
            .unlock_or_create_vault("synthetic master password")
            .unwrap();
        source.save_profile(profile("sql"), "").unwrap();
        let sql = "SELECT 'private SQL marker Ω' AS value";
        let entry = SqlHistoryEntry {
            id: "execution".into(),
            profile_id: "sql".into(),
            connection_key: "sql-key".into(),
            name: "query".into(),
            sql: sql.into(),
            executed_at: 1,
            duration_ms: 5,
            success: true,
            row_count: 1,
            error: None,
        };
        source
            .set_sql_queries_directory(root.join("queries"))
            .unwrap();
        source.set_csv_export_directory(root.join("csv")).unwrap();
        source.record_sql(entry.clone()).unwrap();
        source
            .replace_sql_drafts(
                "sql",
                vec![SqlDraft {
                    id: "draft".into(),
                    profile_id: "sql".into(),
                    connection_key: "sql-key".into(),
                    name: "draft".into(),
                    sql: sql.into(),
                }],
            )
            .unwrap();
        let bytes = fs::read_to_string(source.storage_path()).unwrap();
        assert!(!bytes.contains("private SQL marker") && !bytes.contains(sql));
        let copy = root.join("copy");
        fs::create_dir_all(&copy).unwrap();
        fs::copy(source.storage_path(), copy.join("connections.dbdeck")).unwrap();
        let mut restored = isolated(&copy);
        assert!(restored.sql_workspace().history.is_empty());
        restored
            .unlock_with_key(&source.unlock_key().unwrap())
            .unwrap();
        assert_eq!(restored.sql_workspace().history[0].sql, sql);
        assert_eq!(restored.csv_export_directory(), Some(root.join("csv")));
        assert_eq!(restored.sql_queries_directory(), Some(root.join("queries")));
        assert_eq!(restored.sql_workspace().drafts[0].sql, sql);
        let backup = root.join("sql-backup.dbdeck");
        source.export_backup(&backup).unwrap();
        let mut imported = isolated(&root.join("imported"));
        imported
            .import_backup(&backup, "synthetic master password")
            .unwrap();
        assert_eq!(imported.sql_workspace().history[0].sql, sql);
        assert_eq!(imported.sql_workspace().drafts[0].sql, sql);
        assert_eq!(imported.csv_export_directory(), Some(root.join("csv")));
        let target = source.path.clone();
        source.path = root.join("blocked");
        fs::create_dir(&source.path).unwrap();
        let mut failed = entry;
        failed.id = "failed-write".into();
        assert!(source.record_sql(failed).is_err());
        assert_eq!(source.sql_workspace().history.len(), 1);
        assert!(source.replace_sql_drafts("sql", Vec::new()).is_err());
        assert!(
            source
                .set_csv_export_directory(root.join("failed"))
                .is_err()
        );
        assert_eq!(source.csv_export_directory(), Some(root.join("csv")));
        assert_eq!(source.sql_workspace().drafts.len(), 1);
        assert_eq!(fs::read_to_string(&target).unwrap(), bytes);
        let legacy: super::PortablePayload =
            serde_json::from_str(r#"{"connections":[],"passwords":{}}"#).unwrap();
        assert!(legacy.sql_workspace.history.is_empty());
        source.path = target;
        source.clear_sql_history().unwrap();
        assert!(source.sql_workspace().history.is_empty());
        assert_eq!(source.sql_workspace().drafts.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn canonical_file_is_encrypted_and_copies_alone_with_password_or_key() {
        let root = test_dir();
        let source_root = root.join("source");
        let mut source = isolated(&source_root);
        source
            .unlock_or_create_vault("synthetic master password")
            .unwrap();
        source
            .save_profile(profile("one"), "synthetic-db-password")
            .unwrap();
        let encrypted = fs::read_to_string(source.storage_path()).unwrap();
        for forbidden in [
            "private.example.test",
            "private_db",
            "synthetic-user",
            "synthetic-db-password",
            "Synthetic database",
        ] {
            assert!(!encrypted.contains(forbidden));
        }
        assert!(!source_root.join("legacy/connections.json").exists());
        assert!(!source.vault.path.exists());
        let copy = root.join("copy");
        fs::create_dir_all(&copy).unwrap();
        fs::copy(source.storage_path(), copy.join("connections.dbdeck")).unwrap();
        let mut restored = isolated(&copy);
        assert!(restored.profiles.is_empty());
        assert!(!restored.vault_is_unlocked());
        assert!(restored.unlock_or_create_vault("wrong password").is_err());
        assert!(restored.profiles.is_empty());
        restored
            .unlock_or_create_vault("synthetic master password")
            .unwrap();
        assert_eq!(restored.password("one").unwrap(), "synthetic-db-password");
        let mut auto = isolated(&copy);
        auto.unlock_with_key(&source.unlock_key().unwrap()).unwrap();
        assert_eq!(auto.profiles[0].host, "private.example.test");
        assert_eq!(auto.keyring_id().unwrap(), source.keyring_id().unwrap());
        assert!(auto.unlock_with_key(&[7; 32]).is_err());
        assert_eq!(auto.password("one").unwrap(), "synthetic-db-password");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn legacy_migration_preserves_sources_and_never_falls_back_after_corruption() {
        let root = test_dir();
        fs::create_dir_all(root.join("legacy")).unwrap();
        let mut legacy = isolated(&root);
        legacy.vault.create("legacy pass").unwrap();
        legacy.vault.save("one", "old-password").unwrap();
        let old_vault = fs::read(&legacy.vault.path).unwrap();
        let old_config = serde_json::to_vec(&super::ProfilesFile {
            version: 1,
            connections: vec![profile("one")],
            sql_queries_directory: None,
        })
        .unwrap();
        fs::write(root.join("legacy/connections.json"), &old_config).unwrap();
        let mut migrated = isolated(&root);
        assert_eq!(migrated.profiles.len(), 1);
        migrated.unlock_or_create_vault("legacy pass").unwrap();
        assert_eq!(migrated.password("one").unwrap(), "old-password");
        assert_eq!(
            fs::read(root.join("legacy/connections.json")).unwrap(),
            old_config
        );
        assert_eq!(fs::read(&legacy.vault.path).unwrap(), old_vault);
        fs::write(migrated.storage_path(), b"broken canonical file").unwrap();
        let mut broken = isolated(&root);
        assert!(broken.profiles.is_empty());
        assert!(broken.unlock_or_create_vault("legacy pass").is_err());
        assert!(broken.profiles.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn canonical_write_failure_rolls_back_profiles_and_passwords_together() {
        let root = test_dir();
        let mut store = isolated(&root);
        store
            .unlock_or_create_vault("synthetic master password")
            .unwrap();
        store
            .save_profile(profile("one"), "before-password")
            .unwrap();
        let old = fs::read(store.storage_path()).unwrap();
        let path = store.path.clone();
        store.path = root.join("blocked");
        fs::create_dir(&store.path).unwrap();
        assert!(
            store
                .update_profile(profile("one"), Some("after-password"))
                .is_err()
        );
        assert_eq!(store.password("one").unwrap(), "before-password");
        assert!(store.delete_profile("one").is_err());
        assert_eq!(store.profiles.len(), 1);
        assert!(
            store
                .save_profile(profile("two"), "second-password")
                .is_err()
        );
        assert_eq!(store.profiles.len(), 1);
        assert_eq!(fs::read(path).unwrap(), old);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn gopass_reference_survives_portability_without_duplicating_password() {
        let root = test_dir();
        let mut store = isolated(&root);
        store
            .unlock_or_create_vault("synthetic master password")
            .unwrap();
        store
            .save_profile(profile("one"), "old-local-value")
            .unwrap();
        let mut linked = profile("one");
        linked.gopass_entry = Some("db/synthetic".to_owned());
        store.update_profile(linked, None).unwrap();
        assert!(!store.vault.values.contains_key("one"));
        assert!(store.password("one").is_err());
        let backup = root.join("linked.dbdeck");
        store.export_backup(&backup).unwrap();
        let other_root = test_dir();
        let mut other = isolated(&other_root);
        other
            .unlock_or_create_vault("different synthetic master")
            .unwrap();
        other
            .save_profile(profile("one"), "stale-local-password")
            .unwrap();
        other
            .import_backup(&backup, "synthetic master password")
            .unwrap();
        assert!(!other.vault.values.contains_key("one"));
        assert!(other.password("one").is_err());
        fs::remove_dir_all(other_root).unwrap();
        let mut restored = isolated(&root);
        restored
            .unlock_or_create_vault("synthetic master password")
            .unwrap();
        assert_eq!(
            restored.profiles[0].gopass_entry.as_deref(),
            Some("db/synthetic")
        );
        let mut local = profile("one");
        local.name = "Local again".to_owned();
        restored.update_profile(local, None).unwrap();
        assert!(!restored.profiles[0].password_saved);
        fs::remove_dir_all(root).unwrap();
    }
}
