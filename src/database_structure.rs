//! Read-only schema inspection. Values are bound; identifiers are quoted separately.
use crate::database::{CellValue, DbObject, DbObjectKind, DbPool, TableData};
use sqlx::{Column, Row};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Debug)]
pub struct ObjectStructure {
    pub columns: TableData,
    pub constraints: TableData,
    pub indexes: TableData,
    pub ddl: String,
    pub ddl_note: String,
    pub ddl_can_open: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StructureDifference {
    pub category: &'static str,
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

type MetadataRows<'a> = BTreeMap<&'a str, Vec<&'a [CellValue]>>;

fn metadata_by_name(data: &TableData) -> Result<MetadataRows<'_>, String> {
    let mut by_name = BTreeMap::new();
    for row in &data.rows {
        let [CellValue::Text(name), fields @ ..] = row.as_slice() else {
            return Err("Каталог вернул элемент без имени".into());
        };
        by_name
            .entry(name.as_str())
            .or_insert_with(Vec::new)
            .push(fields);
    }
    Ok(by_name)
}

fn describe_metadata(columns: &[String], rows: &[&[CellValue]]) -> String {
    rows.iter()
        .map(|row| {
            columns
                .iter()
                .skip(1)
                .zip(*row)
                .map(|(column, value)| match value {
                    CellValue::Null => format!("{column}: NULL"),
                    _ => format!("{column}: {:?}", text(value)),
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn structure_differences(
    before: &ObjectStructure,
    after: &ObjectStructure,
) -> Result<Vec<StructureDifference>, String> {
    let mut differences = Vec::new();
    for (category, before, after) in [
        ("Столбец", &before.columns, &after.columns),
        ("Ограничение", &before.constraints, &after.constraints),
        ("Индекс", &before.indexes, &after.indexes),
    ] {
        let left =
            metadata_by_name(before).map_err(|error| format!("Исходная таблица: {error}"))?;
        let right =
            metadata_by_name(after).map_err(|error| format!("Таблица для сравнения: {error}"))?;
        let mut names = left.keys().chain(right.keys()).copied().collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        for name in names {
            let (left, right) = (left.get(name), right.get(name));
            if left != right {
                differences.push(StructureDifference {
                    category,
                    name: name.into(),
                    before: left.map(|rows| describe_metadata(&before.columns, rows)),
                    after: right.map(|rows| describe_metadata(&after.columns, rows)),
                });
            }
        }
    }
    Ok(differences)
}

fn comparison_metadata(
    columns: TableData,
    constraints: TableData,
    indexes: TableData,
) -> ObjectStructure {
    ObjectStructure {
        columns,
        constraints,
        indexes,
        ddl: String::new(),
        ddl_note: String::new(),
        ddl_can_open: false,
    }
}

const PG_COMPARE_COLUMNS: &str = r#"SELECT
    a.attname::text AS "Столбец",
    (row_number() OVER (ORDER BY a.attnum))::text AS "Позиция",
    pg_catalog.format_type(a.atttypid,a.atttypmod)::text AS "Тип",
    a.attnotnull::text AS "NOT NULL",
    pg_catalog.pg_get_expr(d.adbin,d.adrelid) AS "По умолчанию / выражение",
    pg_catalog.col_description(a.attrelid,a.attnum) AS "Комментарий",
    a.attidentity::text AS "Identity",
    a.attgenerated::text AS "Generated",
    CASE WHEN coll.oid IS NOT NULL THEN pg_catalog.format('%I.%I', cn.nspname, coll.collname) END AS "Сортировка"
    FROM pg_catalog.pg_attribute a
    JOIN pg_catalog.pg_class t ON t.oid=a.attrelid
    JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace
    LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
    LEFT JOIN pg_catalog.pg_collation coll ON coll.oid=a.attcollation
    LEFT JOIN pg_catalog.pg_namespace cn ON cn.oid=coll.collnamespace
    WHERE n.nspname=$1 AND t.relname=$2 AND a.attnum>0 AND NOT a.attisdropped
    ORDER BY a.attnum"#;

const PG_COMPARE_CONSTRAINTS: &str = r#"SELECT c.conname::text AS "Имя",
    pg_catalog.pg_get_constraintdef(c.oid,false) AS "Определение",
    c.convalidated::text AS "Проверены существующие строки",
    COALESCE(pg_catalog.to_jsonb(c)->>'conenforced','true') AS "Проверяется"
    FROM pg_catalog.pg_constraint c
    JOIN pg_catalog.pg_class t ON t.oid=c.conrelid
    JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace
    WHERE n.nspname=$1 AND t.relname=$2
    ORDER BY c.conname,c.contype,pg_catalog.pg_get_constraintdef(c.oid,false)"#;

// The per-key deparser omits the owning relation and options.
// Compare index/key options and the INCLUDE boundary separately.
const PG_COMPARE_INDEXES: &str = r#"SELECT ic.relname::text AS "Индекс",
    am.amname::text AS "Метод",
    ARRAY(SELECT pg_catalog.pg_get_indexdef(i.indexrelid, k, false)
        FROM pg_catalog.generate_series(1,i.indnatts) k ORDER BY k)::text AS "Столбцы и выражения",
    i.indnkeyatts::text AS "Число ключевых столбцов",
    ARRAY(SELECT pg_catalog.format('%I.%I', ns.nspname, op.opcname)
        FROM pg_catalog.unnest(i.indclass) WITH ORDINALITY AS k(oid, pos)
        JOIN pg_catalog.pg_opclass op ON op.oid=k.oid
        JOIN pg_catalog.pg_namespace ns ON ns.oid=op.opcnamespace ORDER BY k.pos)::text AS "Классы операторов",
    ARRAY(SELECT ARRAY(SELECT option FROM pg_catalog.unnest(a.attoptions) option ORDER BY option)::text
        FROM pg_catalog.pg_attribute a
        WHERE a.attrelid=i.indexrelid AND a.attnum BETWEEN 1 AND i.indnkeyatts
        ORDER BY a.attnum)::text AS "Параметры классов операторов",
    ARRAY(SELECT CASE WHEN co.oid IS NOT NULL THEN pg_catalog.format('%I.%I',ns.nspname, co.collname) END
        FROM pg_catalog.unnest(i.indcollation) WITH ORDINALITY AS k(oid, pos)
        LEFT JOIN pg_catalog.pg_collation co ON co.oid=k.oid
        LEFT JOIN pg_catalog.pg_namespace ns ON ns.oid=co.collnamespace ORDER BY k.pos)::text AS "Правила сортировки",
    i.indoption::text AS "Параметры порядка",
    i.indisunique::text AS "Уникальный",
    COALESCE(pg_catalog.to_jsonb(i)->>'indnullsnotdistinct','false') AS "NULLS NOT DISTINCT",
    ARRAY(SELECT option FROM pg_catalog.unnest(ic.reloptions) option ORDER BY option)::text AS "Параметры хранения",
    pg_catalog.pg_get_expr(i.indpred,i.indrelid) AS "Условие"
    FROM pg_catalog.pg_index i
    JOIN pg_catalog.pg_class t ON t.oid=i.indrelid
    JOIN pg_catalog.pg_class ic ON ic.oid=i.indexrelid
    JOIN pg_catalog.pg_am am ON am.oid=ic.relam
    JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace
    WHERE n.nspname=$1 AND t.relname=$2 ORDER BY ic.relname"#;

const MYSQL_COMPARE_COLUMNS: &str = r#"SELECT COLUMN_NAME AS `Столбец`,
    CAST(ORDINAL_POSITION AS CHAR) AS `Позиция`, COLUMN_TYPE AS `Тип`, IS_NULLABLE AS `NULL`,
    COLUMN_DEFAULT AS `По умолчанию`, EXTRA AS `Дополнительно`, COLUMN_COMMENT AS `Комментарий`,
    GENERATION_EXPRESSION AS `Выражение`, COLLATION_NAME AS `Сортировка`
    FROM information_schema.COLUMNS WHERE TABLE_SCHEMA=? AND TABLE_NAME=?
        AND CAST(TABLE_SCHEMA AS BINARY)=CAST(? AS BINARY)
        AND CAST(TABLE_NAME AS BINARY)=CAST(? AS BINARY)
    ORDER BY ORDINAL_POSITION"#;
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
        cell_detail_key_columns: Vec::new(),
        truncated: false,
        truncated_columns: false,
    }
}
impl DbPool {
    pub async fn inspect_object(&self, object: &DbObject) -> Result<ObjectStructure, String> {
        self.read_structure(object, false).await
    }

    pub async fn compare_table_structure(
        &self,
        source: &DbObject,
        target_pool: &Self,
        target: &DbObject,
    ) -> Result<Vec<StructureDifference>, String> {
        async fn read(
            pool: &DbPool,
            table: &DbObject,
            side: &str,
        ) -> Result<ObjectStructure, String> {
            let result =
                tokio::time::timeout(Duration::from_secs(15), pool.read_structure(table, true))
                    .await
                    .unwrap_or_else(|_| Err("истекло время ожидания".into()));
            result.map_err(|error| format!("{side} ({}.{}): {error}", table.schema, table.name))
        }
        let (source, target) = futures_util::future::try_join(
            read(self, source, "Исходная таблица"),
            read(target_pool, target, "Таблица для сравнения"),
        )
        .await?;
        structure_differences(&source, &target)
    }

    async fn read_structure(
        &self,
        object: &DbObject,
        comparison: bool,
    ) -> Result<ObjectStructure, String> {
        macro_rules! read {
            ($connection:expr, $sql:expr $(, $extra:expr)*) => {{
                let fetched = sqlx::query($sql)
                    .bind(&object.schema)
                    .bind(&object.name)
                    $(.bind($extra))*
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
                sqlx::query(if comparison {
                    "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY"
                } else {
                    "SET TRANSACTION READ ONLY"
                })
                .execute(&mut *connection)
                .await
                .map_err(|e| e.to_string())?;
                sqlx::query("SET LOCAL search_path = pg_catalog")
                    .execute(&mut *connection)
                    .await
                    .map_err(|e| e.to_string())?;
                let (temporary, is_table): (bool, bool) = sqlx::query_as("SELECT c.relpersistence='t', c.relkind IN ('r','p') FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relname=$2").bind(&object.schema).bind(&object.name).fetch_one(&mut *connection).await.map_err(|e| e.to_string())?;
                if comparison && (!is_table || temporary) {
                    return Err("Объект должен быть постоянной таблицей".into());
                }
                let columns = read!(
                    connection,
                    if comparison {
                        PG_COMPARE_COLUMNS
                    } else {
                        r#"SELECT a.attname::text AS "Столбец", pg_catalog.format_type(a.atttypid,a.atttypmod)::text AS "Тип", CASE WHEN a.attnotnull THEN 'NOT NULL' ELSE 'NULL' END AS "NULL", COALESCE(pg_catalog.pg_get_expr(d.adbin,d.adrelid),'')::text AS "По умолчанию / выражение", COALESCE(pg_catalog.col_description(a.attrelid,a.attnum),'')::text AS "Комментарий", a.attidentity::text AS "Identity", a.attgenerated::text AS "Generated" FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid=a.attrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE n.nspname=$1 AND c.relname=$2 AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum"#
                    }
                );
                if columns.rows.is_empty() && !is_table {
                    return Err("Объект не найден, не имеет столбцов или недоступен".into());
                }
                let constraints = read!(
                    connection,
                    if comparison {
                        PG_COMPARE_CONSTRAINTS
                    } else {
                        r#"SELECT c.conname::text AS "Имя", CASE c.contype WHEN 'p' THEN 'PRIMARY KEY' WHEN 'f' THEN 'FOREIGN KEY' WHEN 'u' THEN 'UNIQUE' WHEN 'c' THEN 'CHECK' WHEN 'x' THEN 'EXCLUDE' ELSE c.contype::text END AS "Тип", pg_catalog.pg_get_constraintdef(c.oid,true)::text AS "Определение" FROM pg_catalog.pg_constraint c JOIN pg_catalog.pg_class t ON t.oid=c.conrelid JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=$1 AND t.relname=$2 ORDER BY c.conname"#
                    }
                );
                let indexes = read!(
                    connection,
                    if comparison {
                        PG_COMPARE_INDEXES
                    } else {
                        r#"SELECT ic.relname::text AS "Индекс", pg_catalog.pg_get_indexdef(i.indexrelid)::text AS "Определение", CASE WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_constraint c WHERE c.conindid=i.indexrelid AND c.contype IN ('p','u','x')) THEN 'Да' ELSE 'Нет' END AS "Ограничение" FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class t ON t.oid=i.indrelid JOIN pg_catalog.pg_class ic ON ic.oid=i.indexrelid JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=$1 AND t.relname=$2 ORDER BY ic.relname"#
                    }
                );
                if comparison {
                    connection.rollback().await.map_err(|e| e.to_string())?;
                    return Ok(comparison_metadata(columns, constraints, indexes));
                }
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
                if comparison {
                    let mut connection = pool
                        .begin_with("START TRANSACTION READ ONLY")
                        .await
                        .map_err(|e| e.to_string())?;
                    // Hold the table metadata lock while reading the separate catalog views.
                    sqlx::query(&format!("SELECT 1 FROM {qualified} LIMIT 0"))
                        .execute(&mut *connection)
                        .await
                        .map_err(|e| e.to_string())?;
                    let is_table: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM information_schema.TABLES
                         WHERE TABLE_SCHEMA=? AND TABLE_NAME=? AND TABLE_TYPE='BASE TABLE'
                           AND CAST(TABLE_SCHEMA AS BINARY)=CAST(? AS BINARY)
                           AND CAST(TABLE_NAME AS BINARY)=CAST(? AS BINARY))",
                    )
                    .bind(&object.schema)
                    .bind(&object.name)
                    .bind(&object.schema)
                    .bind(&object.name)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(|e| e.to_string())?;
                    if !is_table {
                        return Err("Таблица не найдена или недоступна".into());
                    }
                    let features = sqlx::query_as::<_, (String, String)>(
                        "SELECT CAST(TABLE_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR) FROM information_schema.COLUMNS WHERE UPPER(TABLE_SCHEMA)='INFORMATION_SCHEMA' AND UPPER(TABLE_NAME) IN ('CHECK_CONSTRAINTS','TABLE_CONSTRAINTS','STATISTICS')",
                    ).fetch_all(&mut *connection).await.map_err(|e| e.to_string())?;
                    let has = |table: &str, column: &str| {
                        features.iter().any(|(t, c)| {
                            t.eq_ignore_ascii_case(table) && c.eq_ignore_ascii_case(column)
                        })
                    };
                    let checks = has("CHECK_CONSTRAINTS", "CHECK_CLAUSE");
                    let check_join = if !checks {
                        ""
                    } else if has("CHECK_CONSTRAINTS", "TABLE_NAME") {
                        "LEFT JOIN information_schema.CHECK_CONSTRAINTS ch ON tc.CONSTRAINT_TYPE='CHECK'
                          AND ch.CONSTRAINT_SCHEMA=tc.CONSTRAINT_SCHEMA AND ch.TABLE_NAME=tc.TABLE_NAME
                          AND CAST(ch.CONSTRAINT_SCHEMA AS BINARY)=CAST(tc.CONSTRAINT_SCHEMA AS BINARY)
                          AND CAST(ch.TABLE_NAME AS BINARY)=CAST(tc.TABLE_NAME AS BINARY)
                          AND CAST(ch.CONSTRAINT_NAME AS BINARY)=CAST(tc.CONSTRAINT_NAME AS BINARY)"
                    } else {
                        "LEFT JOIN information_schema.CHECK_CONSTRAINTS ch ON tc.CONSTRAINT_TYPE='CHECK'
                          AND ch.CONSTRAINT_SCHEMA=tc.CONSTRAINT_SCHEMA
                          AND CAST(ch.CONSTRAINT_SCHEMA AS BINARY)=CAST(tc.CONSTRAINT_SCHEMA AS BINARY)
                          AND CAST(ch.CONSTRAINT_NAME AS BINARY)=CAST(tc.CONSTRAINT_NAME AS BINARY)"
                    };
                    let check_clause = if checks { "ch.CHECK_CLAUSE" } else { "NULL" };
                    let enforced = if has("TABLE_CONSTRAINTS", "ENFORCED") {
                        "tc.ENFORCED"
                    } else {
                        "NULL"
                    };
                    let expression = if has("STATISTICS", "EXPRESSION") {
                        "EXPRESSION"
                    } else {
                        "NULL"
                    };
                    let visible = if has("STATISTICS", "IS_VISIBLE") {
                        "IS_VISIBLE"
                    } else if has("STATISTICS", "IGNORED") {
                        "CASE IGNORED WHEN 'YES' THEN 'NO' ELSE 'YES' END"
                    } else {
                        "'YES'"
                    };
                    let columns = read!(
                        connection,
                        MYSQL_COMPARE_COLUMNS,
                        &object.schema,
                        &object.name
                    );
                    if columns.rows.is_empty() {
                        return Err("Таблица не найдена или недоступна".into());
                    }
                    let constraints_sql = format!(
                        r#"SELECT tc.CONSTRAINT_NAME AS `Имя`, tc.CONSTRAINT_TYPE AS `Тип`,
                        k.COLUMN_NAME AS `Столбец`, k.REFERENCED_TABLE_SCHEMA AS `Схема ссылки`,
                        k.REFERENCED_TABLE_NAME AS `Таблица ссылки`, k.REFERENCED_COLUMN_NAME AS `Столбец ссылки`,
                        r.UPDATE_RULE AS `ON UPDATE`, r.DELETE_RULE AS `ON DELETE`,
                        {check_clause} AS `CHECK`, {enforced} AS `Проверяется`
                        FROM information_schema.TABLE_CONSTRAINTS tc
                        LEFT JOIN information_schema.KEY_COLUMN_USAGE k ON tc.CONSTRAINT_TYPE<>'CHECK'
                            AND k.CONSTRAINT_SCHEMA=tc.CONSTRAINT_SCHEMA AND k.TABLE_NAME=tc.TABLE_NAME
                            AND CAST(k.CONSTRAINT_SCHEMA AS BINARY)=CAST(tc.CONSTRAINT_SCHEMA AS BINARY)
                            AND CAST(k.TABLE_NAME AS BINARY)=CAST(tc.TABLE_NAME AS BINARY)
                            AND CAST(k.CONSTRAINT_NAME AS BINARY)=CAST(tc.CONSTRAINT_NAME AS BINARY)
                            AND (k.REFERENCED_TABLE_NAME IS NOT NULL)=(tc.CONSTRAINT_TYPE='FOREIGN KEY')
                        LEFT JOIN information_schema.REFERENTIAL_CONSTRAINTS r ON tc.CONSTRAINT_TYPE='FOREIGN KEY'
                            AND r.CONSTRAINT_SCHEMA=tc.CONSTRAINT_SCHEMA AND r.TABLE_NAME=tc.TABLE_NAME
                            AND CAST(r.CONSTRAINT_SCHEMA AS BINARY)=CAST(tc.CONSTRAINT_SCHEMA AS BINARY)
                            AND CAST(r.TABLE_NAME AS BINARY)=CAST(tc.TABLE_NAME AS BINARY)
                            AND CAST(r.CONSTRAINT_NAME AS BINARY)=CAST(tc.CONSTRAINT_NAME AS BINARY)
                        {check_join}
                        WHERE tc.TABLE_SCHEMA=? AND tc.TABLE_NAME=?
                            AND CAST(tc.TABLE_SCHEMA AS BINARY)=CAST(? AS BINARY)
                            AND CAST(tc.TABLE_NAME AS BINARY)=CAST(? AS BINARY)
                        ORDER BY tc.CONSTRAINT_NAME, tc.CONSTRAINT_TYPE, k.ORDINAL_POSITION"#
                    );
                    let constraints =
                        read!(connection, &constraints_sql, &object.schema, &object.name);
                    let indexes_sql = format!(
                        r#"SELECT INDEX_NAME AS `Индекс`,
                        CAST(SEQ_IN_INDEX AS CHAR) AS `Позиция`, COLUMN_NAME AS `Столбец`,
                        {expression} AS `Выражение`, COLLATION AS `Порядок`,
                        CASE WHEN NON_UNIQUE=0 THEN 'Да' ELSE 'Нет' END AS `Уникальный`,
                        INDEX_TYPE AS `Тип`, CAST(SUB_PART AS CHAR) AS `Префикс`, {visible} AS `Видимый`,
                        INDEX_COMMENT AS `Комментарий`
                        FROM information_schema.STATISTICS WHERE TABLE_SCHEMA=? AND TABLE_NAME=?
                            AND CAST(TABLE_SCHEMA AS BINARY)=CAST(? AS BINARY)
                            AND CAST(TABLE_NAME AS BINARY)=CAST(? AS BINARY)
                        ORDER BY INDEX_NAME, SEQ_IN_INDEX"#
                    );
                    let indexes = read!(connection, &indexes_sql, &object.schema, &object.name);
                    connection.rollback().await.map_err(|e| e.to_string())?;
                    return Ok(comparison_metadata(columns, constraints, indexes));
                }
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
    fn comparison_preserves_composite_order_and_null_values() {
        let columns = || table_data(vec![], vec![]);
        let indexes = |fields: Vec<CellValue>| {
            table_data(
                vec!["Индекс".into(), "Ключ".into()],
                fields
                    .into_iter()
                    .map(|field| vec![CellValue::Text("ix".into()), field])
                    .collect(),
            )
        };
        let before = comparison_metadata(
            columns(),
            columns(),
            indexes(vec![CellValue::Null, CellValue::Text("NULL".into())]),
        );
        let after = comparison_metadata(
            columns(),
            columns(),
            indexes(vec![CellValue::Text("NULL".into()), CellValue::Null]),
        );
        let changes = structure_differences(&before, &after).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].before.as_deref(),
            Some("Ключ: NULL\n\nКлюч: \"NULL\"")
        );
        assert_eq!(
            changes[0].after.as_deref(),
            Some("Ключ: \"NULL\"\n\nКлюч: NULL")
        );
    }

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
