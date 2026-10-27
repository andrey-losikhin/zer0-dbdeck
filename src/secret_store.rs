//! External secret providers. Never pass secret values in argv or backend diagnostics to the UI.
use crate::settings::{ConnectionProfile, DatabaseKind, EnvironmentTag, TlsMode};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

pub struct Secret(pub Zeroizing<Vec<u8>>);
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[secret]")
    }
}
impl Secret {
    pub fn text(&self) -> Result<&str, String> {
        std::str::from_utf8(&self.0).map_err(|_| "Некорректная кодировка секрета".to_owned())
    }
}

pub struct Cli {
    gopass: PathBuf,
    keyring: PathBuf,
    timeout: Duration,
}
impl Default for Cli {
    fn default() -> Self {
        Self {
            gopass: "gopass".into(),
            keyring: "secret-tool".into(),
            timeout: Duration::from_secs(60),
        }
    }
}
impl Cli {
    fn run(
        &self,
        program: &std::path::Path,
        args: &[&str],
        input: Option<&[u8]>,
        deadline: Instant,
    ) -> Result<Secret, String> {
        if Instant::now() >= deadline {
            return Err("Истекло время ожидания хранилища".to_owned());
        }
        let mut child = Command::new(program)
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|_| {
                "Не удалось запустить менеджер паролей. Проверьте установку gopass/secret-tool"
                    .to_owned()
            })?;
        let pid = child.id() as i32;
        let mut output = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut bytes = Zeroizing::new(Vec::new());
            let result = output
                .by_ref()
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes);
            let _ = tx.send((result, bytes));
        });
        // Input is a 64-character derived key. Close stdin to complete secret-tool store.
        if let Some(data) = input {
            if child.stdin.take().unwrap().write_all(data).is_err() {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
                let _ = child.wait();
                return Err("Не удалось передать ключ в системную связку".to_owned());
            }
        }
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => break None,
            }
        };
        // Only this operation's process group, never the shared GPG agent.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = child.wait();
        let Some(status) = status else {
            return Err("Истекло время ожидания менеджера паролей".to_owned());
        };
        if !status.success() {
            return Err(
                "Менеджер паролей не открыл запись: проверьте путь и разблокировку связки/GPG"
                    .to_owned(),
            );
        }
        let (read, bytes) = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "Истекло время чтения секрета".to_owned())?;
        if read.is_err() || bytes.len() > 1024 * 1024 {
            return Err("Слишком большая или повреждённая запись".to_owned());
        }
        Ok(Secret(bytes))
    }
    pub fn lookup_key(&self, id: &str, provider: UnlockProvider) -> Result<Secret, String> {
        validate_key_id(id)?;
        let path = format!("dbdeck-unlock/{id}");
        let mut output = match provider {
            UnlockProvider::System => self.run(
                &self.keyring,
                &["lookup", "application", "io.zer0.dbdeck", "vault", id],
                None,
                Instant::now() + self.timeout,
            )?,
            UnlockProvider::Gopass => self.run(
                &self.gopass,
                &["show", "--password", "--nofuzzysearch", "--", &path],
                None,
                Instant::now() + self.timeout,
            )?,
        };
        while output.0.last().is_some_and(|c| *c == b'\n' || *c == b'\r') {
            output.0.pop();
        }
        if output.0.len() != 64 || !output.0.iter().all(u8::is_ascii_hexdigit) {
            return Err("В связке нет корректного ключа этого хранилища".to_owned());
        }
        let key = output
            .0
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        Ok(Secret(Zeroizing::new(key)))
    }
    pub fn remember_key(
        &self,
        id: &str,
        key: &[u8],
        provider: UnlockProvider,
    ) -> Result<(), String> {
        if key.len() != 32 {
            return Err("Некорректный ключ".to_owned());
        }
        let mut encoded = Zeroizing::new(String::with_capacity(64));
        for byte in key {
            use std::fmt::Write;
            write!(&mut *encoded, "{byte:02x}").unwrap();
        }
        validate_key_id(id)?;
        match provider {
            UnlockProvider::System => {
                self.run(
                    &self.keyring,
                    &[
                        "store",
                        "--label=DBDeck: автоматическая разблокировка",
                        "application",
                        "io.zer0.dbdeck",
                        "vault",
                        id,
                    ],
                    Some(encoded.as_bytes()),
                    Instant::now() + self.timeout,
                )?;
            }
            UnlockProvider::Gopass => {
                let path = format!("dbdeck-unlock/{id}");
                self.run(
                    &self.gopass,
                    &["insert", "--force", "--", &path],
                    Some(encoded.as_bytes()),
                    Instant::now() + self.timeout,
                )?;
            }
        }
        Ok(())
    }
    pub fn forget_key(&self, id: &str, provider: UnlockProvider) -> Result<(), String> {
        validate_key_id(id)?;
        match provider {
            UnlockProvider::System => {
                self.run(
                    &self.keyring,
                    &["clear", "application", "io.zer0.dbdeck", "vault", id],
                    None,
                    Instant::now() + self.timeout,
                )?;
            }
            UnlockProvider::Gopass => {
                let path = format!("dbdeck-unlock/{id}");
                self.run(
                    &self.gopass,
                    &["rm", "--force", "--", &path],
                    None,
                    Instant::now() + self.timeout,
                )?;
            }
        }
        Ok(())
    }
    fn show(&self, path: &str, deadline: Instant) -> Result<Secret, String> {
        self.run(
            &self.gopass,
            &[
                "show",
                "--noparsing",
                "--unsafe",
                "--nofuzzysearch",
                "--",
                path,
            ],
            None,
            deadline,
        )
    }
    fn manifest(&self, entry: &str, deadline: Instant) -> Result<(Manifest, Secret), String> {
        let mut bytes = vec![1];
        bytes.extend_from_slice(entry.as_bytes());
        let path = format!(
            ".zer0-waypass/v1/manifests/{}",
            URL_SAFE_NO_PAD.encode(bytes)
        );
        let raw = self.show(&path, deadline)?;
        let value: Manifest = serde_json::from_slice(&raw.0)
            .map_err(|_| "Повреждён manifest zer0-waypass".to_owned())?;
        value.validate()?;
        Ok((value, raw))
    }
    fn field(&self, m: &Manifest, field: &Field, deadline: Instant) -> Result<Secret, String> {
        let path = format!(
            ".zer0-waypass/v1/{}/{}/{}",
            m.bundle_id, m.revision, field.id
        );
        let mut value = self.show(&path, deadline)?;
        // gopass appends one newline to piped output. Preserve spaces and original content.
        if value.0.last() == Some(&b'\n') {
            value.0.pop();
        }
        if !field.multiline && value.0.iter().any(|b| *b == b'\n' || *b == b'\r') {
            return Err("Многострочное значение в однострочном поле gopass".to_owned());
        }
        Ok(value)
    }
    pub fn password(&self, entry: &str) -> Result<Secret, String> {
        validate_entry(entry)?;
        let deadline = Instant::now() + self.timeout;
        let main = self.show(entry, deadline)?;
        if is_bundle(main.text()?) {
            let (manifest, before) = self.manifest(entry, deadline)?;
            let field = manifest
                .fields
                .iter()
                .find(|f| f.kind == "password")
                .ok_or("В gopass нет поля Password")?;
            let secret = self.field(&manifest, field, deadline)?;
            let (_, after) = self.manifest(entry, deadline)?;
            if before.0 != after.0 {
                return Err(
                    "Запись gopass изменилась во время чтения. Повторите подключение".to_owned(),
                );
            }
            Ok(secret)
        } else {
            let value = main
                .text()?
                .split('\n')
                .next()
                .unwrap_or("")
                .strip_suffix('\r')
                .unwrap_or(main.text()?.split('\n').next().unwrap_or(""));
            Ok(Secret(Zeroizing::new(value.as_bytes().to_vec())))
        }
    }
    pub fn import_connection(&self, entry: &str) -> Result<ConnectionProfile, String> {
        validate_entry(entry)?;
        let deadline = Instant::now() + self.timeout;
        let main = self.show(entry, deadline)?;
        let mut values = HashMap::new();
        if is_bundle(main.text()?) {
            let (manifest, before) = self.manifest(entry, deadline)?;
            if !manifest.fields.iter().any(|f| f.kind == "password") {
                return Err("В gopass нет Password".to_owned());
            }
            for field in &manifest.fields {
                if matches!(
                    field.kind.as_str(),
                    "engine" | "host" | "port" | "database" | "username"
                ) {
                    let value = self.field(&manifest, field, deadline)?;
                    values.insert(field.kind.clone(), value.text()?.to_owned());
                }
            }
            let (_, after) = self.manifest(entry, deadline)?;
            if before.0 != after.0 {
                return Err("Запись gopass изменилась. Повторите импорт".to_owned());
            }
        } else {
            for line in main.text()?.lines().skip(1) {
                if let Some((key, value)) = line.split_once(':') {
                    if matches!(
                        key.trim(),
                        "engine" | "host" | "port" | "database" | "username"
                    ) {
                        values.insert(key.trim().to_owned(), value.trim().to_owned());
                    }
                }
            }
        }
        let engine = values
            .get("engine")
            .ok_or("Добавьте поле Engine в gopass (postgresql/mysql/mariadb)")?
            .to_lowercase();
        let kind = match engine.as_str() {
            "postgres" | "postgresql" => DatabaseKind::PostgreSql,
            "mysql" => DatabaseKind::MySql,
            "mariadb" => DatabaseKind::MariaDb,
            _ => return Err("Неподдерживаемый Engine в gopass".to_owned()),
        };
        let host = values
            .remove("host")
            .filter(|v| !v.trim().is_empty())
            .ok_or("Добавьте Host в gopass")?;
        let username = values
            .remove("username")
            .filter(|v| !v.trim().is_empty())
            .ok_or("Добавьте Username в gopass")?;
        let port = values
            .get("port")
            .map(|v| {
                v.parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or("Некорректный Port в gopass")
            })
            .transpose()?
            .unwrap_or(kind.default_port());
        Ok(ConnectionProfile {
            id: uuid::Uuid::new_v4().to_string(),
            name: entry.to_owned(),
            kind,
            host,
            port,
            database: values.remove("database").unwrap_or_default(),
            username,
            environment: EnvironmentTag::Local,
            color: "#4f8fe8".to_owned(),
            read_only: true,
            tls_mode: TlsMode::VerifyFull,
            password_saved: true,
            gopass_entry: Some(entry.to_owned()),
            tls_files: Default::default(),
            native_tools_dir: None,
        })
    }
}

