//! Native database dumps: streaming stdout, bounded diagnostics, private credentials.
use crate::{
    output_file::AtomicOutput,
    settings::{ConnectionProfile, DatabaseKind, TlsMode},
    transfer::Cancellation,
};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Clone, Copy, Debug)]
pub enum DumpMode {
    Schema,
    Full,
}
impl DumpMode {
    pub fn extension(self, kind: DatabaseKind) -> &'static str {
        if matches!((self, kind), (Self::Full, DatabaseKind::PostgreSql)) {
            "dump"
        } else {
            "sql"
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Schema => "Структура БД без данных",
            Self::Full => "Полная резервная копия БД",
        }
    }
}
struct Credentials {
    directory: PathBuf,
    file: PathBuf,
}
impl Credentials {
    fn new(contents: &str) -> Result<Self, String> {
        let directory = std::env::temp_dir().join(format!("dbdeck-auth-{}", uuid::Uuid::new_v4()));
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| "Не удалось создать защищённый каталог credentials")?;
        let result = Self {
            file: directory.join("credentials"),
            directory,
        };
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&result.file)
            .map_err(|_| "Не удалось создать credentials")?;
        file.write_all(contents.as_bytes())
            .map_err(|_| "Не удалось записать credentials")?;
        Ok(result)
    }
}
impl Drop for Credentials {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.file);
        let _ = fs::remove_dir(&self.directory);
    }
}
fn pg_value(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}
fn option_value(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}
fn command(
    profile: &ConnectionProfile,
    password: &str,
    mode: DumpMode,
    program: &Path,
    maria: bool,
) -> Result<(Command, Credentials), String> {
    build_command(profile, password, mode, program, maria, None)
}

fn client_program(profile: &ConnectionProfile, name: &str) -> Result<PathBuf, String> {
    match &profile.native_tools_dir {
        Some(directory) if directory.is_absolute() && directory.is_dir() => {
            Ok(directory.join(name))
        }
        Some(_) => {
            Err("Каталог утилит БД: укажите абсолютный путь к существующему каталогу".into())
        }
        None => Ok(PathBuf::from(name)),
    }
}

fn validate_client_version(version: &str, maria: bool, program: &Path) -> Result<(), String> {
    let version = version.to_ascii_lowercase();
    if version.contains("mariadb") != maria || (!maria && !version.contains("mysql")) {
        return Err(format!(
            "Утилита {} не соответствует серверу {}. Укажите каталог штатных утилит этой СУБД в настройках подключения; MySQL и MariaDB не взаимозаменяемы для полного бэкапа/восстановления.",
            program.display(),
            if maria { "MariaDB" } else { "MySQL" }
        ));
    }
    Ok(())
}

fn check_mysql_client(program: &Path, maria: bool) -> Result<(), String> {
    let output = Command::new(program)
        .arg("--version")
        .output()
        .map_err(|e| format!("Не найдена утилита {}: {e}", program.display()))?;
    if !output.status.success() {
        return Err(format!("Не удалось проверить версию {}", program.display()));
    }
    validate_client_version(&String::from_utf8_lossy(&output.stdout), maria, program)
}

/// MySQL bug #105990: a stored trigger created via multi-statements retains
/// its terminal semicolon. mysqldump wraps it in a version comment that cannot
/// be restored. Unwrap only the known CREATE/DEFINER/TRIGGER wrappers; keep the
/// actual definition byte-for-byte, including its delimiter and semicolon.
fn mysql_code_boundary(bytes: &[u8], escapes: bool) -> bool {
    let mut quote = None;
    let mut line = false;
    let mut block = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if line {
            if b == b'\n' {
                line = false;
            }
            i += 1;
            continue;
        }
        if block {
            if bytes.get(i..i + 2) == Some(b"*/") {
                block = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if let Some(q) = quote {
            if escapes && b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                if bytes.get(i + 1) == Some(&q) {
                    i += 2;
                    continue;
                }
                quote = None;
            }
        } else if [b'\'', b'"', b'`'].contains(&b) {
            quote = Some(b);
        } else if b == b'#'
            || (bytes.get(i..i + 2) == Some(b"--")
                && bytes.get(i + 2).is_some_and(u8::is_ascii_whitespace))
        {
            line = true;
        } else if bytes.get(i..i + 2) == Some(b"/*") {
            block = true;
            i += 2;
            continue;
        }
        i += 1;
    }
    quote.is_none() && !line && !block
}

