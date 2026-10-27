#[path = "csv_import.rs"]
mod csv_import;
#[path = "query_edit.rs"]
mod query_edit;

use std::time::Duration;

use futures_util::{StreamExt, TryStreamExt};
use sqlx::{
    Column, Connection, Executor, Row, TypeInfo, ValueRef, mysql::MySqlPoolOptions,
    postgres::PgPoolOptions,
};

use crate::settings::{ConnectionProfile, DatabaseKind, TlsMode};

pub const MAX_TABLE_ROWS: usize = 200;
/// Bound the client snapshot/DOM and OFFSET depth; use filters or streaming export beyond this.
pub const MAX_BROWSE_ROWS: usize = 10_000;
pub const MAX_PREVIEW_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_TABLE_COLUMNS: usize = 64;
pub const MAX_CATALOG_OBJECTS: usize = 1_000;
pub const MAX_CELL_CHARS: usize = 1_024;
const MAX_BINARY_BYTES: usize = (MAX_CELL_CHARS - 2) / 2;
pub const MAX_QUERY_ROWS: usize = 1_000;
const MAX_QUERY_COLUMNS: usize = 128;

#[derive(Clone, Debug)]
pub struct DbObject {
    pub schema: String,
    pub name: String,
    pub kind: DbObjectKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DbObjectKind {
    Table,
    View,
}

impl DbObjectKind {
    pub fn icon_name(&self) -> &'static str {
        match self {
            Self::Table => "x-office-spreadsheet-symbolic",
            Self::View => "view-list-symbolic",
        }
    }
}