pub fn validate_entry(entry: &str) -> Result<(), String> {
    if entry.is_empty()
        || entry.len() > 4096
        || entry.starts_with('/')
        || entry == ".zer0-waypass"
        || entry.starts_with(".zer0-waypass/")
        || entry.contains('\\')
        || entry.chars().any(char::is_control)
        || entry
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == ".." || s.starts_with('-'))
    {
        return Err("Некорректный путь записи gopass".to_owned());
    }
    Ok(())
}
fn is_bundle(text: &str) -> bool {
    text.lines().skip(1).any(|line| {
        matches!(
            line,
            "zer0-waypass: field bundle" | "zer0-waypass field bundle"
        )
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    bundle_id: String,
    revision: String,
    fields: Vec<Field>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Field {
    id: String,
    kind: String,
    name: String,
    visibility: String,
    multiline: bool,
}
fn valid_token(value: &str) -> bool {
    URL_SAFE_NO_PAD
        .decode(value)
        .is_ok_and(|v| v.len() == 16 && URL_SAFE_NO_PAD.encode(v) == value)
}
impl Manifest {
    fn validate(&self) -> Result<(), String> {
        if self.format != "zer0-waypass/fields-v1"
            || !valid_token(&self.bundle_id)
            || !valid_token(&self.revision)
            || self.fields.is_empty()
            || self.fields.len() > 256
        {
            return Err("Некорректный manifest gopass".to_owned());
        }
        let mut ids = HashSet::new();
        let mut kinds = HashSet::new();
        let mut names = HashSet::new();
        for f in &self.fields {
            let secret = matches!(
                f.kind.as_str(),
                "password"
                    | "api_key"
                    | "token"
                    | "client_secret"
                    | "private_key"
                    | "passphrase"
                    | "sudo_password"
                    | "totp_secret"
                    | "recovery_codes"
            );
            let public = matches!(
                f.kind.as_str(),
                "username"
                    | "url"
                    | "email"
                    | "notes"
                    | "host"
                    | "port"
                    | "database"
                    | "engine"
                    | "dsn"
                    | "client_id"
                    | "jump_host"
            );
            let multiline = matches!(f.kind.as_str(), "notes" | "private_key" | "recovery_codes");
            if !valid_token(&f.id)
                || !ids.insert(&f.id)
                || f.name.trim().is_empty()
                || !names.insert(f.name.to_lowercase())
                || (f.kind != "custom"
                    && (!kinds.insert(&f.kind)
                        || !(secret || public)
                        || f.multiline != multiline
                        || f.visibility != if secret { "secret" } else { "public" }))
                || (f.kind == "custom" && !matches!(f.visibility.as_str(), "secret" | "public"))
            {
                return Err("Некорректные поля manifest gopass".to_owned());
            }
        }
        Ok(())
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachinePreferences {
    remember_unlock: bool,
    #[serde(default)]
    provider: UnlockProvider,
    #[serde(default)]
    device_id: Option<String>,
}
fn preferences_path() -> Result<PathBuf, String> {
    directories::ProjectDirs::from("io", "zer0", "zer0-dbdeck")
        .map(|p| p.config_dir().join("security.json"))
        .ok_or_else(|| "Нет каталога настроек".to_owned())
}
fn preferences() -> MachinePreferences {
    preferences_path()
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}
fn write_preferences(prefs: &MachinePreferences) -> Result<(), String> {
    crate::settings::write_secure_file(
        &preferences_path()?,
        &serde_json::to_vec(prefs).map_err(|_| "Не удалось сохранить настройку")?,
    )
}
pub fn remember_enabled() -> bool {
    preferences().remember_unlock
}
pub fn unlock_provider() -> UnlockProvider {
    preferences().provider
}

pub fn scoped_key_id(vault_id: &str, create: bool) -> Result<String, String> {
    validate_key_id(vault_id)?;
    let mut prefs = preferences();
    if prefs.device_id.is_none() && create {
        prefs.device_id = Some(uuid::Uuid::new_v4().to_string());
        write_preferences(&prefs)?;
    }
    let device = prefs
        .device_id
        .ok_or("На этом компьютере ещё не запоминали ключ")?;
    uuid::Uuid::parse_str(&device).map_err(|_| "Локальная привязка ключа повреждена")?;
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        format!("dbdeck:{vault_id}:{device}").as_bytes(),
    );
    Ok(digest.as_ref().iter().map(|b| format!("{b:02x}")).collect())
}
pub fn set_remember_enabled(enabled: bool) -> Result<(), String> {
    let mut p = preferences();
    p.remember_unlock = enabled;
    write_preferences(&p)
}
pub fn set_unlock_provider(provider: UnlockProvider) -> Result<(), String> {
    let mut p = preferences();
    if p.remember_unlock {
        return Err("Сначала отключите автоматическую разблокировку".to_owned());
    }
    p.provider = provider;
    write_preferences(&p)
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UnlockProvider {
    System,
    #[default]
    Gopass,
}
impl UnlockProvider {
    pub fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Gopass => "gopass",
        }
    }
}
fn validate_key_id(id: &str) -> Result<(), String> {
    if id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("Некорректный ID хранилища".to_owned())
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt;
    struct Fixture {
        root: PathBuf,
        cli: Cli,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("dbdeck-secret-cli-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            let program = root.join("fake-cli");
            std::fs::write(&program,r#"#!/usr/bin/python3
import sys,json,pathlib,time,os
root=pathlib.Path(__file__).parent
args=sys.argv[1:]
with (root/'argv.jsonl').open('a') as f:f.write(json.dumps(args)+'\n')
if (root/'hang').exists():
    pid=os.fork()
    (root/('child.pid' if pid==0 else 'parent.pid')).write_text(str(os.getpid()))
    time.sleep(30)
if (root/'oversize').exists():
    sys.stdout.write('x'*(1024*1024+2));sys.exit(0)
data=json.loads((root/'data.json').read_text())
if args[0]=='store':
    assert args[2:5]==['application','io.zer0.dbdeck','vault']
    (root/'system.key').write_bytes(sys.stdin.buffer.read());sys.exit(0)
if args[0]=='lookup':
    if not (root/'system.key').exists():sys.exit(4)
    sys.stdout.buffer.write((root/'system.key').read_bytes()+b'\n');sys.exit(0)
if args[0]=='clear':
    (root/'system.key').unlink();sys.exit(0)
path=args[args.index('--')+1]
if args[0]=='insert':
    assert args[:3]==['insert','--force','--']
    data[path]=sys.stdin.read();(root/'data.json').write_text(json.dumps(data));sys.exit(0)
if args[0]=='rm':
    assert args[:3]==['rm','--force','--']
    del data[path];(root/'data.json').write_text(json.dumps(data));sys.exit(0)
assert args[0]=='show'
if path not in data:
    sys.stderr.write('synthetic-sensitive-diagnostic');sys.exit(7)
value=data[path]
if (root/'flip').exists() and path.startswith('.zer0-waypass/v1/manifests/'):
    counter=root/'counter';n=int(counter.read_text()) if counter.exists() else 0;counter.write_text(str(n+1))
    if n>0:
        manifest=json.loads(value);manifest['revision']='DDDDDDDDDDDDDDDDDDDDDA';value=json.dumps(manifest)
if '--password' in args:value=value.split('\n')[0]
sys.stdout.write(value+'\n')
"#).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(root.join("data.json"), "{}").unwrap();
            Self {
                root,
                cli: Cli {
                    gopass: program.clone(),
                    keyring: program,
                    timeout: Duration::from_secs(2),
                },
            }
        }
        fn data(&self, value: Value) {
            std::fs::write(
                self.root.join("data.json"),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
        }
        fn calls(&self) -> String {
            std::fs::read_to_string(self.root.join("argv.jsonl")).unwrap_or_default()
        }
        fn bundle(&self, password: &str) -> String {
            let entry = "db/synthetic";
            let mut id = vec![1];
            id.extend_from_slice(entry.as_bytes());
            let manifest_path =
                format!(".zer0-waypass/v1/manifests/{}", URL_SAFE_NO_PAD.encode(id));
            let kinds = ["password", "engine", "host", "port", "database", "username"];
            let mut fields = Vec::new();
            let mut data = serde_json::Map::new();
            let values = [
                password,
                "postgresql",
                "db.example.test",
                "5432",
                "synthetic_db",
                "synthetic_user",
            ];
            for (i, kind) in kinds.iter().enumerate() {
                let token = URL_SAFE_NO_PAD.encode([i as u8; 16]);
                fields.push(json!({"id":token,"kind":kind,"name":kind,"visibility":if i==0{"secret"}else{"public"},"multiline":false}));
                data.insert(
                    format!(
                        ".zer0-waypass/v1/{}/{}/{token}",
                        URL_SAFE_NO_PAD.encode([8; 16]),
                        URL_SAFE_NO_PAD.encode([9; 16])
                    ),
                    json!(values[i]),
                );
            }
            data.insert(
                entry.to_owned(),
                json!("stale-compatibility-password\nzer0-waypass: field bundle"),
            );
            data.insert(manifest_path.clone(),json!(json!({"format":"zer0-waypass/fields-v1","bundle_id":URL_SAFE_NO_PAD.encode([8;16]),"revision":URL_SAFE_NO_PAD.encode([9;16]),"fields":fields}).to_string()));
            self.data(Value::Object(data));
            manifest_path
        }
    }
    #[test]
    fn key_providers_roundtrip_via_stdin_without_secret_argv_or_diagnostics() {
        let fixture = Fixture::new();
        let id = "a".repeat(64);
        let key = [42; 32];
        for provider in [UnlockProvider::System, UnlockProvider::Gopass] {
            fixture.cli.remember_key(&id, &key, provider).unwrap();
            assert_eq!(&*fixture.cli.lookup_key(&id, provider).unwrap().0, &key);
            let encoded = "2a".repeat(32);
            assert!(!fixture.calls().contains(&encoded));
            fixture.cli.forget_key(&id, provider).unwrap();
            let error = fixture.cli.lookup_key(&id, provider).unwrap_err();
            assert!(!error.contains("synthetic-sensitive"));
        }
        assert!(
            fixture
                .cli
                .remember_key("../../bad", &key, UnlockProvider::Gopass)
                .is_err()
        );
    }
    #[test]
    fn structured_records_use_live_password_sidecar_and_import_public_database_fields() {
        let fixture = Fixture::new();
        fixture.bundle("live-password-one");
        assert_eq!(
            fixture
                .cli
                .password("db/synthetic")
                .unwrap()
                .text()
                .unwrap(),
            "live-password-one"
        );
        let profile = fixture.cli.import_connection("db/synthetic").unwrap();
        assert_eq!(profile.host, "db.example.test");
        assert_eq!(profile.username, "synthetic_user");
        assert_eq!(profile.database, "synthetic_db");
        assert_eq!(profile.port, 5432);
        assert_eq!(profile.gopass_entry.as_deref(), Some("db/synthetic"));
        fixture.bundle("live-password-two");
        assert_eq!(
            fixture
                .cli
                .password("db/synthetic")
                .unwrap()
                .text()
                .unwrap(),
            "live-password-two"
        );
        assert!(!fixture.calls().contains("live-password"));
    }
    #[test]
    fn structured_manifest_changes_or_malformed_fields_never_fall_back_to_compatibility_password() {
        let fixture = Fixture::new();
        let manifest = fixture.bundle("live");
        std::fs::write(fixture.root.join("flip"), b"1").unwrap();
        assert!(fixture.cli.password("db/synthetic").is_err());
        std::fs::remove_file(fixture.root.join("flip")).unwrap();
        let mut data: Value =
            serde_json::from_slice(&std::fs::read(fixture.root.join("data.json")).unwrap())
                .unwrap();
        let mut parsed: Value = serde_json::from_str(data[&manifest].as_str().unwrap()).unwrap();
        parsed["fields"][0]["value"] = json!("forbidden");
        data[&manifest] = json!(parsed.to_string());
        fixture.data(data);
        let error = fixture.cli.password("db/synthetic").unwrap_err();
        assert!(!error.contains("stale-compatibility-password"));
    }
    #[test]
    fn legacy_and_path_validation_preserve_password_spaces_and_reject_injection() {
        let fixture = Fixture::new();
        fixture.data(json!({"legacy/synthetic":"  spaced password  \nusername: alice"}));
        for path in [
            "",
            "../x",
            "/x",
            "x/../y",
            "-x",
            "x//y",
            ".zer0-waypass/v1/x",
            "a\nb",
            "a\\b",
        ] {
            assert!(fixture.cli.password(path).is_err());
        }
        assert!(fixture.calls().is_empty());
        assert_eq!(
            fixture
                .cli
                .password("legacy/synthetic")
                .unwrap()
                .text()
                .unwrap(),
            "  spaced password  "
        );
        assert_eq!(
            format!("{:?}", fixture.cli.password("legacy/synthetic").unwrap()),
            "[secret]"
        );
    }
    #[test]
    fn backend_timeout_kills_its_process_group_and_caps_output() {
        let mut fixture = Fixture::new();
        fixture.cli.timeout = Duration::from_millis(400);
        std::fs::write(fixture.root.join("hang"), b"1").unwrap();
        let start = Instant::now();
        assert!(fixture.cli.password("legacy/synthetic").is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
        for name in ["parent.pid", "child.pid"] {
            let pid = std::fs::read_to_string(fixture.root.join(name)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
                if stat.is_err() || stat.unwrap().split_once(") ").unwrap().1.starts_with('Z') {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "backend descendant survived cancellation"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        std::fs::remove_file(fixture.root.join("hang")).unwrap();
        std::fs::write(fixture.root.join("oversize"), b"1").unwrap();
        assert!(fixture.cli.password("legacy/synthetic").is_err());
    }
}