fn mysql_trigger_dump(
    input: impl BufRead,
    mut output: impl Write,
    cancelled: &Cancellation,
) -> Result<(), String> {
    let mut input = input;
    let mut chunk = Vec::new();
    let mut continuation = false;
    loop {
        if cancelled.is_cancelled() {
            return Err("Операция отменена".into());
        }
        chunk.clear();
        let n = input
            .by_ref()
            .take(8192)
            .read_until(b'\n', &mut chunk)
            .map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if !continuation && chunk.starts_with(b"/*!50003 CREATE*/") {
            let start = b"/*!50003 CREATE*/".len();
            let rest = &chunk[start..];
            let definer = rest
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .unwrap_or(rest.len());
            if !rest[definer..].starts_with(b"/*!50017 DEFINER=") {
                return Err("Неизвестный формат CREATE TRIGGER в mysqldump".into());
            }
            // Definer identifiers are backtick quoted; */ inside one is data.
            let mut quoted = false;
            let mut end = None;
            let mut i = definer + b"/*!50017 ".len();
            while i + 1 < rest.len() {
                if rest[i] == b'`' {
                    if quoted && rest[i + 1] == b'`' {
                        i += 2;
                        continue;
                    }
                    quoted = !quoted;
                }
                if !quoted && &rest[i..i + 2] == b"*/" {
                    end = Some(i);
                    break;
                }
                i += 1;
            }
            let end = end.ok_or("Слишком длинный или повреждённый DEFINER в mysqldump")?;
            let tail = &rest[end + 2..];
            let trigger = tail
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .unwrap_or(tail.len());
            let body = tail[trigger..]
                .strip_prefix(b"/*!50003 ")
                .filter(|v| v.starts_with(b"TRIGGER "))
                .ok_or("Неизвестный формат TRIGGER в mysqldump")?;
            output.write_all(b"CREATE ").map_err(|e| e.to_string())?;
            output
                .write_all(&rest[definer + b"/*!50017 ".len()..end])
                .map_err(|e| e.to_string())?;
            output.write_all(b" ").map_err(|e| e.to_string())?;
            let mut definition = body.to_vec();
            let mut candidates = 0;
            loop {
                let trimmed = definition.strip_suffix(b"\n").unwrap_or(&definition);
                if let Some(body) = trimmed.strip_suffix(b" */;;") {
                    candidates += 1;
                    if candidates > 64 {
                        return Err("Неоднозначные границы определения триггера в mysqldump".into());
                    }
                    let boundary = mysql_code_boundary(body, true);
                    let no_escapes = mysql_code_boundary(body, false);
                    if boundary != no_escapes {
                        return Err("Неоднозначные кавычки/SQL mode в определении триггера; используйте штатные инструменты с проверкой восстановления".into());
                    }
                    if boundary {
                        output
                            .write_all(body)
                            .and_then(|_| output.write_all(b" ;;\n"))
                            .map_err(|e| e.to_string())?;
                        break;
                    }
                }
                if cancelled.is_cancelled() {
                    return Err("Операция отменена".into());
                }
                if definition.len() > 16 * 1024 * 1024 {
                    return Err("Определение триггера превышает 16 МиБ; используйте штатный инструмент с проверкой восстановления".into());
                }
                chunk.clear();
                if input
                    .by_ref()
                    .take(8192)
                    .read_until(b'\n', &mut chunk)
                    .map_err(|e| e.to_string())?
                    == 0
                {
                    return Err("Незавершённое определение триггера в mysqldump".into());
                }
                definition.extend_from_slice(&chunk);
            }
            continuation = false;
        } else {
            continuation = chunk.last() != Some(&b'\n');
            output.write_all(&chunk).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn build_command(
    profile: &ConnectionProfile,
    password: &str,
    mode: DumpMode,
    program: &Path,
    maria: bool,
    restore: Option<bool>,
) -> Result<(Command, Credentials), String> {
    profile.validate_transport()?;
    if restore.is_some() && profile.read_only {
        return Err("Подключение доступно только для чтения".into());
    }
    if profile.database.trim().is_empty() {
        return Err("Для резервной копии выберите конкретную базу данных".into());
    }
    let mut cmd = Command::new(program);
    cmd.env_remove("PGPASSWORD")
        .env_remove("PGOPTIONS")
        .env_remove("PGSERVICE")
        .env_remove("MYSQL_PWD");
    let credentials = match profile.kind {
        DatabaseKind::PostgreSql => {
            if [
                &profile.host,
                &profile.database,
                &profile.username,
                &password.to_string(),
            ]
            .iter()
            .any(|s| s.contains(['\n', '\r']))
            {
                return Err(
                    "Клиенты PostgreSQL: переводы строк в реквизитах не поддерживаются pgpass"
                        .into(),
                );
            }
            let pass_field = |s: &str| {
                s.replace('\\', "\\\\")
                    .replace(':', "\\:")
                    .replace('*', "\\*")
            };
            let credentials = Credentials::new(&zeroize::Zeroizing::new(format!(
                "{}:{}:{}:{}:{}\n",
                pass_field(&profile.host),
                profile.port,
                pass_field(&profile.database),
                pass_field(&profile.username),
                pass_field(password)
            )))?;
            let database = &profile.database;
            let ssl = match profile.tls_mode {
                TlsMode::Disabled => "disable",
                TlsMode::Require => "require",
                TlsMode::VerifyFull => "verify-full",
            };
            let mut connection = format!(
                "host={} port={} dbname={} user={} sslmode={} connect_timeout=10",
                pg_value(&profile.host),
                profile.port,
                pg_value(database),
                pg_value(&profile.username),
                ssl
            );
            for (key, path) in [
                ("sslrootcert", &profile.tls_files.ca),
                ("sslcert", &profile.tls_files.client_cert),
                ("sslkey", &profile.tls_files.client_key),
            ] {
                if let Some(path) = path {
                    connection.push_str(&format!(" {key}={}", pg_value(&path.to_string_lossy())));
                }
            }
            cmd.env("PGPASSFILE", &credentials.file)
                .arg("--no-password")
                .arg(format!("--dbname={connection}"));
            if let Some(archive) = restore {
                if archive {
                    cmd.args(["--exit-on-error", "--single-transaction"]);
                } else {
                    cmd.args([
                        "--no-psqlrc",
                        "--set=ON_ERROR_STOP=1",
                        "--single-transaction",
                        "--file=-",
                    ]);
                }
            } else {
                match mode {
                    DumpMode::Schema => {
                        cmd.args(["--schema-only", "--format=plain"]);
                    }
                    DumpMode::Full => {
                        cmd.arg("--format=custom");
                    }
                }
            }
            credentials
        }
        DatabaseKind::MySql | DatabaseKind::MariaDb => {
            if profile.database.is_empty() {
                return Err("Укажите имя базы данных в подключении".into());
            }
            let credentials = Credentials::new(&zeroize::Zeroizing::new(format!(
                "[client]\npassword={}\n",
                option_value(password)
            )))?;
            // Must be the first option; never read the user's unrelated defaults.
            cmd.arg(format!("--defaults-file={}", credentials.file.display()));
            // Prevent .mylogin.cnf from overriding the explicit target. The env
            // override also works with clients predating --no-login-paths.
            cmd.env(
                "MYSQL_TEST_LOGIN_FILE",
                credentials.directory.join("no-login-path"),
            );
            if restore.is_none() {
                cmd.args([
                    "--protocol=TCP",
                    "--quick",
                    "--single-transaction",
                    "--routines",
                    "--events",
                    "--triggers",
                    "--hex-blob",
                    "--default-character-set=utf8mb4",
                ]);
            } else {
                cmd.args([
                    "--protocol=TCP",
                    "--batch",
                    "--binary-mode",
                    "--quick",
                    "--skip-reconnect",
                    "--local-infile=0",
                    "--default-character-set=utf8mb4",
                ]);
            }
            cmd.arg(format!("--host={}", profile.host))
                .arg(format!("--port={}", profile.port))
                .arg(format!("--user={}", profile.username));
            if restore.is_some() {
                cmd.arg("--connect-timeout=10");
            }
            for (key, path) in [
                ("ssl-ca", &profile.tls_files.ca),
                ("ssl-cert", &profile.tls_files.client_cert),
                ("ssl-key", &profile.tls_files.client_key),
            ] {
                if let Some(path) = path {
                    cmd.arg(format!("--{key}={}", path.display()));
                }
            }
            if maria {
                match profile.tls_mode {
                    TlsMode::Disabled => {
                        cmd.arg("--skip-ssl");
                    }
                    TlsMode::Require => {
                        cmd.args(["--ssl", "--skip-ssl-verify-server-cert"]);
                    }
                    TlsMode::VerifyFull => {
                        cmd.args(["--ssl", "--ssl-verify-server-cert"]);
                    }
                }
            } else {
                if restore.is_none() {
                    cmd.args([
                        "--column-statistics=0",
                        "--set-gtid-purged=OFF",
                        "--no-tablespaces",
                    ]);
                }
                cmd.arg(match profile.tls_mode {
                    TlsMode::Disabled => "--ssl-mode=DISABLED",
                    TlsMode::Require => "--ssl-mode=REQUIRED",
                    TlsMode::VerifyFull => "--ssl-mode=VERIFY_IDENTITY",
                });
            }
            if restore.is_none() && matches!(mode, DumpMode::Schema) {
                cmd.arg("--no-data");
            }
            if restore.is_some() {
                cmd.arg(format!("--database={}", profile.database));
            } else {
                // Do not issue CREATE DATABASE / USE. The dump header retains source identity
                // because view/routine definitions may still qualify the source DB.
                cmd.arg("--").arg(&profile.database);
            }
            credentials
        }
    };
    Ok((cmd, credentials))
}
fn run(
    cmd: &mut Command,
    output: Option<&AtomicOutput>,
    cancelled: &Cancellation,
    mut progress: impl FnMut(u64),
) -> Result<String, String> {
    if cancelled.is_cancelled() {
        return Err("Операция отменена".into());
    }
    let stdout = match output {
        Some(output) => Stdio::from(output.file.try_clone().map_err(|e| e.to_string())?),
        None => Stdio::null(),
    };
    let mut child=cmd.stdout(stdout).stderr(Stdio::piped()).process_group(0).spawn()
        .map_err(|e|format!("Не удалось запустить клиент БД: {e}. Установите PostgreSQL/MariaDB/MySQL client tools подходящей версии."))?;
    let pid = child.id() as i32;
    let mut stderr = child.stderr.take().ok_or("Нет stderr процесса")?;
    let reader = std::thread::spawn(move || {
        let mut result = zeroize::Zeroizing::new(Vec::new());
        let mut buffer = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buffer) {
            if n == 0 {
                break;
            }
            let take = n.min((16 * 1024usize).saturating_sub(result.len()));
            result.extend_from_slice(&buffer[..take]);
        }
        result
    });
    let status = loop {
        if cancelled.is_cancelled() {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            let _ = child.wait();
            let _ = reader.join();
            return Err("Операция отменена".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                progress(output.map_or(0, AtomicOutput::bytes));
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(error) => {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
                let _ = child.wait();
                let _ = reader.join();
                return Err(format!("Ошибка клиента БД: {error}"));
            }
        }
    };
    // Reap descendants as well, so a leaked stderr FD cannot stall completion.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let bytes = reader
        .join()
        .map_err(|_| "Не удалось прочитать сообщения утилиты")?;
    let diagnostics = String::from_utf8_lossy(&bytes).into_owned();
    if !status.success() {
        return Err(format!(
            "Утилита завершилась с ошибкой ({status}): {diagnostics}"
        ));
    }
    if cancelled.is_cancelled() {
        return Err("Операция отменена".into());
    }
    progress(output.map_or(0, AtomicOutput::bytes));
    Ok(diagnostics)
}

pub fn dump(
    profile: &ConnectionProfile,
    password: &str,
    mode: DumpMode,
    server_maria: bool,
    path: &Path,
    cancelled: &Cancellation,
    progress: impl FnMut(u64),
) -> Result<String, String> {
    let program = match profile.kind {
        DatabaseKind::PostgreSql => "pg_dump",
        DatabaseKind::MariaDb => "mariadb-dump",
        DatabaseKind::MySql if server_maria => "mariadb-dump",
        DatabaseKind::MySql => "mysqldump",
    };
    let program = client_program(profile, program)?;
    let maria = matches!(profile.kind, DatabaseKind::MariaDb) || server_maria;
    if matches!(profile.kind, DatabaseKind::MySql | DatabaseKind::MariaDb) {
        check_mysql_client(&program, maria)?;
    }
    let (mut cmd, _credentials) = command(profile, password, mode, &program, maria)?;
    let output = AtomicOutput::new(path)?;
    if matches!(profile.kind, DatabaseKind::MySql | DatabaseKind::MariaDb) {
        use base64::Engine;
        let name = base64::engine::general_purpose::STANDARD.encode(profile.database.as_bytes());
        let mut file = &output.file;
        writeln!(file, "-- DBDeck-source-database: {name}").map_err(|e| e.to_string())?;
    }

    cmd.stdin(Stdio::null());
    let result = run(&mut cmd, Some(&output), cancelled, progress);
    let clean = |text: String| {
        if password.is_empty() {
            text
        } else {
            text.replace(password, "[пароль скрыт]")
        }
    };
    let diagnostics = result.map_err(&clean)?;
    if matches!(profile.kind, DatabaseKind::MySql) && !maria {
        let normalized = AtomicOutput::new(path)?;
        let mut input = output.file.try_clone().map_err(|e| e.to_string())?;
        input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        mysql_trigger_dump(BufReader::new(input), &normalized.file, cancelled)?;
        normalized.commit(cancelled)?;
    } else {
        output.commit(cancelled)?;
    }
    Ok(clean(diagnostics))
}

/// The client reads the file directly; stdout is discarded and stderr is bounded.
pub fn restore(
    profile: &ConnectionProfile,
    password: &str,
    path: &Path,
    cancelled: &Cancellation,
) -> Result<String, String> {
    use std::io::{Seek, SeekFrom};
    if profile.read_only {
        return Err("Подключение доступно только для чтения".into());
    }
    if cancelled.is_cancelled() {
        return Err("Операция отменена".into());
    }
    // Nonblocking open also protects against a path swapped to a FIFO before fstat.
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("Не удалось открыть файл: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Выберите обычный файл".into());
    }
    if matches!(profile.kind, DatabaseKind::MySql | DatabaseKind::MariaDb) {
        use base64::Engine;
        let mut prefix = [0u8; 512];
        let n = file.read(&mut prefix).map_err(|e| e.to_string())?;
        let first = prefix[..n]
            .split(|b| *b == b'\n')
            .next()
            .unwrap_or_default();
        if let Some(encoded) = first.strip_prefix(b"-- DBDeck-source-database: ") {
            let name = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| "Повреждён заголовок бэкапа DBDeck")?;
            if name != profile.database.as_bytes() {
                return Err("Этот SQL-бэкап DBDeck создан для другой БД. Представления и процедуры могут содержать её явное имя. Для полного восстановления используйте БД с исходным именем на целевом сервере; для переименования сначала проверьте SQL вручную.".into());
            }
        }
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    }
    let mut signature = [0u8; 5];
    let count = file.read(&mut signature).map_err(|e| e.to_string())?;
    let archive = count == 5 && &signature == b"PGDMP";
    if count == 0 {
        return Err("Файл пуст".into());
    }
    if !archive
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_none_or(|e| !e.eq_ignore_ascii_case("sql"))
    {
        return Err("Поддерживаются SQL-файлы и custom-архивы PostgreSQL (.dump)".into());
    }
    if archive && !matches!(profile.kind, DatabaseKind::PostgreSql) {
        return Err("Архив PostgreSQL нельзя импортировать в MySQL/MariaDB".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let program = match profile.kind {
        DatabaseKind::PostgreSql if archive => "pg_restore",
        DatabaseKind::PostgreSql => "psql",
        DatabaseKind::MariaDb => "mariadb",
        DatabaseKind::MySql => "mysql",
    };
    let program = client_program(profile, program)?;
    let maria = matches!(profile.kind, DatabaseKind::MariaDb);
    if !matches!(profile.kind, DatabaseKind::PostgreSql) {
        check_mysql_client(&program, maria)?;
    }
    let (mut cmd, _credentials) = build_command(
        profile,
        password,
        DumpMode::Full,
        &program,
        maria,
        Some(archive),
    )?;
    cmd.stdin(Stdio::from(file));
    let clean = |text: String| {
        if password.is_empty() {
            text
        } else {
            text.replace(password, "[пароль скрыт]")
        }
    };
    run(&mut cmd, None, cancelled, |_| {})
        .map(clean)
        .map_err(clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn mysql_trigger_wrappers_preserve_sql_and_large_data() {
        let prefix = "/*!50003 CREATE*/ /*!50017 DEFINER=`root`@`%`*/ /*!50003 ";
        for body in [
            "TRIGGER `t` AFTER INSERT ON `x` FOR EACH ROW INSERT INTO audit VALUES(NEW.id);",
            "TRIGGER `t` AFTER INSERT ON `x` FOR EACH ROW BEGIN\nINSERT INTO audit VALUES('inside */;;\nliteral');\nEND;",
        ] {
            let input = format!(
                "INSERT INTO x VALUES('{}');\n{prefix}{body} */;;\n",
                "x".repeat(20000)
            );
            let mut output = Vec::new();
            mysql_trigger_dump(
                std::io::Cursor::new(input.as_bytes()),
                &mut output,
                &Cancellation::new(),
            )
            .unwrap();
            let expected = input.replace(
                &format!("{prefix}{body} */;;"),
                &format!("CREATE DEFINER=`root`@`%` {body} ;;"),
            );
            assert_eq!(output, expected.as_bytes());
        }
        let mut output = Vec::new();
        let cancelled = Cancellation::new();
        cancelled.request();
        assert!(
            mysql_trigger_dump(std::io::Cursor::new(b"SELECT 1;"), &mut output, &cancelled)
                .is_err()
        );
        assert!(
            mysql_trigger_dump(
                std::io::Cursor::new(format!("{prefix}TRIGGER broken")),
                &mut output,
                &Cancellation::new()
            )
            .is_err()
        );
    }
    #[test]
    fn mysql_and_mariadb_tools_must_match_the_server() {
        let path = Path::new("mysqldump");
        assert!(
            validate_client_version("mysqldump Ver 10.19 Distrib 10.11-MariaDB", false, path)
                .is_err()
        );
        assert!(
            validate_client_version(
                "mysql Ver 8.4.8 for Linux (MySQL Community Server)",
                true,
                path
            )
            .is_err()
        );
        assert!(
            validate_client_version(
                "mysql Ver 8.4.8 for Linux (MySQL Community Server)",
                false,
                path
            )
            .is_ok()
        );
        assert!(
            validate_client_version("mariadb-dump Ver 10.19 Distrib 11.4.13-MariaDB", true, path)
                .is_ok()
        );
        assert!(validate_client_version("unknown", false, path).is_err());
    }
    #[test]
    fn secrets_are_private_and_not_command_arguments() {
        let mut profile:ConnectionProfile=serde_json::from_value(serde_json::json!({"id":"t","name":"t","kind":"postgre_sql","host":"localhost","port":5432,"database":"db' options=bad","username":"u","environment":"local","color":"#ffffff","read_only":false,"tls_mode":"disabled"})).unwrap();
        let (cmd, auth) = command(
            &profile,
            "s:e\\cret",
            DumpMode::Schema,
            Path::new("pg_dump"),
            false,
        )
        .unwrap();
        assert!(!format!("{:?}", cmd.get_args().collect::<Vec<_>>()).contains("s:e"));
        assert_eq!(
            fs::metadata(&auth.file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&auth.directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let path = auth.file.clone();
        drop(auth);
        assert!(!path.exists());
        assert_eq!(pg_value("a'\\b"), "'a\\'\\\\b'");
        let database = std::mem::take(&mut profile.database);
        assert!(
            command(
                &profile,
                "secret",
                DumpMode::Full,
                Path::new("pg_dump"),
                false
            )
            .is_err()
        );
        profile.database = database;
        profile.kind = DatabaseKind::MySql;
        let (cmd, auth) = command(
            &profile,
            "x\"\n\\y",
            DumpMode::Full,
            Path::new("mysqldump"),
            false,
        )
        .unwrap();
        assert!(!format!("{:?}", cmd.get_args().collect::<Vec<_>>()).contains("x\""));
        assert!(
            fs::read_to_string(&auth.file)
                .unwrap()
                .contains("password=\"x\\\"\\n\\\\y\"")
        );
    }
    #[test]
    fn restore_targets_are_explicit_and_no_destructive_flags_added() {
        let mut profile: ConnectionProfile = serde_json::from_value(serde_json::json!({"id":"t","name":"t","kind":"postgre_sql","host":"localhost","port":5432,"database":"target","username":"u","environment":"local","color":"#ffffff","read_only":false,"tls_mode":"disabled"})).unwrap();
        for archive in [false, true] {
            let (cmd, _auth) = build_command(
                &profile,
                "secret",
                DumpMode::Full,
                Path::new("unused"),
                false,
                Some(archive),
            )
            .unwrap();
            let args: Vec<_> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert!(args.iter().any(|a| a.contains("dbname='target'")));
            assert!(args.iter().any(|a| a == "--single-transaction"));
            assert!(
                !args
                    .iter()
                    .any(|a| a == "--clean" || a == "--create" || a.contains("secret"))
            );
        }
        profile.read_only = true;
        assert!(
            build_command(
                &profile,
                "secret",
                DumpMode::Full,
                Path::new("unused"),
                false,
                Some(true)
            )
            .is_err()
        );
        profile.read_only = false;
        profile.kind = DatabaseKind::MariaDb;
        let (cmd, _auth) = build_command(
            &profile,
            "secret",
            DumpMode::Full,
            Path::new("unused"),
            true,
            Some(false),
        )
        .unwrap();
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|a| a == "--database=target"));
        assert!(args.iter().any(|a| a == "--binary-mode"));
        assert!(!args.iter().any(|a| a == "--force" || a == "--databases"));
    }

    #[test]
    fn restore_rejects_fifo_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let path = std::env::temp_dir().join(format!("dbdeck-fifo-{}.sql", uuid::Uuid::new_v4()));
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({"id":"t","name":"t","kind":"postgre_sql","host":"localhost","port":5432,"database":"target","username":"u","environment":"local","color":"#ffffff","read_only":false,"tls_mode":"disabled"})).unwrap();
        let start = std::time::Instant::now();
        let result = restore(&profile, "", &path, &Cancellation::new());
        fs::remove_file(path).unwrap();
        assert!(result.unwrap_err().contains("обычный файл"));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn failures_and_cancellation_preserve_destination() {
        let root =
            std::env::temp_dir().join(format!("dbdeck-backup-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("backup.sql");
        fs::write(&path, "previous").unwrap();
        let output = AtomicOutput::new(&path).unwrap();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf partial; printf failure >&2; exit 2"]);
        assert!(run(&mut cmd, Some(&output), &Cancellation::new(), |_| {}).is_err());
        drop(output);
        assert_eq!(fs::read_to_string(&path).unwrap(), "previous");
        let output = AtomicOutput::new(&path).unwrap();
        let cancel = std::sync::Arc::new(Cancellation::new());
        let flag = cancel.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.request();
        });
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let start = std::time::Instant::now();
        assert!(run(&mut cmd, Some(&output), &cancel, |_| {}).is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
        worker.join().unwrap();
        drop(output);
        assert_eq!(fs::read_to_string(&path).unwrap(), "previous");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
