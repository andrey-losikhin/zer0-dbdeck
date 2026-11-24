//! Catalog-backed navigation over ordinary foreign keys.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PgComparison {
    operator_schema: String,
    operator: String,
    parent_type: Option<String>,
    child_type: Option<String>,
    collation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ColumnPair {
    child: String,
    parent: String,
    pg: Option<PgComparison>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ForeignKey {
    name: String,
    child: DbObject,
    parent: DbObject,
    columns: Vec<ColumnPair>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKeyLink {
    source: DbObject,
    pg_identity: Option<PgLinkIdentity>,
    fk: ForeignKey,
    outbound: bool,
}

impl ForeignKeyLink {
    pub fn name(&self) -> &str {
        &self.fk.name
    }

    pub fn outbound(&self) -> bool {
        self.outbound
    }

    pub fn target(&self) -> &DbObject {
        if self.outbound {
            &self.fk.parent
        } else {
            &self.fk.child
        }
    }

    pub fn columns(&self) -> impl Iterator<Item = (&str, &str)> {
        self.fk.columns.iter().map(|pair| {
            if self.outbound {
                (pair.child.as_str(), pair.parent.as_str())
            } else {
                (pair.parent.as_str(), pair.child.as_str())
            }
        })
    }
}

type ColumnNames = (String, String, String, String, String, String, String);

// PostgreSQL object identity is stable across the metadata and result queries.
type PgLinkIdentity = (u32, u32); // source relation OID and constraint OID

fn links(
    source: &DbObject,
    parts: impl IntoIterator<
        Item = (
            ColumnNames,
            Option<PgComparison>,
            bool,
            bool,
            Option<PgLinkIdentity>,
        ),
    >,
) -> Vec<ForeignKeyLink> {
    let mut keys: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for (
        (name, child_schema, child_table, parent_schema, parent_table, child, parent),
        pg,
        outbound,
        inbound,
        pg_identity,
    ) in parts
    {
        keys.entry((
            name,
            child_schema,
            child_table,
            parent_schema,
            parent_table,
            outbound,
            inbound,
            pg_identity,
        ))
        .or_default()
        .push(ColumnPair { child, parent, pg });
    }
    let mut links = Vec::new();
    for (
        (
            name,
            child_schema,
            child_table,
            parent_schema,
            parent_table,
            outbound,
            inbound,
            pg_identity,
        ),
        columns,
    ) in keys
    {
        let fk = ForeignKey {
            name,
            child: DbObject {
                schema: child_schema,
                name: child_table,
                kind: DbObjectKind::Table,
            },
            parent: DbObject {
                schema: parent_schema,
                name: parent_table,
                kind: DbObjectKind::Table,
            },
            columns,
        };
        if outbound {
            links.push(ForeignKeyLink {
                source: source.clone(),
                pg_identity,
                fk: fk.clone(),
                outbound: true,
            });
        }
        if inbound {
            links.push(ForeignKeyLink {
                source: source.clone(),
                pg_identity,
                fk,
                outbound: false,
            });
        }
    }
    links
}

async fn postgres_links(
    connection: &mut sqlx::PgConnection,
    source: &DbObject,
) -> Result<Vec<ForeignKeyLink>, DbError> {
    let parts = sqlx::query(
        "WITH RECURSIVE source(oid) AS (
             SELECT c.oid FROM pg_catalog.pg_class c
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname = $2
         ), endpoints(oid) AS (
             SELECT oid FROM source
             UNION ALL
             SELECT i.inhparent FROM endpoints e
             JOIN pg_catalog.pg_class part ON part.oid = e.oid AND part.relispartition
             JOIN pg_catalog.pg_inherits i ON i.inhrelid = part.oid
         )
         SELECT fk.conname::text, cn.nspname::text, child.relname::text,
                pn.nspname::text, parent.relname::text, ca.attname::text, pa.attname::text,
                ons.nspname::text, op.oprname::text,
                CASE WHEN pt.typtype <> 'p' THEN pg_catalog.quote_ident(ptn.nspname) || '.' || pg_catalog.quote_ident(pt.typname) END,
                CASE WHEN ct.typtype <> 'p' THEN pg_catalog.quote_ident(ctn.nspname) || '.' || pg_catalog.quote_ident(ct.typname) END,
                CASE WHEN coll.oid IS NOT NULL THEN pg_catalog.quote_ident(cns.nspname) || '.' || pg_catalog.quote_ident(coll.collname) END,
                fk.conrelid IN (SELECT oid FROM endpoints),
                fk.confrelid IN (SELECT oid FROM endpoints),
                (SELECT oid FROM source), fk.oid
         FROM pg_catalog.pg_constraint fk
         JOIN pg_catalog.pg_class child ON child.oid = fk.conrelid
         JOIN pg_catalog.pg_namespace cn ON cn.oid = child.relnamespace
         JOIN pg_catalog.pg_class parent ON parent.oid = fk.confrelid
         JOIN pg_catalog.pg_namespace pn ON pn.oid = parent.relnamespace
         CROSS JOIN LATERAL ROWS FROM (pg_catalog.unnest(fk.conkey), pg_catalog.unnest(fk.confkey),
                                      pg_catalog.unnest(fk.conpfeqop))
             WITH ORDINALITY AS pair(child_num, parent_num, eq_op, position)
         JOIN pg_catalog.pg_attribute ca ON ca.attrelid = child.oid AND ca.attnum = pair.child_num
         JOIN pg_catalog.pg_attribute pa ON pa.attrelid = parent.oid AND pa.attnum = pair.parent_num
         JOIN pg_catalog.pg_operator op ON op.oid = pair.eq_op
         JOIN pg_catalog.pg_namespace ons ON ons.oid = op.oprnamespace
         JOIN pg_catalog.pg_type pt ON pt.oid = op.oprleft
         JOIN pg_catalog.pg_namespace ptn ON ptn.oid = pt.typnamespace
         JOIN pg_catalog.pg_type ct ON ct.oid = op.oprright
         JOIN pg_catalog.pg_namespace ctn ON ctn.oid = ct.typnamespace
         LEFT JOIN pg_catalog.pg_collation coll ON coll.oid = pa.attcollation
         LEFT JOIN pg_catalog.pg_namespace cns ON cns.oid = coll.collnamespace
         WHERE fk.contype = 'f' AND fk.conparentid = 0
           AND COALESCE(pg_catalog.to_jsonb(fk)->>'conperiod', 'false') = 'false'
           AND (fk.conrelid IN (SELECT oid FROM endpoints) OR fk.confrelid IN (SELECT oid FROM endpoints))
         ORDER BY fk.oid, pair.position",
    )
    .bind(&source.schema)
    .bind(&source.name)
    .fetch_all(connection)
    .await
    .map_err(|error| db_error(&error))?
    .into_iter()
    .map(|row| {
        let names = (
            row.try_get(0)?, row.try_get(1)?, row.try_get(2)?, row.try_get(3)?,
            row.try_get(4)?, row.try_get(5)?, row.try_get(6)?,
        );
        let comparison = PgComparison {
            operator_schema: row.try_get(7)?,
            operator: row.try_get(8)?,
            parent_type: row.try_get(9)?,
            child_type: row.try_get(10)?,
            collation: row.try_get(11)?,
        };
        use sqlx::postgres::types::Oid;
        let identity = (row.try_get::<Oid, _>(14)?.0, row.try_get::<Oid, _>(15)?.0);
        Ok((names, Some(comparison), row.try_get(12)?, row.try_get(13)?, Some(identity)))
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(|error| db_error(&error))?;
    Ok(links(source, parts))
}

async fn mysql_links(
    connection: &mut sqlx::MySqlConnection,
    source: &DbObject,
) -> Result<Vec<ForeignKeyLink>, DbError> {
    let parts = sqlx::query(
        "SELECT CONSTRAINT_NAME, TABLE_SCHEMA, TABLE_NAME, REFERENCED_TABLE_SCHEMA,
                REFERENCED_TABLE_NAME, COLUMN_NAME, REFERENCED_COLUMN_NAME
         FROM information_schema.KEY_COLUMN_USAGE
         WHERE REFERENCED_TABLE_NAME IS NOT NULL
           AND ((TABLE_SCHEMA = ? AND TABLE_NAME = ?)
             OR (REFERENCED_TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME = ?))
         ORDER BY TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION",
    )
    .bind(&source.schema)
    .bind(&source.name)
    .bind(&source.schema)
    .bind(&source.name)
    .fetch_all(connection)
    .await
    .map_err(|error| db_error(&error))?
    .into_iter()
    .map(|row| {
        // MySQL can report UTF-8 catalog names as VARBINARY.
        let names: ColumnNames = (
            row.try_get_unchecked(0)?,
            row.try_get_unchecked(1)?,
            row.try_get_unchecked(2)?,
            row.try_get_unchecked(3)?,
            row.try_get_unchecked(4)?,
            row.try_get_unchecked(5)?,
            row.try_get_unchecked(6)?,
        );
        let outbound = source.schema == names.1 && source.name == names.2;
        let inbound = source.schema == names.3 && source.name == names.4;
        Ok((names, None, outbound, inbound, None))
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(|error| db_error(&error))?;
    Ok(links(source, parts))
}

// Hold the relation lock through metadata validation and the SELECT.
macro_rules! pin_table {
    ($tx:expr, $object:expr, $dialect:expr) => {
        sqlx::query(&format!(
            "SELECT 1 FROM {} LIMIT 0",
            qualified($object, $dialect)
        ))
        .execute(&mut *$tx)
        .await
        .map_err(|e| db_error(&e))?
    };
}

impl DbPool {
    pub async fn foreign_key_links(
        &self,
        source: &DbObject,
    ) -> Result<Vec<ForeignKeyLink>, DbError> {
        if source.kind != DbObjectKind::Table {
            return Ok(Vec::new());
        }
        match self {
            Self::PostgreSql(pool) => {
                let mut tx = pool
                    .begin_with("START TRANSACTION READ ONLY")
                    .await
                    .map_err(|e| db_error(&e))?;
                let result = postgres_links(&mut tx, source).await;
                tx.rollback().await.map_err(|e| db_error(&e))?;
                result
            }
            Self::MySql(pool) => {
                let mut connection = pool.acquire().await.map_err(|e| db_error(&e))?;
                mysql_links(&mut connection, source).await
            }
        }
    }

    pub async fn read_related_rows(
        &self,
        source: &DbObject,
        key: &[(CellDetailKeyColumn, String)],
        link: &ForeignKeyLink,
    ) -> Result<TableData, DbError> {
        if key.is_empty() || source != &link.source {
            return Err("Для перехода по связи нужен первичный ключ исходной строки".into());
        }
        let target = link.target();
        match self {
            Self::PostgreSql(pool) => {
                let dialect = SqlDialect::PostgreSql;
                let mut tx = pool
                    .begin_with("START TRANSACTION READ ONLY")
                    .await
                    .map_err(|e| db_error(&e))?;
                pin_table!(tx, source, dialect);
                pin_table!(tx, target, dialect);
                let Some(partitioned) = table_key_scope_pg(&mut tx, source).await? else {
                    return Err("Первичный ключ не определяет строку для всего объекта".into());
                };
                confirm_link(postgres_links(&mut tx, source).await?, link)?;
                let target_partitioned = sqlx::query_scalar::<_, bool>(
                    "SELECT c.relkind = 'p' FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                     WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p')",
                )
                .bind(&target.schema)
                .bind(&target.name)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| db_error(&e))?;
                let source_columns = table_columns_pg(&mut *tx, source).await?;
                let keys = cell_detail_keys(&source_columns, key)?;
                let (columns, clipped) = limit_columns(table_columns_pg(&mut *tx, target).await?);
                let sql = related_sql(
                    link,
                    &columns,
                    &keys,
                    dialect,
                    &pg_from(source, partitioned),
                    &pg_from(target, target_partitioned),
                )?;
                let mut query = sqlx::query(&sql);
                for (_, value) in key {
                    query = query.bind(value);
                }
                let rows = query.fetch_all(&mut *tx).await;
                tx.rollback().await.map_err(|e| db_error(&e))?;
                Ok(related_data(
                    columns,
                    clipped,
                    rows.map_err(|e| db_error(&e))?,
                    |row, index| preview_cell(row.try_get(index * 2), row.try_get(index * 2 + 1)),
                ))
            }
            Self::MySql(pool) => {
                let dialect = SqlDialect::MySql;
                let mut tx = pool
                    .begin_with("START TRANSACTION READ ONLY")
                    .await
                    .map_err(|e| db_error(&e))?;
                pin_table!(tx, source, dialect);
                pin_table!(tx, target, dialect);
                if !mysql_is_base_table(&mut tx, source).await?
                    || !mysql_is_base_table(&mut tx, target).await?
                {
                    return Err("Объект больше не является обычной таблицей".into());
                }
                confirm_link(mysql_links(&mut tx, source).await?, link)?;
                let source_columns = table_columns_mysql(&mut *tx, source).await?;
                let keys = cell_detail_keys(&source_columns, key)?;
                let pk_columns = sqlx::query_scalar::<_, String>(
                    "SELECT CAST(COLUMN_NAME AS CHAR) FROM information_schema.KEY_COLUMN_USAGE
                     WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY'
                       AND REFERENCED_TABLE_NAME IS NULL
                       AND CAST(TABLE_SCHEMA AS BINARY) = CAST(? AS BINARY)
                       AND CAST(TABLE_NAME AS BINARY) = CAST(? AS BINARY)",
                )
                .bind(&source.schema)
                .bind(&source.name)
                .bind(&source.schema)
                .bind(&source.name)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| db_error(&e))?;
                if pk_columns.len() != keys.len()
                    || keys.iter().any(|key| !pk_columns.contains(&key.name))
                {
                    return Err(
                        "Для просмотра связанных строк нужен полный первичный ключ строки".into(),
                    );
                }
                let (columns, clipped) =
                    limit_columns(table_columns_mysql(&mut *tx, target).await?);
                let sql = related_sql(
                    link,
                    &columns,
                    &keys,
                    dialect,
                    &qualified(source, dialect),
                    &qualified(target, dialect),
                )?;
                let mut query = sqlx::query(&sql);
                for (column, (_, value)) in keys.iter().zip(key) {
                    query = query.bind(value);
                    if !column.binary {
                        query = query.bind(value);
                    }
                }
                let rows = query.fetch_all(&mut *tx).await;
                tx.rollback().await.map_err(|e| db_error(&e))?;
                Ok(related_data(
                    columns,
                    clipped,
                    rows.map_err(|e| db_error(&e))?,
                    |row, index| {
                        preview_cell(row.try_get_unchecked(index * 2), row.try_get(index * 2 + 1))
                    },
                ))
            }
        }
    }
}

fn confirm_link(links: Vec<ForeignKeyLink>, expected: &ForeignKeyLink) -> Result<(), DbError> {
    if links.contains(expected) {
        Ok(())
    } else {
        Err("Внешний ключ изменился. Откройте связанные строки заново".into())
    }
}

fn qualified(object: &DbObject, dialect: SqlDialect) -> String {
    format!(
        "{}.{}",
        dialect.quote(&object.schema),
        dialect.quote(&object.name)
    )
}

fn pg_from(object: &DbObject, partitioned: bool) -> String {
    let table = qualified(object, SqlDialect::PostgreSql);
    if partitioned {
        table
    } else {
        format!("ONLY {table}")
    }
}

fn related_sql(
    link: &ForeignKeyLink,
    columns: &[DbColumn],
    keys: &[&DbColumn],
    dialect: SqlDialect,
    source_from: &str,
    target_from: &str,
) -> Result<String, DbError> {
    if columns.is_empty() {
        return Err("Столбцы связанной таблицы недоступны".into());
    }
    let projection = columns
        .iter()
        .map(|column| {
            let name = format!("linked.{}", dialect.quote(&column.name));
            let (value, truncated) =
                cell_preview_projection_sql(column, &name, dialect, MAX_CELL_CHARS);
            format!("{value}, {truncated}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let (child_alias, parent_alias) = if link.outbound {
        ("origin", "linked")
    } else {
        ("linked", "origin")
    };
    let predicates = link
        .fk
        .columns
        .iter()
        .map(|pair| -> Result<String, DbError> {
            let parent = format!("{parent_alias}.{}", dialect.quote(&pair.parent));
            let child = format!("{child_alias}.{}", dialect.quote(&pair.child));
            let comparison = match &pair.pg {
                Some(comparison) => comparison.predicate(&parent, &child)?,
                None => format!("{parent} = {child}"),
            };
            Ok(format!(
                "{child} IS NOT NULL AND {parent} IS NOT NULL AND {comparison}"
            ))
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(" AND ");
    let source_columns = link
        .columns()
        .map(|(source, _)| dialect.quote(source))
        .collect::<Vec<_>>()
        .join(", ");
    let key = cell_key_predicates(keys, dialect);
    let limit = MAX_TABLE_ROWS + 1;
    Ok(format!(
        "SELECT {projection} FROM (SELECT {source_columns} FROM {source_from} WHERE {key}) AS origin
         JOIN {target_from} AS linked ON {predicates} LIMIT {limit}"
    ))
}

impl PgComparison {
    fn predicate(&self, parent: &str, child: &str) -> Result<String, DbError> {
        // Operator names are unquoted tokens in OPERATOR(schema.name).
        if self.operator.is_empty()
            || self.operator.contains("--")
            || self.operator.contains("/*")
            || !self
                .operator
                .bytes()
                .all(|ch| b"+-*/<>=~!@#%^&|?".contains(&ch))
        {
            return Err("Не поддерживается оператор внешнего ключа".into());
        }
        let cast = |value: &str, ty: Option<&str>| match ty {
            Some(ty) => format!("CAST({value} AS {ty})"),
            None => value.to_owned(),
        };
        let mut parent = cast(parent, self.parent_type.as_deref());
        let child = cast(child, self.child_type.as_deref());
        if let Some(collation) = &self.collation {
            parent = format!("({parent} COLLATE {collation})");
        }
        Ok(format!(
            "{parent} OPERATOR({}.{}) {child}",
            quote_pg_identifier(&self.operator_schema),
            self.operator,
        ))
    }
}

fn related_data<R>(
    columns: Vec<DbColumn>,
    clipped: bool,
    rows: Vec<R>,
    cell: impl Fn(&R, usize) -> CellValue,
) -> TableData {
    to_table_data(
        columns.into_iter().map(|column| column.name).collect(),
        rows,
        Vec::new(),
        clipped,
        cell,
    )
}

fn preview_cell(
    text: Result<Option<String>, sqlx::Error>,
    clipped: Result<bool, sqlx::Error>,
) -> CellValue {
    match (text, clipped) {
        (Ok(None), _) => CellValue::Null,
        (Ok(Some(text)), Ok(true)) => CellValue::Truncated(text),
        (Ok(Some(text)), Ok(false)) => CellValue::Text(text),
        (Err(error), _) | (_, Err(error)) => CellValue::DecodeError(error.to_string()),
    }
}
