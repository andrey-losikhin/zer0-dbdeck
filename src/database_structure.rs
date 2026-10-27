//! Read-only schema inspection. Values are bound; identifiers are quoted separately.
use crate::database::{CellValue, DbObject, DbObjectKind, DbPool, TableData};
use sqlx::{Column, Row};

#[derive(Clone, Debug)]
pub struct ObjectStructure {
    pub columns: TableData,
    pub constraints: TableData,
    pub indexes: TableData,
    pub ddl: String,
    pub ddl_note: String,
    pub ddl_can_open: bool,
}
fn text(value: &CellValue) -> &str {
    if let CellValue::Text(value) = value {
        value
    } else {
        ""
    }
}
fn quoted(identifier: &str, mysql: bool) -> String {
    let quote = if mysql { '`' } else { '"' };
    format!(
        "{quote}{}{quote}",
        identifier.replace(quote, &format!("{quote}{quote}"))
    )
}
fn table_data(columns: Vec<String>, rows: Vec<Vec<CellValue>>) -> TableData {
    TableData {
        columns,
        rows,
        primary_key: Vec::new(),
        truncated: false,
        truncated_columns: false,
    }
}
impl DbPool {
    pub async fn inspect_object(&self, object: &DbObject) -> Result<ObjectStructure, String> {
        macro_rules! read {
            ($connection:expr, $sql:expr) => {{
                let fetched = sqlx::query($sql)
                    .bind(&object.schema)
                    .bind(&object.name)
                    .fetch_all(&mut *$connection)
                    .await
                    .map_err(|e| e.to_string())?;
                let columns = fetched
                    .first()
                    .map(|r| r.columns().iter().map(|c| c.name().to_owned()).collect())
                    .unwrap_or_default();
                // Metadata expressions are text; MySQL may label their UTF-8 bytes VARBINARY.
                // Decode still validates UTF-8, and any error rejects the whole definition.
                let rows = fetched
                    .into_iter()
                    .map(|r| {
                        (0..r.columns().len())
                            .map(|i| {
                                r.try_get_unchecked::<Option<String>, _>(i)
                                    .map(|v| v.map(CellValue::Text).unwrap_or(CellValue::Null))
                            })
                            .collect::<Result<Vec<_>, sqlx::Error>>()
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                table_data(columns, rows)
            }};
        }
        let qualified = format!(
            "{}.{}",
            quoted(&object.schema, matches!(self, Self::MySql(_))),
            quoted(&object.name, matches!(self, Self::MySql(_)))
        );
        match self {
            Self::PostgreSql(pool) => {
                let mut connection = pool.begin().await.map_err(|e| e.to_string())?;
                sqlx::query("SET TRANSACTION READ ONLY")
                    .execute(&mut *connection)
                    .await
                    .map_err(|e| e.to_string())?;
                sqlx::query("SET LOCAL search_path = pg_catalog")
                    .execute(&mut *connection)
                    .await
                    .map_err(|e| e.to_string())?;
                let temporary: bool = sqlx::query_scalar("SELECT c.relpersistence='t' FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relname=$2").bind(&object.schema).bind(&object.name).fetch_one(&mut *connection).await.map_err(|e| e.to_string())?;
                let columns = read!(
                    connection,
                    r#"SELECT a.attname::text AS "Столбец", pg_catalog.format_type(a.atttypid,a.atttypmod)::text AS "Тип", CASE WHEN a.attnotnull THEN 'NOT NULL' ELSE 'NULL' END AS "NULL", COALESCE(pg_catalog.pg_get_expr(d.adbin,d.adrelid),'')::text AS "По умолчанию / выражение", COALESCE(pg_catalog.col_description(a.attrelid,a.attnum),'')::text AS "Комментарий", a.attidentity::text AS "Identity", a.attgenerated::text AS "Generated" FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid=a.attrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE n.nspname=$1 AND c.relname=$2 AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum"#
                );
                if columns.rows.is_empty() {
                    return Err("Объект не найден, не имеет столбцов или недоступен".into());
                }
                let constraints = read!(
                    connection,
                    r#"SELECT c.conname::text AS "Имя", CASE c.contype WHEN 'p' THEN 'PRIMARY KEY' WHEN 'f' THEN 'FOREIGN KEY' WHEN 'u' THEN 'UNIQUE' WHEN 'c' THEN 'CHECK' WHEN 'x' THEN 'EXCLUDE' ELSE c.contype::text END AS "Тип", pg_catalog.pg_get_constraintdef(c.oid,true)::text AS "Определение" FROM pg_catalog.pg_constraint c JOIN pg_catalog.pg_class t ON t.oid=c.conrelid JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=$1 AND t.relname=$2 ORDER BY c.conname"#
                );
                let indexes = read!(
                    connection,
                    r#"SELECT ic.relname::text AS "Индекс", pg_catalog.pg_get_indexdef(i.indexrelid)::text AS "Определение", CASE WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_constraint c WHERE c.conindid=i.indexrelid AND c.contype IN ('p','u','x')) THEN 'Да' ELSE 'Нет' END AS "Ограничение" FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class t ON t.oid=i.indrelid JOIN pg_catalog.pg_class ic ON ic.oid=i.indexrelid JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=$1 AND t.relname=$2 ORDER BY ic.relname"#
                );
                let ddl = if object.kind == DbObjectKind::View {
                    let definition: String = sqlx::query_scalar("SELECT CASE WHEN c.relkind='m' THEN 'CREATE MATERIALIZED VIEW ' ELSE 'CREATE VIEW ' END || pg_catalog.quote_ident(n.nspname) || '.' || pg_catalog.quote_ident(c.relname) || ' AS ' || pg_catalog.pg_get_viewdef(c.oid,true) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relname=$2 AND c.relkind IN ('v','m')").bind(&object.schema).bind(&object.name).fetch_one(&mut *connection).await.map_err(|e| e.to_string())?;
                    definition
                } else {
                    postgres_ddl(&qualified, &columns, &constraints, &indexes)
                };
                connection.rollback().await.map_err(|e| e.to_string())?;
                let mut ddl_note = String::from(
                    "Базовый DDL PostgreSQL: столбцы, defaults, identity/generated, ограничения и индексы. Владельцы, права, параметры хранения, секционирование и отдельные sequence/domain/type не включены; для точной копии схемы нужен pg_dump.",
                );
                let ddl = if temporary {
                    ddl_note = "Временная таблица принадлежит исходной SQL-сессии. DDL показан справочно: pg_temp_N и ON COMMIT не переносятся в новую вкладку. Открытие для выполнения отключено.".into();
                    format!("-- {ddl_note}\n{ddl}")
                } else {
                    ddl
                };
                Ok(ObjectStructure {
                    columns,
                    constraints,
                    indexes,
                    ddl,
                    ddl_note,
                    ddl_can_open: !temporary,
                })
            }
            Self::MySql(pool) => {
                let mut connection = pool.acquire().await.map_err(|e| e.to_string())?;
                let columns = read!(
                    connection,
                    "SELECT COLUMN_NAME AS `Столбец`, COLUMN_TYPE AS `Тип`, IS_NULLABLE AS `NULL`, COLUMN_DEFAULT AS `По умолчанию`, EXTRA AS `Дополнительно`, COLUMN_COMMENT AS `Комментарий`, GENERATION_EXPRESSION AS `Выражение` FROM information_schema.COLUMNS WHERE TABLE_SCHEMA=? AND TABLE_NAME=? ORDER BY ORDINAL_POSITION"
                );
                if columns.rows.is_empty() {
                    return Err("Объект не найден или недоступен".into());
                }
                let constraints = read!(
                    connection,
                    "SELECT tc.CONSTRAINT_NAME AS `Имя`, tc.CONSTRAINT_TYPE AS `Тип`, k.COLUMN_NAME AS `Столбец`, k.REFERENCED_TABLE_SCHEMA AS `Схема ссылки`, k.REFERENCED_TABLE_NAME AS `Таблица ссылки`, k.REFERENCED_COLUMN_NAME AS `Столбец ссылки` FROM information_schema.TABLE_CONSTRAINTS tc LEFT JOIN information_schema.KEY_COLUMN_USAGE k ON k.CONSTRAINT_SCHEMA=tc.CONSTRAINT_SCHEMA AND k.TABLE_NAME=tc.TABLE_NAME AND k.CONSTRAINT_NAME=tc.CONSTRAINT_NAME WHERE tc.TABLE_SCHEMA=? AND tc.TABLE_NAME=? ORDER BY tc.CONSTRAINT_NAME,k.ORDINAL_POSITION"
                );
                let indexes = read!(
                    connection,
                    "SELECT INDEX_NAME AS `Индекс`, CAST(SEQ_IN_INDEX AS CHAR) AS `Позиция`, COLUMN_NAME AS `Столбец`, CASE WHEN NON_UNIQUE=0 THEN 'Да' ELSE 'Нет' END AS `Уникальный`, INDEX_TYPE AS `Тип`, CAST(SUB_PART AS CHAR) AS `Префикс` FROM information_schema.STATISTICS WHERE TABLE_SCHEMA=? AND TABLE_NAME=? ORDER BY INDEX_NAME,SEQ_IN_INDEX"
                );
                let command = format!(
                    "SHOW CREATE {} {qualified}",
                    if object.kind == DbObjectKind::View {
                        "VIEW"
                    } else {
                        "TABLE"
                    }
                );
                let row = sqlx::query(&command)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(|e| e.to_string())?;
                let ddl: String = row.try_get(1).map_err(|e| e.to_string())?;
                Ok(ObjectStructure { columns, constraints, indexes, ddl: format!("{ddl};"), ddl_note: "DDL получен от сервера через SHOW CREATE. В MySQL изменение структуры может выполнять неявный commit.".into(), ddl_can_open: true })
            }
        }
    }
}
fn postgres_ddl(
    qualified: &str,
    columns: &TableData,
    constraints: &TableData,
    indexes: &TableData,
) -> String {
    let mut definitions = Vec::new();
    for row in &columns.rows {
        let mut definition = format!("    {} {}", quoted(text(&row[0]), false), text(&row[1]));
        match text(&row[5]) {
            "a" => definition.push_str(" GENERATED ALWAYS AS IDENTITY"),
            "d" => definition.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
            _ => {
                if !text(&row[6]).is_empty() {
                    definition.push_str(&format!(
                        " GENERATED ALWAYS AS ({}) {}",
                        text(&row[3]),
                        if text(&row[6]) == "v" {
                            "VIRTUAL"
                        } else {
                            "STORED"
                        }
                    ));
                } else if !text(&row[3]).is_empty() {
                    definition.push_str(&format!(" DEFAULT {}", text(&row[3])));
                }
            }
        }
        if text(&row[2]) == "NOT NULL" {
            definition.push_str(" NOT NULL");
        }
        definitions.push(definition);
    }
    for row in &constraints.rows {
        if text(&row[1]) == "NOT NULL" || text(&row[1]) == "n" {
            continue;
        }
        definitions.push(format!(
            "    CONSTRAINT {} {}",
            quoted(text(&row[0]), false),
            text(&row[2])
        ));
    }
    let mut ddl = format!(
        "CREATE TABLE {qualified} (\n{}\n);",
        definitions.join(",\n")
    );
    for row in &indexes.rows {
        if text(&row[2]) != "Да" {
            ddl.push_str(&format!("\n{};", text(&row[1])));
        }
    }
    for row in &columns.rows {
        if !text(&row[4]).is_empty() {
            ddl.push_str(&format!(
                "\nCOMMENT ON COLUMN {qualified}.{} IS '{}';",
                quoted(text(&row[0]), false),
                text(&row[4]).replace('\'', "''")
            ));
        }
    }
    ddl
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_and_comments_are_quoted_and_constraint_indexes_are_not_duplicated() {
        assert_eq!(quoted("x\"y", false), "\"x\"\"y\"");
        assert_eq!(quoted("x`y", true), "`x``y`");
        let columns = table_data(
            vec![],
            vec![
                vec!["id", "integer", "NOT NULL", "", "O'Brien", "a", ""]
                    .into_iter()
                    .map(|s| CellValue::Text(s.into()))
                    .collect(),
            ],
        );
        let constraints = table_data(
            vec![],
            vec![
                vec!["pk", "PRIMARY KEY", "PRIMARY KEY (id)"]
                    .into_iter()
                    .map(|s| CellValue::Text(s.into()))
                    .collect(),
            ],
        );
        let indexes = table_data(
            vec![],
            vec![
                vec!["pk", "CREATE UNIQUE INDEX pk ON t(id)", "Да"]
                    .into_iter()
                    .map(|s| CellValue::Text(s.into()))
                    .collect(),
            ],
        );
        let ddl = postgres_ddl("\"public\".\"t\"", &columns, &constraints, &indexes);
        assert!(ddl.contains("GENERATED ALWAYS AS IDENTITY NOT NULL"));
        assert!(ddl.contains("PRIMARY KEY (id)"));
        assert!(!ddl.contains("CREATE UNIQUE INDEX"));
        assert!(ddl.contains("'O''Brien'"));
    }
}