pub(crate) enum QueryConnection<'a> {
    PostgreSql(&'a mut sqlx::PgConnection),
    MySql(&'a mut sqlx::MySqlConnection),
}
type QueryCandidate = (query_edit::Source, DbObject, Vec<DbColumn>);

#[derive(Clone, Debug)]
struct DbColumn {
    name: String,
    data_type: String,
    binary: bool,
    primary_key: bool,
    generated: bool,
    udt_schema: String,
    udt_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterOperator {
    Equal,
    NotEqual,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    Greater,
    GreaterEqual,
    Less,
    LessEqual,
    In,
    NotIn,
    IsNull,
    IsNotNull,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilterLogic {
    #[default]
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableFilter {
    pub column: String,
    pub logic: FilterLogic,
    pub operator: FilterOperator,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableSort {
    pub column: String,
    pub descending: bool,
}

#[derive(Clone, Debug)]
pub struct TableCatalog {
    pub schemas: Vec<String>,
    pub objects: Vec<DbObject>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct TableData {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<CellValue>>,
    pub primary_key: Vec<String>,
    pub truncated: bool,
    pub truncated_columns: bool,
}

impl TableData {
    pub fn trim_preview(&mut self, max_rows: usize) -> bool {
        let mut bytes = 0usize;
        let keep = self
            .rows
            .iter()
            .take(max_rows)
            .take_while(|row| {
                bytes = bytes.saturating_add(row_memory(row));
                bytes <= MAX_PREVIEW_BYTES
            })
            .count();
        let clipped = keep < self.rows.len();
        self.rows.truncate(keep);
        clipped
    }
}
fn row_memory(row: &[CellValue]) -> usize {
    row.iter().fold(
        row.len() * std::mem::size_of::<CellValue>(),
        |bytes, cell| {
            bytes.saturating_add(match cell {
                CellValue::Text(s) | CellValue::Truncated(s) | CellValue::DecodeError(s) => {
                    s.capacity()
                }
                CellValue::Null => 0,
            })
        },
    )
}

#[derive(Clone, Debug)]
pub struct TableCellEdit {
    pub primary_key_values: Vec<CellValue>,
    pub column: String,
    pub original_value: CellValue,
    pub value: Option<String>,
}

#[derive(Clone, Debug)]
pub struct QueryResult {
    pub editable_object: Option<DbObject>,
    pub read_only_columns: Vec<String>,
    pub data: std::sync::Arc<TableData>,
    pub rows_affected: u64,
    pub row_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellValue {
    Text(String),
    Truncated(String),
    Null,
    DecodeError(String),
}

macro_rules! query_cell_fn {
    ($fn_name:ident, $row_type:ty) => {
        fn $fn_name(row: &$row_type, index: usize) -> CellValue {
            if row.try_get_raw(index).is_ok_and(|value| value.is_null()) {
                return CellValue::Null;
            }
            match row.try_get_unchecked::<Option<String>, usize>(index) {
                Ok(Some(value)) => truncate_cell(value),
                Ok(None) => CellValue::Null,
                Err(text_error) => match row.try_get_unchecked::<Option<Vec<u8>>, usize>(index) {
                    Ok(Some(bytes)) => {
                        let mut value = format!(
                            "0x{}",
                            bytes
                                .iter()
                                .take(MAX_BINARY_BYTES)
                                .map(|byte| format!("{byte:02x}"))
                                .collect::<String>()
                        );
                        if bytes.len() > MAX_BINARY_BYTES {
                            value.push('…');
                            CellValue::Truncated(value)
                        } else {
                            CellValue::Text(value)
                        }
                    }
                    Ok(None) => CellValue::Null,
                    Err(_) => {
                        let type_name = row.column(index).type_info().name().to_owned();
                        CellValue::DecodeError(format!("{type_name}: {text_error}"))
                    }
                },
            }
        }
    };
}

query_cell_fn!(query_cell_postgres, sqlx::postgres::PgRow);
query_cell_fn!(query_cell_mysql, sqlx::mysql::MySqlRow);

fn truncate_cell(value: String) -> CellValue {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(MAX_CELL_CHARS).collect::<String>();
    if chars.next().is_some() {
        CellValue::Truncated(prefix)
    } else {
        CellValue::Text(value)
    }
}

#[derive(Clone)]
pub enum DbPool {
    PostgreSql(sqlx::PgPool),
    MySql(sqlx::MySqlPool),
}

impl std::fmt::Debug for DbPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DbPool(..)")
    }
}

fn pg_pool_options() -> PgPoolOptions {
    PgPoolOptions::new().max_connections(3).acquire_timeout(Duration::from_secs(8)).after_connect(|connection,_|Box::pin(async move {
        connection.execute("SET application_name='DBDeck'; SET lock_timeout='5s'; SET idle_in_transaction_session_timeout='300s'").await?;Ok(())
    }))
}
fn mysql_pool_options() -> MySqlPoolOptions {
    MySqlPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(Duration::from_secs(8))
        .after_connect(|connection, _| {
            Box::pin(async move {
                connection
                    .execute("SET SESSION lock_wait_timeout=5, innodb_lock_wait_timeout=5")
                    .await?;
                Ok(())
            })
        })
}

impl DbPool {
    pub async fn connect(profile: &ConnectionProfile, password: &str) -> Result<Self, String> {
        profile.validate_transport()?;
        let timeout = Duration::from_secs(8);
        match profile.kind {
            DatabaseKind::PostgreSql => {
                let mut options = sqlx::postgres::PgConnectOptions::new()
                    .host(&profile.host)
                    .port(profile.port)
                    .username(&profile.username)
                    .password(password)
                    .ssl_mode(match profile.tls_mode {
                        TlsMode::Disabled => sqlx::postgres::PgSslMode::Disable,
                        TlsMode::Require => sqlx::postgres::PgSslMode::Require,
                        TlsMode::VerifyFull => sqlx::postgres::PgSslMode::VerifyFull,
                    });
                if let Some(path) = &profile.tls_files.ca {
                    options = options.ssl_root_cert(path);
                }
                if let Some(path) = &profile.tls_files.client_cert {
                    options = options.ssl_client_cert(path);
                }
                if let Some(path) = &profile.tls_files.client_key {
                    options = options.ssl_client_key(path);
                }
                let explicit = profile.database.trim();
                let candidates = if explicit.is_empty() {
                    vec![profile.username.as_str(), "postgres", "template1"]
                } else {
                    vec![explicit]
                };
                let mut last_error = String::new();
                for database in candidates {
                    match pg_pool_options()
                        .max_connections(3)
                        .acquire_timeout(timeout)
                        .connect_with(options.clone().database(database))
                        .await
                    {
                        Ok(pool) => return Ok(Self::PostgreSql(pool)),
                        Err(error) => {
                            let unavailable = matches!(&error, sqlx::Error::Database(error)
                                if matches!(error.code().as_deref(), Some("3D000" | "42501")));
                            last_error = format_database_error(&error.to_string());
                            if !explicit.is_empty() || !unavailable {
                                return Err(last_error);
                            }
                        }
                    }
                }
                Err(format!(
                    "{last_error}. Укажите доступную базу для начального подключения."
                ))
            }
            DatabaseKind::MySql | DatabaseKind::MariaDb => {
                let mut options = sqlx::mysql::MySqlConnectOptions::new()
                    .host(&profile.host)
                    .port(profile.port)
                    .username(&profile.username)
                    .password(password)
                    .ssl_mode(match profile.tls_mode {
                        TlsMode::Disabled => sqlx::mysql::MySqlSslMode::Disabled,
                        TlsMode::Require => sqlx::mysql::MySqlSslMode::Required,
                        TlsMode::VerifyFull => sqlx::mysql::MySqlSslMode::VerifyIdentity,
                    });
                if let Some(path) = &profile.tls_files.ca {
                    options = options.ssl_ca(path);
                }
                if let Some(path) = &profile.tls_files.client_cert {
                    options = options.ssl_client_cert(path);
                }
                if let Some(path) = &profile.tls_files.client_key {
                    options = options.ssl_client_key(path);
                }
                if !profile.database.trim().is_empty() {
                    options = options.database(profile.database.trim());
                }
                mysql_pool_options()
                    .max_connections(3)
                    .acquire_timeout(timeout)
                    .connect_with(options)
                    .await
                    .map(Self::MySql)
                    .map_err(|error| format_database_error(&error.to_string()))
            }
        }
    }

    pub async fn profile_database_targets(
        profile: &ConnectionProfile,
        password: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let pool = match Self::connect(profile, password).await {
            Ok(pool) => pool,
            Err(original) if !profile.database.is_empty() => {
                // A saved database may have been removed; keep the configured DB as first attempt.
                let mut root = profile.clone();
                root.database.clear();
                Self::connect(&root, password).await.map_err(|_| original)?
            }
            Err(error) => return Err(error),
        };
        pool.database_targets(&profile.database).await
    }

    /// Lightweight metadata; no table scans or database size calculation.
    pub async fn database_targets(&self, preferred: &str) -> Result<Vec<(String, String)>, String> {
        match self {
            Self::PostgreSql(pool) => sqlx::query_as::<_, (String, String)>(
                "SELECT datname::text, 'Владелец: ' || pg_get_userbyid(datdba) || ' · Кодировка: ' || pg_encoding_to_char(encoding) FROM pg_catalog.pg_database WHERE datallowconn AND NOT datistemplate AND has_database_privilege(oid, 'CONNECT') ORDER BY (datname = $1) DESC, datname LIMIT 1001",
            ).bind(preferred).fetch_all(pool).await,
            Self::MySql(pool) => sqlx::query_as::<_, (String, String)>(
                "SELECT SCHEMA_NAME, CONCAT('Кодировка: ', DEFAULT_CHARACTER_SET_NAME, ' · Сравнение: ', DEFAULT_COLLATION_NAME) FROM information_schema.SCHEMATA ORDER BY (SCHEMA_NAME = ?) DESC, SCHEMA_NAME LIMIT 1001",
            ).bind(preferred).fetch_all(pool).await,
        }.map_err(|e| format_database_error(&e.to_string()))
    }

    pub async fn postgres_databases(&self) -> Result<Vec<String>, String> {
        let Self::PostgreSql(pool) = self else {
            return Err("Список баз PostgreSQL доступен только для PostgreSQL".into());
        };
        sqlx::query_scalar(
            "SELECT datname::text FROM pg_catalog.pg_database \
             WHERE datallowconn AND NOT datistemplate \
               AND has_database_privilege(oid, 'CONNECT') \
             ORDER BY datname LIMIT 1000",
        )
        .fetch_all(pool)
        .await
        .map_err(|error| format_database_error(&error.to_string()))
    }

    pub async fn server_icon(&self) -> Result<&'static str, String> {
        match self {
            Self::PostgreSql(_) => Ok("postgresql"),
            Self::MySql(pool) => {
                let version: String = sqlx::query_scalar("SELECT VERSION()")
                    .fetch_one(pool)
                    .await
                    .map_err(|error| format_database_error(&error.to_string()))?;
                Ok(if version.to_ascii_lowercase().contains("mariadb") {
                    "mariadb"
                } else {
                    "mysql"
                })
            }
        }
    }

    /// A separate pool preserves the current database and all existing SQL sessions.
    pub async fn connect_database(&self, database: &str) -> Result<Self, String> {
        match self {
            Self::PostgreSql(pool) => pg_pool_options()
                .max_connections(3)
                .acquire_timeout(Duration::from_secs(8))
                .connect_with((*pool.connect_options()).clone().database(database))
                .await
                .map(Self::PostgreSql),
            Self::MySql(pool) => mysql_pool_options()
                .max_connections(3)
                .acquire_timeout(Duration::from_secs(8))
                .connect_with((*pool.connect_options()).clone().database(database))
                .await
                .map(Self::MySql),
        }
        .map_err(|error| format_database_error(&error.to_string()))
    }

    pub async fn check(&self) -> Result<(), String> {
        match self {
            Self::PostgreSql(pool) => sqlx::query("SELECT 1")
                .fetch_one(pool)
                .await
                .map(|_| ())
                .map_err(|error| format_database_error(&error.to_string())),
            Self::MySql(pool) => sqlx::query("SELECT 1")
                .fetch_one(pool)
                .await
                .map(|_| ())
                .map_err(|error| format_database_error(&error.to_string())),
        }
    }

    #[cfg(test)]
    pub async fn execute_sql(&self, sql: &str) -> Result<QueryResult, String> {
        match self {
            Self::PostgreSql(pool) => {
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(|e| format_database_error(&e.to_string()))?;
                execute_on_connection(QueryConnection::PostgreSql(&mut connection), sql).await
            }
            Self::MySql(pool) => {
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(|e| format_database_error(&e.to_string()))?;
                execute_on_connection(QueryConnection::MySql(&mut connection), sql).await
            }
        }
    }

    async fn query_edit_candidate(
        connection: QueryConnection<'_>,
        sql: &str,
    ) -> Result<Option<QueryCandidate>, String> {
        let mysql = matches!(connection, QueryConnection::MySql(_));
        let Some(source) = query_edit::source(sql, mysql) else {
            return Ok(None);
        };
        let (object, columns) = match connection {
            QueryConnection::PostgreSql(pool) => {
                let name = match &source.schema {
                    Some(schema) => format!(
                        "{}.{}",
                        quote_pg_identifier(schema),
                        quote_pg_identifier(&source.table)
                    ),
                    None => quote_pg_identifier(&source.table),
                };
                let row = sqlx::query("SELECT n.nspname AS schema_name, c.relname AS table_name FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE c.oid=pg_catalog.to_regclass($1) AND c.relkind IN ('r','p') AND (c.relkind='p' OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhparent=c.oid))")
                    .bind(name).fetch_optional(&mut *pool).await.map_err(|e|format_database_error(&e.to_string()))?;
                let Some(row) = row else { return Ok(None) };
                let object = DbObject {
                    schema: row.try_get("schema_name").map_err(|_| "Нет схемы")?,
                    name: row.try_get("table_name").map_err(|_| "Нет таблицы")?,
                    kind: DbObjectKind::Table,
                };
                let columns = table_columns_pg(&mut *pool, &object).await?;
                (object, columns)
            }
            QueryConnection::MySql(pool) => {
                let schema = match source.schema.clone() {
                    Some(schema) => schema,
                    None => sqlx::query_scalar::<_, Option<String>>("SELECT DATABASE()")
                        .fetch_one(&mut *pool)
                        .await
                        .map_err(|e| format_database_error(&e.to_string()))?
                        .unwrap_or_default(),
                };
                let exists:i64=sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema=? AND table_name=? AND table_type='BASE TABLE'")
                    .bind(&schema).bind(&source.table).fetch_one(&mut *pool).await.map_err(|e|format_database_error(&e.to_string()))?;
                if exists != 1 {
                    return Ok(None);
                }
                let object = DbObject {
                    schema,
                    name: source.table.clone(),
                    kind: DbObjectKind::Table,
                };
                if !mysql_is_base_table(&mut *pool, &object).await? {
                    return Ok(None);
                }
                let columns = table_columns_mysql(&mut *pool, &object).await?;
                (object, columns)
            }
        };
        Ok(Some((source, object, columns)))
    }

    fn validate_query_edit_target(
        (source, object, columns): QueryCandidate,
        result_columns: &[String],
        rows: &[Vec<CellValue>],
    ) -> Option<(DbObject, Vec<String>, Vec<String>)> {
        let selected = source
            .columns
            .unwrap_or_else(|| columns.iter().map(|c| c.name.clone()).collect());
        if selected != result_columns
            || selected
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != selected.len()
            || selected
                .iter()
                .any(|name| !columns.iter().any(|c| c.name == *name))
        {
            return None;
        }
        let keys = columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.clone())
            .collect::<Vec<_>>();
        if keys.is_empty() || keys.iter().any(|key| !selected.contains(key)) {
            return None;
        }
        let indices = keys
            .iter()
            .map(|key| selected.iter().position(|name| name == key).unwrap())
            .collect::<Vec<_>>();
        let mut seen = std::collections::HashSet::new();
        for row in rows {
            let Some(key) = indices
                .iter()
                .map(|i| match row.get(*i) {
                    Some(CellValue::Text(value)) => Some(value.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
            else {
                return None;
            };
            if !seen.insert(key) {
                return None;
            }
        }
        let blocked = columns
            .iter()
            .filter(|c| c.primary_key || c.generated)
            .map(|c| c.name.clone())
            .collect();
        Some((object, keys, blocked))
    }

    pub async fn list_sql_completions(
        &self,
        profile: &ConnectionProfile,
    ) -> Result<Vec<String>, String> {
        let mut words = Vec::new();
        match self {
            Self::PostgreSql(pool) => {
                let rows = sqlx::query(
                    "SELECT table_name, column_name FROM information_schema.columns \
                     WHERE table_schema NOT IN ('information_schema', 'pg_catalog') \
                       AND table_schema NOT LIKE 'pg_toast%' \
                       AND table_schema NOT LIKE 'pg_temp_%' \
                     ORDER BY table_schema, table_name, ordinal_position LIMIT 20000",
                )
                .fetch_all(pool)
                .await
                .map_err(|error| format_database_error(&error.to_string()))?;
                for row in rows {
                    if let Ok(value) = row.try_get::<String, _>("table_name") {
                        words.push(value);
                    }
                    if let Ok(value) = row.try_get::<String, _>("column_name") {
                        words.push(value);
                    }
                }
            }
            Self::MySql(pool) => {
                let rows = sqlx::query(
                    "SELECT table_name, column_name FROM information_schema.columns \
                     WHERE table_schema NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') \
                       AND (? = '' OR table_schema = ?) \
                     ORDER BY table_schema, table_name, ordinal_position LIMIT 20000",
                )
                .bind(profile.database.trim())
                .bind(profile.database.trim())
                .fetch_all(pool)
                .await
                .map_err(|error| format_database_error(&error.to_string()))?;
                for row in rows {
                    if let Ok(value) = row.try_get::<String, _>("table_name") {
                        words.push(value);
                    }
                    if let Ok(value) = row.try_get::<String, _>("column_name") {
                        words.push(value);
                    }
                }
            }
        }
        words.sort_unstable();
        words.dedup();
        Ok(words)
    }

    pub async fn list_tables(&self, profile: &ConnectionProfile) -> Result<TableCatalog, String> {
        match self {
            Self::PostgreSql(pool) => {
                let schemas = sqlx::query_scalar::<_, String>(
                    "SELECT schema_name FROM information_schema.schemata \
                     WHERE schema_name NOT LIKE 'pg_toast%' \
                       AND schema_name NOT LIKE 'pg_temp_%' \
                     ORDER BY CASE WHEN schema_name = 'public' THEN 0 \
                                   WHEN schema_name IN ('information_schema', 'pg_catalog') THEN 2 \
                                   ELSE 1 END, schema_name",
                )
                .fetch_all(pool)
                .await
                .map_err(|error| format_database_error(&error.to_string()))?;
                let rows = sqlx::query(&format!(
                    "SELECT table_schema AS table_schema, table_name AS table_name, table_type AS object_type \
                     FROM information_schema.tables \
                     WHERE table_type IN ('BASE TABLE', 'VIEW') \
                       AND table_schema NOT LIKE 'pg_toast%' \
                       AND table_schema NOT LIKE 'pg_temp_%' \
                     ORDER BY CASE WHEN table_schema = 'public' THEN 0 \
                                   WHEN table_schema IN ('information_schema', 'pg_catalog') THEN 2 \
                                   ELSE 1 END, table_schema, table_name LIMIT {}",
                    MAX_CATALOG_OBJECTS + 1
                ))
                .fetch_all(pool)
                .await
                .map_err(|error| format_database_error(&error.to_string()))?;

                table_catalog(
                    schemas,
                    rows.into_iter().map(|row| {
                        Ok(DbObject {
                            schema: row
                                .try_get("table_schema")
                                .map_err(|error| error.to_string())?,
                            name: row
                                .try_get("table_name")
                                .map_err(|error| error.to_string())?,
                            kind: if row
                                .try_get::<String, _>("object_type")
                                .map_err(|error| error.to_string())?
                                .eq_ignore_ascii_case("VIEW")
                            {
                                DbObjectKind::View
                            } else {
                                DbObjectKind::Table
                            },
                        })
                    }),
                )
            }
            Self::MySql(pool) => {
                let schemas = sqlx::query_scalar::<_, String>(
                    "SELECT CAST(schema_name AS CHAR) FROM information_schema.schemata \
                     WHERE schema_name NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') \
                       AND (? = '' OR schema_name = ?) \
                     ORDER BY schema_name",
                )
                .bind(profile.database.trim())
                .bind(profile.database.trim())
                .fetch_all(pool)
                .await
                .map_err(|error| format_database_error(&error.to_string()))?;
                let rows = sqlx::query(&format!(
                    "SELECT CAST(table_schema AS CHAR) AS table_schema, \
                            CAST(table_name AS CHAR) AS table_name, \
                            CAST(table_type AS CHAR) AS object_type \
                     FROM information_schema.tables \
                     WHERE table_type IN ('BASE TABLE', 'VIEW') \
                       AND table_schema NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') \
                       AND (? = '' OR table_schema = ?) \
                     ORDER BY table_schema, table_name LIMIT {}",
                    MAX_CATALOG_OBJECTS + 1
                ))
                .bind(profile.database.trim())
                .bind(profile.database.trim())
                .fetch_all(pool)
                .await
                .map_err(|error| format_database_error(&error.to_string()))?;

                table_catalog(
                    schemas,
                    rows.into_iter().map(|row| {
                        Ok(DbObject {
                            schema: row
                                .try_get("table_schema")
                                .map_err(|error| error.to_string())?,
                            name: row
                                .try_get("table_name")
                                .map_err(|error| error.to_string())?,
                            kind: if row
                                .try_get::<String, _>("object_type")
                                .map_err(|error| error.to_string())?
                                .eq_ignore_ascii_case("VIEW")
                            {
                                DbObjectKind::View
                            } else {
                                DbObjectKind::Table
                            },
                        })
                    }),
                )
            }
        }
    }

    pub async fn read_table(&self, object: &DbObject) -> Result<TableData, String> {
        self.read_table_view(object, &[], None, 0).await
    }

    pub async fn read_table_view(
        &self,
        object: &DbObject,
        filters: &[TableFilter],
        sort: Option<&TableSort>,
        offset: usize,
    ) -> Result<TableData, String> {
        self.read_table_view_with_condition(object, filters, None, sort, offset)
            .await
    }

    pub async fn read_table_view_with_condition(
        &self,
        object: &DbObject,
        filters: &[TableFilter],
        raw_condition: Option<&str>,
        sort: Option<&TableSort>,
        offset: usize,
    ) -> Result<TableData, String> {
        let condition = raw_condition.map(validate_raw_condition).transpose()?;
        match self {
            Self::PostgreSql(pool) => {
                let all_columns = table_columns_pg(pool, object).await?;
                let primary_key: Vec<String> = all_columns
                    .iter()
                    .filter(|column| column.primary_key)
                    .map(|column| column.name.clone())
                    .collect();
                let (column_info, truncated_columns) = limit_columns(all_columns);
                let columns = column_info
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>();
                let selection = column_info
                    .iter()
                    .enumerate()
                    .flat_map(|(index, column)| {
                        let identifier = quote_pg_identifier(&column.name);
                        let (text, length) = if column.binary {
                            (
                                format!("('0x' || ENCODE(SUBSTRING({identifier} FROM 1 FOR {MAX_BINARY_BYTES}), 'hex'))"),
                                format!("OCTET_LENGTH({identifier})"),
                            )
                        } else {
                            let text = format!("CAST({identifier} AS TEXT)");
                            (format!("LEFT({text}, {MAX_CELL_CHARS})"), format!("CHAR_LENGTH({text})"))
                        };
                        [
                            format!("{text} AS \"v{index}\""),
                            format!("({identifier} IS NOT NULL AND {length} > {}) AS \"t{index}\"", if column.binary { MAX_BINARY_BYTES } else { MAX_CELL_CHARS }),
                        ]
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let table = format!(
                    "{}.{}",
                    quote_pg_identifier(&object.schema),
                    quote_pg_identifier(&object.name)
                );
                let (where_clause, binds) = build_view_where_clause(
                    filters,
                    condition.as_deref(),
                    &column_info,
                    SqlDialect::PostgreSql,
                )?;
                let order_clause = build_stable_order_clause(
                    sort,
                    &column_info,
                    &primary_key,
                    SqlDialect::PostgreSql,
                )?;
                let sql = format!(
                    "SELECT {selection} FROM {table}{where_clause}{order_clause} LIMIT {} OFFSET {offset}",
                    MAX_TABLE_ROWS + 1,
                );
                let mut query = sqlx::query(&sql);
                for value in binds {
                    query = query.bind(value);
                }
                let rows = {
                    let mut transaction = pool
                        .begin_with("START TRANSACTION READ ONLY")
                        .await
                        .map_err(|error| format_database_error(&error.to_string()))?;
                    let result = query.fetch_all(&mut *transaction).await;
                    transaction
                        .rollback()
                        .await
                        .map_err(|error| format_database_error(&error.to_string()))?;
                    result
                }
                .map_err(|error| format_database_error(&error.to_string()))?;
                Ok(to_table_data(
                    columns,
                    rows,
                    primary_key,
                    truncated_columns,
                    |row, index| match row.try_get::<Option<String>, _>(index * 2) {
                        Ok(Some(value)) => match row.try_get::<bool, _>(index * 2 + 1) {
                            Ok(true) => CellValue::Truncated(value),
                            Ok(false) => CellValue::Text(value),
                            Err(error) => CellValue::DecodeError(error.to_string()),
                        },
                        Ok(None) => CellValue::Null,
                        Err(error) => CellValue::DecodeError(error.to_string()),
                    },
                ))
            }
            Self::MySql(pool) => {
                let all_columns = table_columns_mysql(pool, object).await?;
                let primary_key: Vec<String> = all_columns
                    .iter()
                    .filter(|column| column.primary_key)
                    .map(|column| column.name.clone())
                    .collect();
                let (column_info, truncated_columns) = limit_columns(all_columns);
                let columns = column_info
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>();
                let selection = column_info
                    .iter()
                    .enumerate()
                    .flat_map(|(index, column)| {
                        let identifier = quote_mysql_identifier(&column.name);
                        let (text, length) = if column.binary {
                            (
                                format!("CONCAT('0x', HEX(SUBSTRING({identifier}, 1, {MAX_BINARY_BYTES})))"),
                                format!("OCTET_LENGTH({identifier})"),
                            )
                        } else {
                            let text = format!("CAST({identifier} AS CHAR)");
                            (format!("LEFT({text}, {MAX_CELL_CHARS})"), format!("CHAR_LENGTH({text})"))
                        };
                        [
                            format!("{text} AS `v{index}`"),
                            format!("({identifier} IS NOT NULL AND {length} > {}) AS `t{index}`", if column.binary { MAX_BINARY_BYTES } else { MAX_CELL_CHARS }),
                        ]
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let table = format!(
                    "{}.{}",
                    quote_mysql_identifier(&object.schema),
                    quote_mysql_identifier(&object.name)
                );
                let (where_clause, binds) = build_view_where_clause(
                    filters,
                    condition.as_deref(),
                    &column_info,
                    SqlDialect::MySql,
                )?;
                let order_clause =
                    build_stable_order_clause(sort, &column_info, &primary_key, SqlDialect::MySql)?;
                let sql = format!(
                    "SELECT {selection} FROM {table}{where_clause}{order_clause} LIMIT {} OFFSET {offset}",
                    MAX_TABLE_ROWS + 1,
                );
                let mut query = sqlx::query(&sql);
                for value in binds {
                    query = query.bind(value);
                }
                let rows = {
                    let mut transaction = pool
                        .begin_with("START TRANSACTION READ ONLY")
                        .await
                        .map_err(|error| format_database_error(&error.to_string()))?;
                    let result = query.fetch_all(&mut *transaction).await;
                    transaction
                        .rollback()
                        .await
                        .map_err(|error| format_database_error(&error.to_string()))?;
                    result
                }
                .map_err(|error| format_database_error(&error.to_string()))?;
                Ok(to_table_data(
                    columns,
                    rows,
                    primary_key,
                    truncated_columns,
                    |row, index| match row.try_get::<Option<String>, _>(index * 2) {
                        Ok(Some(value)) => match row.try_get::<bool, _>(index * 2 + 1) {
                            Ok(true) => CellValue::Truncated(value),
                            Ok(false) => CellValue::Text(value),
                            Err(error) => CellValue::DecodeError(error.to_string()),
                        },
                        Ok(None) => CellValue::Null,
                        Err(error) => CellValue::DecodeError(error.to_string()),
                    },
                ))
            }
        }
    }

    pub async fn update_table_cells(
        &self,
        object: &DbObject,
        edits: &[TableCellEdit],
    ) -> Result<usize, String> {
        match self {
            Self::PostgreSql(pool) => {
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(|e| format_database_error(&e.to_string()))?;
                update_table_cells_on(QueryConnection::PostgreSql(&mut connection), object, edits)
                    .await
            }
            Self::MySql(pool) => {
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(|e| format_database_error(&e.to_string()))?;
                update_table_cells_on(QueryConnection::MySql(&mut connection), object, edits).await
            }
        }
    }

    pub async fn delete_table_row(
        &self,
        object: &DbObject,
        primary_key_values: &[CellValue],
    ) -> Result<u64, String> {
        match self {
            Self::PostgreSql(pool) => {
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(|e| format_database_error(&e.to_string()))?;
                delete_table_row_on(
                    QueryConnection::PostgreSql(&mut connection),
                    object,
                    primary_key_values,
                )
                .await
            }
            Self::MySql(pool) => {
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(|e| format_database_error(&e.to_string()))?;
                delete_table_row_on(
                    QueryConnection::MySql(&mut connection),
                    object,
                    primary_key_values,
                )
                .await
            }
        }
    }

    pub async fn clone_table_row(
        &self,
        object: &DbObject,
        primary_key_values: &[CellValue],
    ) -> Result<u64, String> {
        if object.kind != DbObjectKind::Table {
            return Err("Клонирование строк представлений не поддерживается".to_owned());
        }
        match self {
            Self::PostgreSql(pool) => {
                let columns = table_columns_pg(pool, object).await?;
                let keys = columns
                    .iter()
                    .filter(|column| column.primary_key)
                    .collect::<Vec<_>>();
                if keys.is_empty() || keys.len() != primary_key_values.len() {
                    return Err("Не удалось определить первичный ключ строки".to_owned());
                }
                let copied = columns
                    .iter()
                    .filter(|column| !column.primary_key && !column.generated)
                    .collect::<Vec<_>>();
                if copied.is_empty() {
                    return Err("В таблице нет столбцов для копирования".to_owned());
                }
                let table = format!(
                    "{}.{}",
                    quote_pg_identifier(&object.schema),
                    quote_pg_identifier(&object.name)
                );
                let mut binds = Vec::with_capacity(keys.len());
                let mut predicates = Vec::with_capacity(keys.len());
                for (index, (key, value)) in keys.iter().zip(primary_key_values).enumerate() {
                    let CellValue::Text(value) = value else {
                        return Err("Первичный ключ строки нельзя прочитать".to_owned());
                    };
                    binds.push(value.clone());
                    let parameter = index + 1;
                    predicates.push(format!(
                        "{} = CAST(${parameter} AS {}.{})",
                        quote_pg_identifier(&key.name),
                        quote_pg_identifier(&key.udt_schema),
                        quote_pg_identifier(&key.udt_name)
                    ));
                }
                let names = copied
                    .iter()
                    .map(|column| quote_pg_identifier(&column.name))
                    .collect::<Vec<_>>();
                let sql = format!(
                    "INSERT INTO {table} ({}) SELECT {} FROM {table} WHERE {}",
                    names.join(", "),
                    names.join(", "),
                    predicates.join(" AND ")
                );
                let mut query = sqlx::query(&sql);
                for value in binds {
                    query = query.bind(value);
                }
                query
                    .execute(pool)
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(|error| format_database_error(&error.to_string()))
            }
            Self::MySql(pool) => {
                let columns = table_columns_mysql(pool, object).await?;
                let keys = columns
                    .iter()
                    .filter(|column| column.primary_key)
                    .collect::<Vec<_>>();
                if keys.is_empty() || keys.len() != primary_key_values.len() {
                    return Err("Не удалось определить первичный ключ строки".to_owned());
                }
                let copied = columns
                    .iter()
                    .filter(|column| !column.primary_key && !column.generated)
                    .collect::<Vec<_>>();
                if copied.is_empty() {
                    return Err("В таблице нет столбцов для копирования".to_owned());
                }
                let table = format!(
                    "{}.{}",
                    quote_mysql_identifier(&object.schema),
                    quote_mysql_identifier(&object.name)
                );
                let names = copied
                    .iter()
                    .map(|column| quote_mysql_identifier(&column.name))
                    .collect::<Vec<_>>();
                let sql = format!(
                    "INSERT INTO {table} ({}) SELECT {} FROM {table} WHERE {}",
                    names.join(", "),
                    names.join(", "),
                    keys.iter()
                        .map(|key| format!("{} = ?", quote_mysql_identifier(&key.name)))
                        .collect::<Vec<_>>()
                        .join(" AND ")
                );
                let mut query = sqlx::query(&sql);
                for value in primary_key_values {
                    let CellValue::Text(value) = value else {
                        return Err("Первичный ключ строки нельзя прочитать".to_owned());
                    };
                    query = query.bind(value);
                }
                query
                    .execute(pool)
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(|error| format_database_error(&error.to_string()))
            }
        }
    }

    pub async fn insert_default_table_row(&self, object: &DbObject) -> Result<u64, String> {
        if object.kind != DbObjectKind::Table {
            return Err("Добавление строк в представления не поддерживается".to_owned());
        }
        let table = match self {
            Self::PostgreSql(_) => format!(
                "{}.{}",
                quote_pg_identifier(&object.schema),
                quote_pg_identifier(&object.name)
            ),
            Self::MySql(_) => format!(
                "{}.{}",
                quote_mysql_identifier(&object.schema),
                quote_mysql_identifier(&object.name)
            ),
        };
        match self {
            Self::PostgreSql(pool) => sqlx::query(&format!("INSERT INTO {table} DEFAULT VALUES"))
                .execute(pool)
                .await
                .map(|result| result.rows_affected())
                .map_err(|error| format_database_error(&error.to_string())),
            Self::MySql(pool) => sqlx::query(&format!("INSERT INTO {table} () VALUES ()"))
                .execute(pool)
                .await
                .map(|result| result.rows_affected())
                .map_err(|error| format_database_error(&error.to_string())),
        }
    }

    pub async fn update_table_column(
        &self,
        object: &DbObject,
        column_name: &str,
        value: Option<&str>,
    ) -> Result<u64, String> {
        if object.kind != DbObjectKind::Table {
            return Err("Редактирование представлений не поддерживается".to_owned());
        }
        match self {
            Self::PostgreSql(pool) => {
                let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
                sqlx::query(&format!(
                    "LOCK TABLE {}.{} IN ROW EXCLUSIVE MODE",
                    quote_pg_identifier(&object.schema),
                    quote_pg_identifier(&object.name)
                ))
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
                let columns = table_columns_pg(&mut *tx, object).await?;
                let column = columns
                    .iter()
                    .find(|column| column.name == column_name)
                    .ok_or_else(|| format!("Столбец «{column_name}» больше не существует"))?;
                if column.primary_key || column.binary || column.generated {
                    return Err("Массовое изменение первичного ключа не поддерживается".to_owned());
                }
                let table = format!(
                    "{}.{}",
                    quote_pg_identifier(&object.schema),
                    quote_pg_identifier(&object.name)
                );
                let set_value = if value.is_some() {
                    format!(
                        "CAST($1 AS {}.{})",
                        quote_pg_identifier(&column.udt_schema),
                        quote_pg_identifier(&column.udt_name)
                    )
                } else {
                    "NULL".to_owned()
                };
                let query = format!(
                    "UPDATE {table} SET {} = {set_value}",
                    quote_pg_identifier(column_name)
                );
                let result = if let Some(value) = value {
                    sqlx::query(&query).bind(value).execute(&mut *tx).await
                } else {
                    sqlx::query(&query).execute(&mut *tx).await
                }
                .map_err(|error| format_database_error(&error.to_string()))?;
                let count = result.rows_affected();
                tx.commit().await.map_err(|e| e.to_string())?;
                Ok(count)
            }
            Self::MySql(pool) => {
                let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
                mysql_validate_edit_table(&mut tx, object).await?;
                let columns = table_columns_mysql(&mut *tx, object).await?;
                let column = columns
                    .iter()
                    .find(|column| column.name == column_name)
                    .ok_or_else(|| format!("Столбец «{column_name}» больше не существует"))?;
                if column.primary_key || column.binary || column.generated {
                    return Err("Массовое изменение первичного ключа не поддерживается".to_owned());
                }
                let table = format!(
                    "{}.{}",
                    quote_mysql_identifier(&object.schema),
                    quote_mysql_identifier(&object.name)
                );
                let query = format!(
                    "UPDATE {table} SET {} = {}",
                    quote_mysql_identifier(column_name),
                    if value.is_some() { "?" } else { "NULL" }
                );
                let result = if let Some(value) = value {
                    sqlx::query(&query).bind(value).execute(&mut *tx).await
                } else {
                    sqlx::query(&query).execute(&mut *tx).await
                }
                .map_err(|error| format_database_error(&error.to_string()))?;
                let count = result.rows_affected();
                tx.commit().await.map_err(|e| e.to_string())?;
                Ok(count)
            }
        }
    }

    pub async fn export_table_csv_controlled(
        &self,
        object: &DbObject,
        file: &mut std::fs::File,
        cancelled: &std::sync::atomic::AtomicBool,
        mut progress: impl FnMut(usize) + Send,
    ) -> Result<usize, String> {
        use std::io::Write;
        use std::sync::atomic::Ordering;
        let mut writer = std::io::BufWriter::with_capacity(1024 * 1024, file);
        match self {
            Self::PostgreSql(pool) => {
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(|e| format_database_error(&e.to_string()))?
                    .detach();
                sqlx::query("BEGIN READ ONLY")
                    .execute(&mut connection)
                    .await
                    .map_err(|e| e.to_string())?;
                sqlx::query("SET LOCAL lock_timeout='5s'")
                    .execute(&mut connection)
                    .await
                    .map_err(|e| e.to_string())?;
                sqlx::query("SET LOCAL statement_timeout='300s'")
                    .execute(&mut connection)
                    .await
                    .map_err(|e| e.to_string())?;
                let columns = table_columns_pg(&mut connection, object).await?;
                let names = columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>();
                let selection = columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let identifier = quote_pg_identifier(&column.name);
                        let value = if column.binary {
                            format!("('0x' || ENCODE({identifier}, 'hex'))")
                        } else {
                            format!("CAST({identifier} AS TEXT)")
                        };
                        format!("{value} AS \"v{index}\"")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let table = format!(
                    "{}.{}",
                    quote_pg_identifier(&object.schema),
                    quote_pg_identifier(&object.name)
                );
                crate::write_csv_header(&mut writer, &names)?;
                let sql = format!("SELECT {selection} FROM {table}");
                let mut rows = sqlx::query(&sql).fetch(&mut connection);
                let mut count = 0;
                loop {
                    let row = loop {
                        if cancelled.load(Ordering::Acquire) {
                            return Err("Экспорт отменён".into());
                        }
                        match tokio::time::timeout(Duration::from_millis(250), rows.try_next())
                            .await
                        {
                            Ok(result) => {
                                break result.map_err(|e| format_database_error(&e.to_string()))?;
                            }
                            Err(_) => continue,
                        }
                    };
                    let Some(row) = row else {
                        break;
                    };
                    let values = (0..columns.len())
                        .map(|index| match row.try_get::<Option<String>, _>(index) {
                            Ok(Some(value)) => Ok(CellValue::Text(value)),
                            Ok(None) => Ok(CellValue::Null),
                            Err(error) => {
                                Err(format!("Не удалось прочитать значение для CSV: {error}"))
                            }
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    crate::write_csv_row(&mut writer, &values)?;
                    count += 1;
                    if count % 10_000 == 0 {
                        progress(count);
                    }
                }
                drop(rows);
                sqlx::query("ROLLBACK")
                    .execute(&mut connection)
                    .await
                    .map_err(|e| e.to_string())?;
                writer
                    .flush()
                    .map_err(|e| format!("Ошибка записи CSV: {e}"))?;
                progress(count);
                Ok(count)
            }
            Self::MySql(pool) => {
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(|e| format_database_error(&e.to_string()))?
                    .detach();
                sqlx::query("SET SESSION lock_wait_timeout=5")
                    .execute(&mut connection)
                    .await
                    .map_err(|e| e.to_string())?;
                sqlx::query("SET SESSION innodb_lock_wait_timeout=5")
                    .execute(&mut connection)
                    .await
                    .map_err(|e| e.to_string())?;
                sqlx::query("START TRANSACTION READ ONLY")
                    .execute(&mut connection)
                    .await
                    .map_err(|e| e.to_string())?;
                let columns = table_columns_mysql(&mut connection, object).await?;
                let names = columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>();
                let selection = columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let identifier = quote_mysql_identifier(&column.name);
                        let value = if column.binary {
                            format!("CONCAT('0x', HEX({identifier}))")
                        } else {
                            format!("CAST({identifier} AS CHAR)")
                        };
                        format!("{value} AS `v{index}`")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let table = format!(
                    "{}.{}",
                    quote_mysql_identifier(&object.schema),
                    quote_mysql_identifier(&object.name)
                );
                crate::write_csv_header(&mut writer, &names)?;
                let sql = format!("SELECT {selection} FROM {table}");
                let mut rows = sqlx::query(&sql).fetch(&mut connection);
                let mut count = 0;
                loop {
                    let row = loop {
                        if cancelled.load(Ordering::Acquire) {
                            return Err("Экспорт отменён".into());
                        }
                        match tokio::time::timeout(Duration::from_millis(250), rows.try_next())
                            .await
                        {
                            Ok(result) => {
                                break result.map_err(|e| format_database_error(&e.to_string()))?;
                            }
                            Err(_) => continue,
                        }
                    };
                    let Some(row) = row else {
                        break;
                    };
                    let values = (0..columns.len())
                        .map(|index| match row.try_get::<Option<String>, _>(index) {
                            Ok(Some(value)) => Ok(CellValue::Text(value)),
                            Ok(None) => Ok(CellValue::Null),
                            Err(error) => {
                                Err(format!("Не удалось прочитать значение для CSV: {error}"))
                            }
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    crate::write_csv_row(&mut writer, &values)?;
                    count += 1;
                    if count % 10_000 == 0 {
                        progress(count);
                    }
                }
                drop(rows);
                sqlx::query("ROLLBACK")
                    .execute(&mut connection)
                    .await
                    .map_err(|e| e.to_string())?;
                writer
                    .flush()
                    .map_err(|e| format!("Ошибка записи CSV: {e}"))?;
                progress(count);
                Ok(count)
            }
        }
    }
}

fn looks_like_row_query(sql: &str) -> bool {
    let statement = sql.trim_start().to_ascii_lowercase();
    ["select", "with", "values", "show", "describe", "explain"]
        .iter()
        .any(|prefix| statement.starts_with(prefix))
}

async fn table_columns_pg<'e, E: Executor<'e, Database = sqlx::Postgres>>(
    pool: E,
    object: &DbObject,
) -> Result<Vec<DbColumn>, String> {
    sqlx::query(
        "SELECT c.column_name AS column_name, c.data_type AS data_type, \
                c.udt_schema AS udt_schema, c.udt_name AS udt_name, \
                (c.is_identity <> 'NO' OR c.is_generated <> 'NEVER') AS is_generated, \
                EXISTS (SELECT 1 FROM information_schema.table_constraints tc \
                        JOIN information_schema.key_column_usage kcu \
                          ON kcu.constraint_catalog = tc.constraint_catalog \
                         AND kcu.constraint_schema = tc.constraint_schema \
                         AND kcu.constraint_name = tc.constraint_name \
                         AND kcu.table_schema = tc.table_schema \
                         AND kcu.table_name = tc.table_name \
                        WHERE tc.constraint_type = 'PRIMARY KEY' \
                          AND kcu.table_schema = c.table_schema \
                          AND kcu.table_name = c.table_name \
                          AND kcu.column_name = c.column_name) AS is_primary_key \
         FROM information_schema.columns c \
         WHERE c.table_schema = $1 AND c.table_name = $2 \
         ORDER BY c.ordinal_position",
    )
    .bind(&object.schema)
    .bind(&object.name)
    .fetch_all(pool)
    .await
    .map_err(|error| format_database_error(&error.to_string()))?
    .into_iter()
    .map(|row| {
        let name = row
            .try_get("column_name")
            .map_err(|error| error.to_string())?;
        let data_type: String = row
            .try_get("data_type")
            .map_err(|error| error.to_string())?;
        let udt_schema: String = row
            .try_get("udt_schema")
            .map_err(|error| error.to_string())?;
        let udt_name: String = row.try_get("udt_name").map_err(|error| error.to_string())?;
        let primary_key: bool = row
            .try_get("is_primary_key")
            .map_err(|error| error.to_string())?;
        let generated: bool = row
            .try_get("is_generated")
            .map_err(|error| error.to_string())?;
        Ok(DbColumn {
            name,
            data_type: data_type.clone(),
            binary: data_type.eq_ignore_ascii_case("bytea"),
            primary_key,
            generated,
            udt_schema,
            udt_name,
        })
    })
    .collect()
}

// information_schema intentionally omits MySQL TEMPORARY tables, which can
// shadow even a fully-qualified permanent table on another pooled connection.
async fn mysql_is_base_table(
    connection: &mut sqlx::MySqlConnection,
    object: &DbObject,
) -> Result<bool, String> {
    let ddl = sqlx::query(&format!(
        "SHOW CREATE TABLE {}.{}",
        quote_mysql_identifier(&object.schema),
        quote_mysql_identifier(&object.name)
    ))
    .fetch_one(connection)
    .await
    .map_err(|e| format_database_error(&e.to_string()))?;
    let definition: String = ddl.try_get(1).map_err(|_| "Нет определения таблицы")?;
    Ok(definition.starts_with("CREATE TABLE"))
}

async fn table_columns_mysql<'e, E: Executor<'e, Database = sqlx::MySql>>(
    pool: E,
    object: &DbObject,
) -> Result<Vec<DbColumn>, String> {
    sqlx::query(
        "SELECT CAST(column_name AS CHAR) AS column_name, \
                CAST(data_type AS CHAR) AS data_type, \
                CAST(character_set_name AS CHAR) AS character_set_name, \
                CAST(extra AS CHAR) LIKE '%GENERATED%' OR CAST(extra AS CHAR) LIKE '%auto_increment%' AS is_generated, \
                CAST(column_key AS CHAR) = 'PRI' AS is_primary_key \
         FROM information_schema.columns \
         WHERE table_schema = ? AND table_name = ? \
         ORDER BY ordinal_position",
    )
    .bind(&object.schema)
    .bind(&object.name)
    .fetch_all(pool)
    .await
    .map_err(|error| format_database_error(&error.to_string()))?
    .into_iter()
    .map(|row| {
        let name = row
            .try_get("column_name")
            .map_err(|error| error.to_string())?;
        let data_type: String = row
            .try_get("data_type")
            .map_err(|error| error.to_string())?;
        let character_set: Option<String> = row
            .try_get("character_set_name")
            .map_err(|error| error.to_string())?;
        let primary_key: bool = row
            .try_get("is_primary_key")
            .map_err(|error| error.to_string())?;
        let generated: bool = row
            .try_get("is_generated")
            .map_err(|error| error.to_string())?;
        Ok(DbColumn {
            name,
            data_type: data_type.clone(),
            binary: is_mysql_binary_type(&data_type, character_set.as_deref()),
            primary_key,
            generated,
            udt_schema: String::new(),
            udt_name: data_type,
        })
    })
    .collect()
}

fn to_table_data<R, F>(
    columns: Vec<String>,
    mut rows: Vec<R>,
    primary_key: Vec<String>,
    truncated_columns: bool,
    cell: F,
) -> TableData
where
    F: Fn(&R, usize) -> CellValue,
{
    let truncated = rows.len() > MAX_TABLE_ROWS;
    rows.truncate(MAX_TABLE_ROWS);
    let rows = rows
        .iter()
        .map(|row| (0..columns.len()).map(|index| cell(row, index)).collect())
        .collect();
    TableData {
        columns,
        rows,
        primary_key,
        truncated,
        truncated_columns,
    }
}

fn limit_columns(mut columns: Vec<DbColumn>) -> (Vec<DbColumn>, bool) {
    let truncated = columns.len() > MAX_TABLE_COLUMNS;
    columns.truncate(MAX_TABLE_COLUMNS);
    (columns, truncated)
}

fn is_mysql_binary_type(data_type: &str, character_set: Option<&str>) -> bool {
    character_set.is_some_and(|charset| charset.eq_ignore_ascii_case("binary"))
        || matches!(
            data_type.to_ascii_lowercase().as_str(),
            "binary"
                | "varbinary"
                | "bit"
                | "tinyblob"
                | "blob"
                | "mediumblob"
                | "longblob"
                | "geometry"
                | "point"
                | "linestring"
                | "polygon"
                | "multipoint"
                | "multilinestring"
                | "multipolygon"
                | "geometrycollection"
        )
}

fn table_catalog(
    schemas: Vec<String>,
    mut objects: impl Iterator<Item = Result<DbObject, String>>,
) -> Result<TableCatalog, String> {
    let mut collected = Vec::new();
    for object in objects.by_ref().take(MAX_CATALOG_OBJECTS + 1) {
        collected.push(object?);
    }
    let truncated = collected.len() > MAX_CATALOG_OBJECTS;
    collected.truncate(MAX_CATALOG_OBJECTS);
    Ok(TableCatalog {
        schemas,
        objects: collected,
        truncated,
    })
}

fn quote_pg_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn quote_mysql_identifier(identifier: &str) -> String {
    format!("`{}`", identifier.replace('`', "``"))
}

#[derive(Clone, Copy)]
enum SqlDialect {
    PostgreSql,
    MySql,
}

// Keep the fragment inside one parenthesized expression in both SQL dialects.
// Quotes use portable doubled delimiters; reject ambiguous backslash escapes.
pub fn validate_raw_condition(input: &str) -> Result<String, String> {
    let input = input.trim();
    let input = if input
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("where"))
        && input[5..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_whitespace())
    {
        input[5..].trim()
    } else {
        input
    };
    let mut chars = input.chars().peekable();
    let mut quote = None;
    let mut depth = 0usize;
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            return Err(
                "В SQL-фильтре используйте удвоенные кавычки вместо обратного слеша".to_owned(),
            );
        }
        if let Some(delimiter) = quote {
            if ch == delimiter {
                if chars.peek() == Some(&delimiter) {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => quote = Some(ch),
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            ')' => return Err("Лишняя закрывающая скобка в SQL-фильтре".to_owned()),
            ';' | '#' | '$' => {
                return Err(
                    "Введите одно SQL-условие без команд, комментариев и параметров".to_owned(),
                );
            }
            '-' if chars.peek() == Some(&'-') => {
                return Err("Комментарии в SQL-фильтре не поддерживаются".to_owned());
            }
            '/' if chars.peek() == Some(&'*') => {
                return Err("Комментарии в SQL-фильтре не поддерживаются".to_owned());
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return Err("В SQL-фильтре не закрыты кавычки или скобки".to_owned());
    }
    let sql = format!("SELECT 1 WHERE {input}");
    if [
        crate::sql_risk::Dialect::PostgreSql,
        crate::sql_risk::Dialect::MySql,
    ]
    .into_iter()
    .any(|dialect| crate::sql_risk::analyze(&sql, dialect) == crate::sql_risk::SqlRisk::ChangesData)
    {
        return Err("SQL-фильтр не должен вызывать функции изменения состояния, блокировок или чтения файлов сервера".into());
    }
    Ok(input.to_owned())
}

fn build_view_where_clause(
    filters: &[TableFilter],
    raw_condition: Option<&str>,
    columns: &[DbColumn],
    dialect: SqlDialect,
) -> Result<(String, Vec<String>), String> {
    if let Some(condition) = raw_condition {
        Ok((
            if condition.is_empty() {
                String::new()
            } else {
                format!(" WHERE ({condition})")
            },
            Vec::new(),
        ))
    } else {
        build_where_clause(filters, columns, dialect)
    }
}

fn build_where_clause(
    filters: &[TableFilter],
    columns: &[DbColumn],
    dialect: SqlDialect,
) -> Result<(String, Vec<String>), String> {
    let mut clauses = Vec::new();
    let mut binds = Vec::new();
    for filter in filters {
        let column = columns
            .iter()
            .find(|column| column.name == filter.column)
            .ok_or_else(|| format!("Столбец «{}» больше не существует", filter.column))?;
        let identifier = match dialect {
            SqlDialect::PostgreSql => quote_pg_identifier(&column.name),
            SqlDialect::MySql => quote_mysql_identifier(&column.name),
        };
        let text_identifier = match dialect {
            SqlDialect::PostgreSql => format!("CAST({identifier} AS TEXT)"),
            SqlDialect::MySql => format!("CAST({identifier} AS CHAR)"),
        };
        let mut parameter = |value: &str| {
            binds.push(value.to_owned());
            match dialect {
                SqlDialect::PostgreSql => format!("${}", binds.len()),
                SqlDialect::MySql => "?".to_owned(),
            }
        };
        let clause = match filter.operator {
            FilterOperator::IsNull => format!("{identifier} IS NULL"),
            FilterOperator::IsNotNull => format!("{identifier} IS NOT NULL"),
            FilterOperator::Contains | FilterOperator::NotContains => {
                let value = parameter(&filter.value);
                let contains = match dialect {
                    SqlDialect::PostgreSql => format!("POSITION({value} IN {text_identifier}) > 0"),
                    SqlDialect::MySql => format!("LOCATE({value}, {text_identifier}) > 0"),
                };
                if filter.operator == FilterOperator::NotContains {
                    format!("NOT ({contains})")
                } else {
                    contains
                }
            }
            FilterOperator::StartsWith | FilterOperator::EndsWith => {
                let value = parameter(&filter.value);
                let length = match dialect {
                    SqlDialect::PostgreSql => format!("CHAR_LENGTH({value})"),
                    SqlDialect::MySql => format!("CHAR_LENGTH({value})"),
                };
                let edge = if filter.operator == FilterOperator::StartsWith {
                    "LEFT"
                } else {
                    "RIGHT"
                };
                let compared_value = parameter(&filter.value);
                format!("{edge}({text_identifier}, {length}) = {compared_value}")
            }
            FilterOperator::In | FilterOperator::NotIn => {
                let values = filter
                    .value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>();
                if values.is_empty() {
                    return Err(format!(
                        "Укажите значения через запятую для фильтра столбца «{}»",
                        filter.column
                    ));
                }
                let parameters = values
                    .iter()
                    .map(|value| {
                        let raw = parameter(value);
                        match dialect {
                            SqlDialect::PostgreSql => pg_comparison_cast(&column.data_type)
                                .map_or(raw.clone(), |type_name| {
                                    pg_typed_parameter(&raw, type_name)
                                }),
                            SqlDialect::MySql => raw,
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let left = match dialect {
                    SqlDialect::PostgreSql => pg_comparison_cast(&column.data_type)
                        .map_or(text_identifier, |type_name| {
                            format!("CAST({identifier} AS {type_name})")
                        }),
                    SqlDialect::MySql => identifier,
                };
                format!(
                    "{left} {} ({parameters})",
                    if filter.operator == FilterOperator::NotIn {
                        "NOT IN"
                    } else {
                        "IN"
                    }
                )
            }
            operator => {
                let symbol = match operator {
                    FilterOperator::Equal => "=",
                    FilterOperator::NotEqual => "<>",
                    FilterOperator::Greater => ">",
                    FilterOperator::GreaterEqual => ">=",
                    FilterOperator::Less => "<",
                    FilterOperator::LessEqual => "<=",
                    _ => unreachable!(),
                };
                let left = match dialect {
                    SqlDialect::PostgreSql => pg_comparison_cast(&column.data_type)
                        .map_or(text_identifier, |type_name| {
                            format!("CAST({identifier} AS {type_name})")
                        }),
                    SqlDialect::MySql => identifier,
                };
                let raw_parameter = parameter(&filter.value);
                let value = match dialect {
                    SqlDialect::PostgreSql => match pg_comparison_cast(&column.data_type) {
                        Some(type_name) => pg_typed_parameter(&raw_parameter, type_name),
                        None => raw_parameter,
                    },
                    SqlDialect::MySql => raw_parameter,
                };
                format!("{left} {symbol} {value}")
            }
        };
        clauses.push((clause, filter.logic));
    }
    if clauses.is_empty() {
        Ok((String::new(), binds))
    } else {
        let mut expression = clauses[0].0.clone();
        for (clause, logic) in clauses.iter().skip(1) {
            expression = format!(
                "({expression}) {} ({clause})",
                if *logic == FilterLogic::Or {
                    "OR"
                } else {
                    "AND"
                }
            );
        }
        Ok((format!(" WHERE {expression}"), binds))
    }
}

fn pg_typed_parameter(parameter: &str, type_name: &str) -> String {
    format!("CAST(CAST({parameter} AS TEXT) AS {type_name})")
}

fn build_order_clause(
    sort: Option<&TableSort>,
    columns: &[DbColumn],
    dialect: SqlDialect,
) -> Result<String, String> {
    let Some(sort) = sort else {
        return Ok(String::new());
    };
    let column = columns
        .iter()
        .find(|column| column.name == sort.column)
        .ok_or_else(|| format!("Столбец «{}» больше не существует", sort.column))?;
    let identifier = match dialect {
        SqlDialect::PostgreSql => quote_pg_identifier(&column.name),
        SqlDialect::MySql => quote_mysql_identifier(&column.name),
    };
    Ok(format!(
        " ORDER BY {identifier} {}",
        if sort.descending { "DESC" } else { "ASC" }
    ))
}

fn build_stable_order_clause(
    sort: Option<&TableSort>,
    columns: &[DbColumn],
    primary_key: &[String],
    dialect: SqlDialect,
) -> Result<String, String> {
    let mut order = build_order_clause(sort, columns, dialect)?;
    let tie_breakers = primary_key
        .iter()
        .filter(|key| !sort.is_some_and(|sort| sort.column == **key))
        .map(|key| {
            let identifier = match dialect {
                SqlDialect::PostgreSql => quote_pg_identifier(key),
                SqlDialect::MySql => quote_mysql_identifier(key),
            };
            format!("{identifier} ASC")
        })
        .collect::<Vec<_>>();
    if !tie_breakers.is_empty() {
        if order.is_empty() {
            order.push_str(" ORDER BY ");
        } else {
            order.push_str(", ");
        }
        order.push_str(&tie_breakers.join(", "));
    }
    Ok(order)
}

fn pg_comparison_cast(data_type: &str) -> Option<&'static str> {
    let data_type = data_type.to_ascii_lowercase();
    if matches!(
        data_type.as_str(),
        "smallint"
            | "integer"
            | "bigint"
            | "numeric"
            | "decimal"
            | "real"
            | "double precision"
            | "money"
    ) {
        Some("NUMERIC")
    } else if matches!(data_type.as_str(), "boolean") {
        Some("BOOLEAN")
    } else if data_type == "date" {
        Some("DATE")
    } else if data_type.starts_with("timestamp with time zone") {
        Some("TIMESTAMPTZ")
    } else if data_type.starts_with("timestamp") {
        Some("TIMESTAMP")
    } else if data_type.starts_with("time with time zone") {
        Some("TIMETZ")
    } else if data_type.starts_with("time") {
        Some("TIME")
    } else {
        None
    }
}

fn format_database_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("unknown message type") {
        "Сервер ответил по неизвестному протоколу. Проверьте, что выбран правильный движок и порт (PostgreSQL или MySQL/MariaDB).".to_owned()
    } else if lower.contains("password authentication failed") || lower.contains("access denied") {
        "Ошибка аутентификации: проверьте пользователя и пароль".to_owned()
    } else if lower.contains("connection refused") || lower.contains("timed out") {
        "Не удалось подключиться: проверьте адрес, порт и доступность сервера".to_owned()
    } else {
        error.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CellValue, DbColumn, DbObject, DbObjectKind, MAX_CATALOG_OBJECTS, MAX_TABLE_COLUMNS,
        MAX_TABLE_ROWS, is_mysql_binary_type, limit_columns, quote_mysql_identifier,
        quote_pg_identifier, table_catalog, to_table_data, validate_raw_condition,
    };

    #[test]
    fn raw_condition_is_one_balanced_expression() {
        for (input, expected) in [
            (
                "WHERE x != 4 AND (name = 'Bob' OR id > 10)",
                "x != 4 AND (name = 'Bob' OR id > 10)",
            ),
            (
                "where x in (select id from t where id > 2)",
                "x in (select id from t where id > 2)",
            ),
            ("note = 'it''s ; -- text'", "note = 'it''s ; -- text'"),
            ("wherever = 2", "wherever = 2"),
            ("", ""),
        ] {
            assert_eq!(validate_raw_condition(input).unwrap(), expected);
        }
        for input in [
            "1); DELETE FROM t; --",
            "x = 1 -- comment",
            "x = 1 /* comment */",
            "(x = 1",
            "x = 'open",
            "x = $$a$$",
            "x = 1 #comment",
            "x = 1; SELECT 2",
            r"x = 'a\' OR true",
            "GET_LOCK('x',0)=1",
            "pg_catalog.\"pg_try_advisory_lock\"(42)",
        ] {
            assert!(validate_raw_condition(input).is_err(), "{input}");
        }
    }

    #[test]
    fn quote_identifiers_escapes_delimiters() {
        assert_eq!(quote_pg_identifier("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_mysql_identifier("a`b"), "`a``b`");
    }

    #[test]
    fn row_limit_keeps_exactly_maximum_and_marks_truncation() {
        let data = to_table_data(
            vec!["id".to_owned()],
            (0..=MAX_TABLE_ROWS).collect::<Vec<_>>(),
            Vec::new(),
            false,
            |row, _| CellValue::Text(row.to_string()),
        );
        assert_eq!(data.rows.len(), MAX_TABLE_ROWS);
        assert!(data.truncated);
        assert!(!data.truncated_columns);
    }

    #[test]
    fn row_limit_does_not_mark_short_results_as_truncated() {
        let data = to_table_data(
            vec!["id".to_owned()],
            vec![1, 2],
            Vec::new(),
            false,
            |row, _| CellValue::Text(row.to_string()),
        );
        assert_eq!(data.rows.len(), 2);
        assert!(!data.truncated);
    }

    #[test]
    fn exactly_maximum_rows_are_not_reported_as_truncated() {
        let data = to_table_data(
            vec!["id".to_owned()],
            (0..MAX_TABLE_ROWS).collect::<Vec<_>>(),
            Vec::new(),
            false,
            |row, _| CellValue::Text(row.to_string()),
        );
        assert_eq!(data.rows.len(), MAX_TABLE_ROWS);
        assert!(!data.truncated);
    }

    #[test]
    fn column_limit_marks_only_more_than_maximum() {
        let exact = (0..MAX_TABLE_COLUMNS)
            .map(|i| DbColumn {
                name: i.to_string(),
                data_type: "integer".to_owned(),
                binary: false,
                primary_key: false,
                generated: false,
                udt_schema: "pg_catalog".to_owned(),
                udt_name: "int4".to_owned(),
            })
            .collect();
        let (exact, was_truncated) = limit_columns(exact);
        assert_eq!(exact.len(), MAX_TABLE_COLUMNS);
        assert!(!was_truncated);

        let extra = (0..=MAX_TABLE_COLUMNS)
            .map(|i| DbColumn {
                name: i.to_string(),
                data_type: "integer".to_owned(),
                binary: false,
                primary_key: false,
                generated: false,
                udt_schema: "pg_catalog".to_owned(),
                udt_name: "int4".to_owned(),
            })
            .collect();
        let (extra, was_truncated) = limit_columns(extra);
        assert_eq!(extra.len(), MAX_TABLE_COLUMNS);
        assert!(was_truncated);
        assert_eq!(
            extra[MAX_TABLE_COLUMNS - 1].name,
            (MAX_TABLE_COLUMNS - 1).to_string()
        );
    }

    #[test]
    fn mysql_binary_column_types_are_detected() {
        assert!(is_mysql_binary_type("BLOB", None));
        assert!(is_mysql_binary_type("varbinary", None));
        assert!(is_mysql_binary_type("BIT", None));
        assert!(is_mysql_binary_type("point", None));
        assert!(is_mysql_binary_type("varchar", Some("binary")));
        assert!(!is_mysql_binary_type("text", Some("utf8mb4")));
        assert!(!is_mysql_binary_type("varchar", Some("utf8mb4")));
    }

    #[test]
    fn catalog_limit_reports_truncation_only_when_extra_object_exists() {
        let at_limit = table_catalog(
            vec!["public".to_owned()],
            (0..MAX_CATALOG_OBJECTS).map(|index| {
                Ok(DbObject {
                    schema: "public".to_owned(),
                    name: index.to_string(),
                    kind: DbObjectKind::Table,
                })
            }),
        )
        .unwrap();
        assert_eq!(at_limit.objects.len(), MAX_CATALOG_OBJECTS);
        assert!(!at_limit.truncated);

        let over_limit = table_catalog(
            vec!["public".to_owned()],
            (0..=MAX_CATALOG_OBJECTS).map(|index| {
                Ok(DbObject {
                    schema: "public".to_owned(),
                    name: index.to_string(),
                    kind: DbObjectKind::Table,
                })
            }),
        )
        .unwrap();
        assert_eq!(over_limit.objects.len(), MAX_CATALOG_OBJECTS);
        assert!(over_limit.truncated);
    }
}

#[cfg(test)]
pub(crate) fn execute_on_connection<'a>(
    connection: QueryConnection<'a>,
    sql: &'a str,
) -> futures_util::future::BoxFuture<'a, Result<QueryResult, String>> {
    execute_on_connection_controlled(connection, sql, None, None)
}

pub(crate) fn execute_on_connection_controlled<'a>(
    connection: QueryConnection<'a>,
    sql: &'a str,
    cancelled: Option<&'a std::sync::atomic::AtomicBool>,
    execution_started: Option<&'a std::sync::atomic::AtomicBool>,
) -> futures_util::future::BoxFuture<'a, Result<QueryResult, String>> {
    execute_on_connection_mode(connection, sql, cancelled, execution_started, false)
}

pub(crate) fn execute_read_only_on_connection<'a>(
    connection: QueryConnection<'a>,
    sql: &'a str,
    cancelled: Option<&'a std::sync::atomic::AtomicBool>,
    execution_started: Option<&'a std::sync::atomic::AtomicBool>,
) -> futures_util::future::BoxFuture<'a, Result<QueryResult, String>> {
    execute_on_connection_mode(connection, sql, cancelled, execution_started, true)
}
fn execute_on_connection_mode<'a>(
    connection: QueryConnection<'a>,
    sql: &'a str,
    cancelled: Option<&'a std::sync::atomic::AtomicBool>,
    execution_started: Option<&'a std::sync::atomic::AtomicBool>,
    prepared: bool,
) -> futures_util::future::BoxFuture<'a, Result<QueryResult, String>> {
    Box::pin(async move {
        let mut columns = Vec::new();
        let mut rows = Vec::new();
        let mut rows_affected = 0u64;
        let mut row_count = 0usize;
        let mut truncated_columns = false;
        let mut current_columns = Vec::new();
        let mut current_rows = Vec::new();
        let mut current_row_count = 0usize;
        let mut current_truncated_columns = false;
        let mut current_row_bytes = 0usize;

        macro_rules! collect_query {
            ($pool:expr, $cell_fn:ident) => {{
                // Validate a single statement using the server parser on this same
                // connection. Execute in text format: binary result values cannot be
                // decoded generically as strings (integers may contain NUL bytes).
                if prepared {
                    ($pool)
                        .prepare(sql)
                        .await
                        .map_err(|error| format_database_error(&error.to_string()))?;
                }
                let mut stream = sqlx::raw_sql(sql).fetch_many($pool).boxed();
                while let Some(step) = stream
                    .try_next()
                    .await
                    .map_err(|error| format_database_error(&error.to_string()))?
                {
                    match step {
                        sqlx::Either::Left(result) => {
                            rows_affected = rows_affected.saturating_add(result.rows_affected());
                            if !current_columns.is_empty() {
                                columns = std::mem::take(&mut current_columns);
                                rows = std::mem::take(&mut current_rows);
                                row_count = current_row_count;
                                truncated_columns = current_truncated_columns;
                                current_row_count = 0;
                                current_row_bytes = 0;
                                current_truncated_columns = false;
                            }
                        }
                        sqlx::Either::Right(row) => {
                            if current_columns.is_empty() {
                                // Only the last nonempty result is displayed. Release the
                                // previous preview before collecting another bounded set.
                                rows = Vec::new();
                                current_truncated_columns = row.columns().len() > MAX_QUERY_COLUMNS;
                                current_columns = row
                                    .columns()
                                    .iter()
                                    .take(MAX_QUERY_COLUMNS)
                                    .map(|column| column.name().to_owned())
                                    .collect();
                            }
                            current_row_count = current_row_count.saturating_add(1);
                            debug_assert!(rows.is_empty());
                            if current_rows.len() < MAX_QUERY_ROWS
                                && current_row_bytes < MAX_PREVIEW_BYTES
                            {
                                let values: Vec<CellValue> = (0..current_columns.len())
                                    .map(|index| $cell_fn(&row, index))
                                    .collect();
                                let next_bytes =
                                    current_row_bytes.saturating_add(row_memory(&values));
                                if next_bytes <= MAX_PREVIEW_BYTES {
                                    current_rows.push(values);
                                    current_row_bytes = next_bytes;
                                } else {
                                    current_row_bytes = MAX_PREVIEW_BYTES;
                                }
                            }
                        }
                    }
                }
                if prepared && !current_columns.is_empty() {
                    columns = std::mem::take(&mut current_columns);
                    rows = std::mem::take(&mut current_rows);
                    row_count = current_row_count;
                    truncated_columns = current_truncated_columns;
                }
            }};
        }

        let candidate = match connection {
            QueryConnection::PostgreSql(connection) => {
                let candidate =
                    DbPool::query_edit_candidate(QueryConnection::PostgreSql(connection), sql)
                        .await
                        .ok()
                        .flatten();
                if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
                    return Err("Запрос отменён до выполнения SQL".into());
                }
                if let Some(flag) = execution_started {
                    flag.store(true, std::sync::atomic::Ordering::Release);
                }
                collect_query!(&mut *connection, query_cell_postgres);
                if columns.is_empty() && looks_like_row_query(sql) {
                    if let Ok(description) = connection.describe(sql).await {
                        truncated_columns = description.columns().len() > MAX_QUERY_COLUMNS;
                        columns = description
                            .columns()
                            .iter()
                            .take(MAX_QUERY_COLUMNS)
                            .map(|c| c.name().to_owned())
                            .collect();
                    }
                }
                candidate
            }
            QueryConnection::MySql(connection) => {
                let candidate =
                    DbPool::query_edit_candidate(QueryConnection::MySql(connection), sql)
                        .await
                        .ok()
                        .flatten();
                if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
                    return Err("Запрос отменён до выполнения SQL".into());
                }
                if let Some(flag) = execution_started {
                    flag.store(true, std::sync::atomic::Ordering::Release);
                }
                collect_query!(&mut *connection, query_cell_mysql);
                if columns.is_empty() && looks_like_row_query(sql) {
                    if let Ok(description) = connection.describe(sql).await {
                        truncated_columns = description.columns().len() > MAX_QUERY_COLUMNS;
                        columns = description
                            .columns()
                            .iter()
                            .take(MAX_QUERY_COLUMNS)
                            .map(|c| c.name().to_owned())
                            .collect();
                    }
                }
                candidate
            }
        };

        let mut primary_key = Vec::new();
        let mut read_only_columns = Vec::new();
        let editable_object = if !truncated_columns {
            match candidate.and_then(|candidate| {
                DbPool::validate_query_edit_target(candidate, &columns, &rows)
            }) {
                Some((object, keys, blocked)) => {
                    primary_key = keys;
                    read_only_columns = blocked;
                    Some(object)
                }
                _ => None,
            }
        } else {
            None
        };
        let truncated = row_count > rows.len();
        Ok(QueryResult {
            editable_object,
            read_only_columns,
            data: std::sync::Arc::new(TableData {
                columns,
                rows,
                primary_key,
                truncated,
                truncated_columns,
            }),
            rows_affected,
            row_count,
        })
    })
}

async fn mysql_validate_edit_table(
    connection: &mut sqlx::MySqlConnection,
    object: &DbObject,
) -> Result<(), String> {
    let table = format!(
        "{}.{}",
        quote_mysql_identifier(&object.schema),
        quote_mysql_identifier(&object.name)
    );
    // EXECUTE acquires a transaction-scoped metadata lock, including on MyISAM.
    // Keep it before SHOW CREATE/ENGINE/column checks to exclude concurrent DDL.
    sqlx::query(&format!("SELECT 1 FROM {table} LIMIT 0"))
        .execute(&mut *connection)
        .await
        .map_err(|error| format_database_error(&error.to_string()))?;
    if !mysql_is_base_table(&mut *connection, object).await? {
        return Err("Временная таблица перекрывает исходную: изменения отменены".into());
    }
    let engine = sqlx::query_scalar::<_, String>(
        "SELECT CAST(ENGINE AS CHAR) FROM information_schema.tables WHERE table_schema = ? AND table_name = ?")
        .bind(&object.schema).bind(&object.name).fetch_optional(&mut *connection).await
        .map_err(|error| format_database_error(&error.to_string()))?;
    if !engine
        .as_deref()
        .is_some_and(|engine| engine.eq_ignore_ascii_case("InnoDB"))
    {
        return Err("Сохранение пакета правок поддерживается только для InnoDB: требуется транзакционный откат.".into());
    }
    let mode: String = sqlx::query_scalar("SELECT @@SESSION.sql_mode")
        .fetch_one(&mut *connection)
        .await
        .map_err(|e| e.to_string())?;
    if !mode
        .split(',')
        .any(|m| m == "STRICT_TRANS_TABLES" || m == "STRICT_ALL_TABLES")
    {
        return Err("Изменение данных требует STRICT_TRANS_TABLES или STRICT_ALL_TABLES: нестрогий режим может незаметно обрезать значения.".into());
    }
    Ok(())
}

async fn mysql_lock_matches(
    connection: &mut sqlx::MySqlConnection,
    table: &str,
    column: &str,
    predicates: &str,
    keys: &[String],
    original: Option<&str>,
) -> Result<bool, String> {
    let sql = format!(
        "SELECT CAST({} AS CHAR) FROM {table} WHERE {predicates} FOR UPDATE",
        quote_mysql_identifier(column)
    );
    let mut query = sqlx::query(&sql);
    for key in keys {
        query = query.bind(key);
    }
    let row = query
        .fetch_optional(connection)
        .await
        .map_err(|error| format_database_error(&error.to_string()))?;
    match row {
        Some(row) => Ok(row
            .try_get::<Option<String>, _>(0)
            .map_err(|error| format_database_error(&error.to_string()))?
            .as_deref()
            == original),
        None => Ok(false),
    }
}

fn editable_original<'a>(
    edit: &'a TableCellEdit,
    column: &DbColumn,
) -> Result<Option<&'a str>, String> {
    if column.binary || column.generated {
        return Err("Бинарный или вычисляемый столбец недоступен для изменения".into());
    }
    match &edit.original_value {
        CellValue::Text(value) => Ok(Some(value)),
        CellValue::Null => Ok(None),
        CellValue::Truncated(_) | CellValue::DecodeError(_) => {
            Err("Нельзя перезаписать обрезанное или непрочитанное значение".into())
        }
    }
}

pub(crate) fn update_table_cells_on<'a>(
    connection: QueryConnection<'a>,
    object: &'a DbObject,
    edits: &'a [TableCellEdit],
) -> futures_util::future::BoxFuture<'a, Result<usize, String>> {
    Box::pin(async move {
        if edits.is_empty() {
            return Ok(0);
        }
        if object.kind != DbObjectKind::Table {
            return Err("Редактирование представлений не поддерживается".to_owned());
        }
        match connection {
            QueryConnection::PostgreSql(connection) => {
                let table = format!(
                    "{}.{}",
                    quote_pg_identifier(&object.schema),
                    quote_pg_identifier(&object.name)
                );
                let mut transaction = connection
                    .begin()
                    .await
                    .map_err(|error| format_database_error(&error.to_string()))?;
                // Execute a table access (not only PREPARE) to hold its metadata lock
                // until commit/rollback. DDL cannot replace the table/engine while
                // we validate metadata and apply the optimistic batch.
                sqlx::query(&format!("SELECT 1 FROM {table} LIMIT 0"))
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| format_database_error(&error.to_string()))?;
                let columns = table_columns_pg(&mut *transaction, object).await?;
                let keys = columns
                    .iter()
                    .filter(|column| column.primary_key)
                    .collect::<Vec<_>>();
                if keys.is_empty() {
                    return Err(
                        "Таблицу нельзя безопасно редактировать без первичного ключа".to_owned(),
                    );
                }

                for edit in edits {
                    if edit.primary_key_values.len() != keys.len() {
                        return Err("Не совпали значения первичного ключа строки".to_owned());
                    }
                    let target = columns
                        .iter()
                        .find(|column| column.name == edit.column)
                        .ok_or_else(|| format!("Столбец «{}» больше не существует", edit.column))?;
                    if target.primary_key {
                        return Err(
                            "Изменение первичного ключа через таблицу не поддерживается".to_owned()
                        );
                    }
                    let original = editable_original(edit, target)?;
                    let mut binds = Vec::new();
                    let set_expression = if let Some(value) = &edit.value {
                        binds.push(value.clone());
                        format!(
                            "CAST($1 AS {}.{})",
                            quote_pg_identifier(&target.udt_schema),
                            quote_pg_identifier(&target.udt_name)
                        )
                    } else {
                        "NULL".to_owned()
                    };
                    let mut predicates = Vec::with_capacity(keys.len());
                    for (key, value) in keys.iter().zip(&edit.primary_key_values) {
                        let value = match value {
                            CellValue::Text(value) => value.clone(),
                            CellValue::Null => {
                                return Err("Первичный ключ строки содержит NULL".to_owned());
                            }
                            CellValue::Truncated(_) | CellValue::DecodeError(_) => {
                                return Err(
                                    "Нельзя определить первичный ключ этой строки".to_owned()
                                );
                            }
                        };
                        binds.push(value);
                        let parameter = binds.len();
                        predicates.push(format!(
                            "{} = CAST(${parameter} AS {}.{})",
                            quote_pg_identifier(&key.name),
                            quote_pg_identifier(&key.udt_schema),
                            quote_pg_identifier(&key.udt_name)
                        ));
                    }
                    binds.push(original.unwrap_or_default().to_owned());
                    let parameter = binds.len();
                    // Binary collation preserves case and whitespace even on databases
                    // configured with nondeterministic text collations. NULL is separate.
                    predicates.push(if original.is_some() {
                        format!("CAST({} AS TEXT) COLLATE \"C\" = CAST(${parameter} AS TEXT) COLLATE \"C\"", quote_pg_identifier(&edit.column))
                    } else {
                        // Keep the final parameter typed and referenced in both branches.
                        format!("{} IS NULL AND CAST(${parameter} AS TEXT) = ''", quote_pg_identifier(&edit.column))
                    });
                    let sql = format!(
                        "UPDATE {table} SET {} = {set_expression} WHERE {}",
                        quote_pg_identifier(&edit.column),
                        predicates.join(" AND ")
                    );
                    let mut query = sqlx::query(&sql);
                    for value in binds {
                        query = query.bind(value);
                    }
                    let result = query
                        .execute(&mut *transaction)
                        .await
                        .map_err(|error| format_database_error(&error.to_string()))?;
                    if result.rows_affected() != 1 {
                        return Err(format!(
                            "Конфликт: значение изменилось или строка удалена (совпадений: {}). Обновите таблицу; весь пакет изменений отменён.",
                            result.rows_affected()
                        ));
                    }
                }
                transaction
                    .commit()
                    .await
                    .map_err(|error| format_database_error(&error.to_string()))?;
                Ok(edits.len())
            }
            QueryConnection::MySql(connection) => {
                let table = format!(
                    "{}.{}",
                    quote_mysql_identifier(&object.schema),
                    quote_mysql_identifier(&object.name)
                );
                let mut transaction = connection
                    .begin()
                    .await
                    .map_err(|error| format_database_error(&error.to_string()))?;
                mysql_validate_edit_table(&mut transaction, object).await?;
                let columns = table_columns_mysql(&mut *transaction, object).await?;
                let keys = columns
                    .iter()
                    .filter(|column| column.primary_key)
                    .collect::<Vec<_>>();
                if keys.is_empty() {
                    return Err(
                        "Таблицу нельзя безопасно редактировать без первичного ключа".to_owned(),
                    );
                }

                for edit in edits {
                    if edit.primary_key_values.len() != keys.len() {
                        return Err("Не совпали значения первичного ключа строки".to_owned());
                    }
                    let target = columns
                        .iter()
                        .find(|column| column.name == edit.column)
                        .ok_or_else(|| format!("Столбец «{}» больше не существует", edit.column))?;
                    if target.primary_key {
                        return Err(
                            "Изменение первичного ключа через таблицу не поддерживается".to_owned()
                        );
                    }
                    let original = editable_original(edit, target)?;
                    let set_expression = if edit.value.is_some() { "?" } else { "NULL" };
                    let mut key_binds = Vec::new();
                    let mut predicates = Vec::with_capacity(keys.len());
                    for (key, value) in keys.iter().zip(&edit.primary_key_values) {
                        let value = match value {
                            CellValue::Text(value) => value.clone(),
                            CellValue::Null => {
                                return Err("Первичный ключ строки содержит NULL".to_owned());
                            }
                            CellValue::Truncated(_) | CellValue::DecodeError(_) => {
                                return Err(
                                    "Нельзя определить первичный ключ этой строки".to_owned()
                                );
                            }
                        };
                        key_binds.push(value);
                        predicates.push(format!("{} = ?", quote_mysql_identifier(&key.name)));
                    }
                    if !mysql_lock_matches(
                        &mut transaction,
                        &table,
                        &edit.column,
                        &predicates.join(" AND "),
                        &key_binds,
                        original,
                    )
                    .await?
                    {
                        return Err("Конфликт: значение изменилось или строка удалена. Обновите таблицу; весь пакет изменений отменён.".to_owned());
                    }
                    let sql = format!(
                        "UPDATE {table} SET {} = {set_expression} WHERE {}",
                        quote_mysql_identifier(&edit.column),
                        predicates.join(" AND ")
                    );
                    let mut query = sqlx::query(&sql);
                    if let Some(value) = &edit.value {
                        query = query.bind(value);
                    }
                    for value in key_binds {
                        query = query.bind(value);
                    }
                    query
                        .execute(&mut *transaction)
                        .await
                        .map_err(|error| format_database_error(&error.to_string()))?;
                }
                transaction
                    .commit()
                    .await
                    .map_err(|error| format_database_error(&error.to_string()))?;
                Ok(edits.len())
            }
        }
    })
}

pub(crate) fn delete_table_row_on<'a>(
    connection: QueryConnection<'a>,
    object: &'a DbObject,
    primary_key_values: &'a [CellValue],
) -> futures_util::future::BoxFuture<'a, Result<u64, String>> {
    Box::pin(async move {
        if object.kind != DbObjectKind::Table {
            return Err("Удаление строк представлений не поддерживается".to_owned());
        }
        match connection {
            QueryConnection::PostgreSql(connection) => {
                let keys = table_columns_pg(&mut *connection, object)
                    .await?
                    .into_iter()
                    .filter(|column| column.primary_key)
                    .collect::<Vec<_>>();
                if keys.is_empty() || keys.len() != primary_key_values.len() {
                    return Err("Не удалось определить первичный ключ строки".to_owned());
                }
                let table = format!(
                    "{}.{}",
                    quote_pg_identifier(&object.schema),
                    quote_pg_identifier(&object.name)
                );
                let mut binds = Vec::with_capacity(keys.len());
                let mut predicates = Vec::with_capacity(keys.len());
                for (index, (key, value)) in keys.iter().zip(primary_key_values).enumerate() {
                    let value = match value {
                        CellValue::Text(value) => value.clone(),
                        _ => return Err("Первичный ключ строки нельзя прочитать".to_owned()),
                    };
                    binds.push(value);
                    let parameter = index + 1;
                    predicates.push(format!(
                        "{} = CAST(${parameter} AS {}.{})",
                        quote_pg_identifier(&key.name),
                        quote_pg_identifier(&key.udt_schema),
                        quote_pg_identifier(&key.udt_name)
                    ));
                }
                let sql = format!("DELETE FROM {table} WHERE {}", predicates.join(" AND "));
                let mut query = sqlx::query(&sql);
                for value in binds {
                    query = query.bind(value);
                }
                query
                    .execute(&mut *connection)
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(|error| format_database_error(&error.to_string()))
            }
            QueryConnection::MySql(connection) => {
                if !mysql_is_base_table(&mut *connection, object).await? {
                    return Err(
                        "Временная таблица перекрывает исходную: удаление отменено".to_owned()
                    );
                }

                let keys = table_columns_mysql(&mut *connection, object)
                    .await?
                    .into_iter()
                    .filter(|column| column.primary_key)
                    .collect::<Vec<_>>();
                if keys.is_empty() || keys.len() != primary_key_values.len() {
                    return Err("Не удалось определить первичный ключ строки".to_owned());
                }
                let table = format!(
                    "{}.{}",
                    quote_mysql_identifier(&object.schema),
                    quote_mysql_identifier(&object.name)
                );
                let sql = format!(
                    "DELETE FROM {table} WHERE {}",
                    keys.iter()
                        .map(|key| format!("{} = ?", quote_mysql_identifier(&key.name)))
                        .collect::<Vec<_>>()
                        .join(" AND ")
                );
                let mut query = sqlx::query(&sql);
                for value in primary_key_values {
                    let CellValue::Text(value) = value else {
                        return Err("Первичный ключ строки нельзя прочитать".to_owned());
                    };
                    query = query.bind(value);
                }
                query
                    .execute(&mut *connection)
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(|error| format_database_error(&error.to_string()))
            }
        }
    })
}

#[cfg(test)]
mod optimistic_mysql_tests {
    use super::*;
    #[test]
    #[ignore = "requires Compose MariaDB/MySQL; metadata locks only, no fixture writes"]
    fn batch_edit_guard_holds_metadata_lock_until_rollback() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for (port, key) in [
                (53306, "DBDECK_MARIADB_PASSWORD"),
                (53307, "DBDECK_MYSQL_PASSWORD"),
            ] {
                let options = sqlx::mysql::MySqlConnectOptions::new()
                    .host("127.0.0.1")
                    .port(port)
                    .database("dbdeck_test")
                    .username("dbdeck_test")
                    .password(&std::env::var(key).unwrap());
                let mut first = sqlx::MySqlConnection::connect_with(&options).await.unwrap();
                let mut second = sqlx::MySqlConnection::connect_with(&options).await.unwrap();
                let object = DbObject {
                    schema: "dbdeck_test".into(),
                    name: "sample_records".into(),
                    kind: DbObjectKind::Table,
                };
                let mut tx = first.begin().await.unwrap();
                mysql_validate_edit_table(&mut tx, &object).await.unwrap();
                sqlx::query("SET SESSION lock_wait_timeout=1")
                    .execute(&mut second)
                    .await
                    .unwrap();
                let error = sqlx::raw_sql("LOCK TABLES sample_records WRITE")
                    .execute(&mut second)
                    .await
                    .unwrap_err();
                assert_eq!(
                    error
                        .as_database_error()
                        .and_then(|e| e.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>())
                        .map(|e| e.number()),
                    Some(1205)
                );
                tx.rollback().await.unwrap();
                sqlx::raw_sql("LOCK TABLES sample_records WRITE")
                    .execute(&mut second)
                    .await
                    .unwrap();
                sqlx::raw_sql("UNLOCK TABLES")
                    .execute(&mut second)
                    .await
                    .unwrap();
                first.close().await.unwrap();
                second.close().await.unwrap();
            }
        });
    }
    #[test]
    #[ignore = "requires Compose MariaDB/MySQL; connection-local TEMP tables only"]
    fn stale_cell_checks_are_binary_and_null_aware() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for (port, key) in [(53306, "DBDECK_MARIADB_PASSWORD"), (53307, "DBDECK_MYSQL_PASSWORD")] {
                let options = sqlx::mysql::MySqlConnectOptions::new()
                    .host("127.0.0.1").port(port).database("dbdeck_test").username("dbdeck_test")
                    .password(&std::env::var(key).expect("test database password"));
                let mut connection = sqlx::MySqlConnection::connect_with(&options).await.unwrap();
                sqlx::raw_sql("CREATE TEMPORARY TABLE dbdeck_conflict_test (id INT PRIMARY KEY, value TEXT); INSERT INTO dbdeck_conflict_test VALUES (1, 'Original')")
                    .execute(&mut connection).await.unwrap();
                let keys = vec!["1".into()];
                let mut tx = connection.begin().await.unwrap();
                for (expected, matches) in [(Some("Original"), true), (Some("original"), false), (Some("Original "), false), (None, false)] {
                    assert_eq!(mysql_lock_matches(&mut tx, "dbdeck_conflict_test", "value", "id = ?", &keys, expected).await.unwrap(), matches);
                }
                sqlx::query("UPDATE dbdeck_conflict_test SET value = NULL WHERE id=1").execute(&mut *tx).await.unwrap();
                assert!(mysql_lock_matches(&mut tx, "dbdeck_conflict_test", "value", "id = ?", &keys, None).await.unwrap());
                assert!(!mysql_lock_matches(&mut tx, "dbdeck_conflict_test", "value", "id = ?", &keys, Some("")).await.unwrap());
                assert!(!mysql_lock_matches(&mut tx, "dbdeck_conflict_test", "value", "id = ?", &["2".into()], None).await.unwrap());
                tx.rollback().await.unwrap();
                connection.close().await.unwrap();
            }
        });
    }
}

#[cfg(test)]
mod preview_budget_tests {
    use super::*;
    #[test]
    fn preview_keeps_only_a_contiguous_bounded_prefix() {
        let mut data = TableData {
            columns: vec!["value".into()],
            rows: vec![vec![CellValue::Null]; MAX_BROWSE_ROWS + 1],
            primary_key: vec![],
            truncated: true,
            truncated_columns: false,
        };
        assert!(data.trim_preview(MAX_BROWSE_ROWS));
        assert_eq!(data.rows.len(), MAX_BROWSE_ROWS);
        data.rows = vec![
            vec![CellValue::Text("first".into())],
            vec![CellValue::Text(String::with_capacity(MAX_PREVIEW_BYTES))],
            vec![CellValue::Text("last".into())],
        ];
        assert!(data.trim_preview(MAX_BROWSE_ROWS));
        assert_eq!(data.rows.len(), 1);
        assert_eq!(data.rows[0][0], CellValue::Text("first".into()));
    }
}
