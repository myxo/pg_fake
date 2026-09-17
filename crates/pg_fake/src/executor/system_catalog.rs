use crate::{
    catalog::{Catalog, IdentityKind, RelationName, SchemaId},
    error::{Result, SqlState},
    value::{BaseType, Oid, PgType, Value},
};

pub(crate) const PG_CATALOG_SCHEMA_OID: Oid = 11;

#[derive(Clone, Copy)]
pub(crate) struct SystemColumn {
    pub(crate) name: &'static str,
    pub(crate) data_type: PgType,
}

const fn column(name: &'static str, base: BaseType) -> SystemColumn {
    SystemColumn {
        name,
        data_type: PgType {
            base,
            typmod: PgType::NO_TYPEMOD,
        },
    }
}

const PG_NAMESPACE_COLUMNS: &[SystemColumn] = &[
    column("oid", BaseType::Oid),
    column("nspname", BaseType::Text),
];
const PG_CLASS_COLUMNS: &[SystemColumn] = &[
    column("oid", BaseType::Oid),
    column("relname", BaseType::Text),
    column("relnamespace", BaseType::Oid),
    column("relkind", BaseType::Bpchar),
    column("relpersistence", BaseType::Bpchar),
];
const PG_ATTRIBUTE_COLUMNS: &[SystemColumn] = &[
    column("attrelid", BaseType::Oid),
    column("attname", BaseType::Text),
    column("atttypid", BaseType::Oid),
    column("atttypmod", BaseType::Int4),
    column("attnum", BaseType::Int2),
    column("attnotnull", BaseType::Bool),
    column("attisdropped", BaseType::Bool),
    column("attidentity", BaseType::Bpchar),
];
const PG_TYPE_COLUMNS: &[SystemColumn] = &[
    column("oid", BaseType::Oid),
    column("typname", BaseType::Text),
    column("typnamespace", BaseType::Oid),
    column("typtype", BaseType::Bpchar),
    column("typcategory", BaseType::Bpchar),
    column("typelem", BaseType::Oid),
    column("typarray", BaseType::Oid),
];

pub(crate) fn describe_system_relation(name: &RelationName) -> Option<&'static [SystemColumn]> {
    if name
        .schema
        .as_deref()
        .is_some_and(|schema| schema != "pg_catalog")
    {
        return None;
    }
    match name.name.as_str() {
        "pg_namespace" => Some(PG_NAMESPACE_COLUMNS),
        "pg_class" => Some(PG_CLASS_COLUMNS),
        "pg_attribute" => Some(PG_ATTRIBUTE_COLUMNS),
        "pg_type" => Some(PG_TYPE_COLUMNS),
        _ => None,
    }
}

pub(crate) fn describe_visible_system_relation(
    catalog: &Catalog,
    name: &RelationName,
) -> Option<&'static [SystemColumn]> {
    if name.schema.is_none() && system_relation_is_shadowed(catalog, &name.name) {
        return None;
    }
    describe_system_relation(name)
}

fn system_relation_is_shadowed(catalog: &Catalog, name: &str) -> bool {
    let explicitly_lists_temp = catalog
        .search_path
        .iter()
        .any(|schema| schema == crate::catalog::TEMP_SCHEMA);
    let implicit_temp_shadows = !explicitly_lists_temp
        && relation_exists_in_schema(catalog, crate::catalog::TEMP_SCHEMA, name);
    if implicit_temp_shadows {
        return true;
    }
    let Some(pg_catalog_position) = catalog
        .search_path
        .iter()
        .position(|schema| schema == "pg_catalog")
    else {
        return false;
    };
    catalog.search_path[..pg_catalog_position]
        .iter()
        .any(|schema| relation_exists_in_schema(catalog, schema, name))
}

fn relation_exists_in_schema(catalog: &Catalog, schema: &str, name: &str) -> bool {
    let qualified = RelationName::create(Some(schema.to_owned()), name.to_owned());
    catalog
        .resolve_relation_name(&qualified)
        .is_ok_and(|resolved| catalog.has_resolved_relation(&resolved))
}

pub(crate) fn materialize_system_relation(
    catalog: &Catalog,
    name: &RelationName,
) -> Option<Vec<Vec<Value>>> {
    describe_visible_system_relation(catalog, name)?;
    match name.name.as_str() {
        "pg_namespace" => Some(
            std::iter::once(vec![
                Value::Oid(PG_CATALOG_SCHEMA_OID),
                Value::Text("pg_catalog".into()),
            ])
            .chain(catalog.iterate_schemas().map(|schema| {
                vec![
                    Value::Oid(map_schema_oid(schema.id)),
                    Value::Text(schema.name.clone()),
                ]
            }))
            .collect(),
        ),
        "pg_class" => Some(materialize_pg_class(catalog)),
        "pg_attribute" => Some(materialize_pg_attribute(catalog)),
        "pg_type" => Some(materialize_pg_type()),
        _ => None,
    }
}

fn materialize_pg_class(catalog: &Catalog) -> Vec<Vec<Value>> {
    let system = [
        (1259, "pg_class"),
        (1249, "pg_attribute"),
        (1247, "pg_type"),
        (2615, "pg_namespace"),
    ]
    .into_iter()
    .map(|(oid, name)| class_row(oid, name, PG_CATALOG_SCHEMA_OID, "r", "p"));
    let tables = catalog.iterate_tables().map(|table| {
        class_row(
            map_table_oid(table.id),
            &table.name,
            map_schema_oid(table.schema_id),
            "r",
            if matches!(
                table.persistence,
                crate::catalog::TablePersistence::Temporary { .. }
            ) {
                "t"
            } else {
                "p"
            },
        )
    });
    let indexes = catalog.iterate_tables().flat_map(|table| {
        table.indexes.iter().map(|index| {
            class_row(
                map_index_oid(index.id),
                &index.name,
                map_schema_oid(table.schema_id),
                "i",
                if matches!(
                    table.persistence,
                    crate::catalog::TablePersistence::Temporary { .. }
                ) {
                    "t"
                } else {
                    "p"
                },
            )
        })
    });
    let constraint_indexes = catalog.iterate_tables().flat_map(|table| {
        table
            .constraints
            .iter()
            .filter_map(|constraint| match constraint {
                crate::catalog::Constraint::PrimaryKey { id, name, .. }
                | crate::catalog::Constraint::Unique { id, name, .. } => Some(class_row(
                    map_constraint_index_oid(*id),
                    name,
                    map_schema_oid(table.schema_id),
                    "i",
                    if matches!(
                        table.persistence,
                        crate::catalog::TablePersistence::Temporary { .. }
                    ) {
                        "t"
                    } else {
                        "p"
                    },
                )),
                _ => None,
            })
    });
    let views = catalog.iterate_views().map(|view| {
        class_row(
            map_view_oid(view.id),
            &view.name,
            map_schema_oid(view.schema_id),
            "v",
            if catalog.get_schema_name(view.schema_id) == crate::catalog::TEMP_SCHEMA {
                "t"
            } else {
                "p"
            },
        )
    });
    let sequences = catalog.iterate_sequences().map(|sequence| {
        class_row(
            map_sequence_oid(sequence.id),
            &sequence.name,
            map_schema_oid(sequence.schema_id),
            "S",
            if catalog.get_schema_name(sequence.schema_id) == crate::catalog::TEMP_SCHEMA {
                "t"
            } else {
                "p"
            },
        )
    });
    system
        .chain(tables)
        .chain(indexes)
        .chain(constraint_indexes)
        .chain(views)
        .chain(sequences)
        .collect()
}

fn class_row(oid: Oid, name: &str, namespace: Oid, kind: &str, persistence: &str) -> Vec<Value> {
    vec![
        Value::Oid(oid),
        Value::Text(name.into()),
        Value::Oid(namespace),
        Value::Text(kind.into()),
        Value::Text(persistence.into()),
    ]
}

fn materialize_pg_attribute(catalog: &Catalog) -> Vec<Vec<Value>> {
    catalog
        .iterate_tables()
        .flat_map(|table| {
            table
                .columns
                .iter()
                .enumerate()
                .map(move |(index, column)| {
                    vec![
                        Value::Oid(map_table_oid(table.id)),
                        Value::Text(column.name.clone()),
                        Value::Oid(column.data_type.map_to_oid()),
                        Value::Int4(column.data_type.typmod),
                        Value::Int2(i16::try_from(index + 1).expect("column number fits int2")),
                        Value::Bool(!column.nullable),
                        Value::Bool(false),
                        Value::Text(
                            match column.identity {
                                Some(IdentityKind::Always) => "a",
                                Some(IdentityKind::ByDefault) => "d",
                                None => "",
                            }
                            .into(),
                        ),
                    ]
                })
        })
        .chain(catalog.iterate_views().flat_map(|view| {
            view.columns.iter().enumerate().map(move |(index, column)| {
                vec![
                    Value::Oid(map_view_oid(view.id)),
                    Value::Text(column.name.clone()),
                    Value::Oid(column.data_type.map_to_oid()),
                    Value::Int4(column.data_type.typmod),
                    Value::Int2(i16::try_from(index + 1).expect("column number fits int2")),
                    Value::Bool(false),
                    Value::Bool(false),
                    Value::Text(String::new()),
                ]
            })
        }))
        .collect()
}

fn materialize_pg_type() -> Vec<Vec<Value>> {
    supported_types()
        .iter()
        .map(|base| {
            let element = base
                .get_array_element_type()
                .map_or(0, BaseType::map_to_oid);
            let array = type_array_oid(*base);
            vec![
                Value::Oid(base.map_to_oid()),
                Value::Text(base.get_postgres_name().into()),
                Value::Oid(PG_CATALOG_SCHEMA_OID),
                Value::Text("b".into()),
                Value::Text(type_category(*base).into()),
                Value::Oid(element),
                Value::Oid(array),
            ]
        })
        .collect()
}

fn type_array_oid(base: BaseType) -> Oid {
    match base {
        BaseType::Bool => 1000,
        BaseType::Bytea => 1001,
        BaseType::Int2 => 1005,
        BaseType::Int4 => 1007,
        BaseType::Text => 1009,
        BaseType::Bpchar => 1014,
        BaseType::Varchar => 1015,
        BaseType::Int8 => 1016,
        BaseType::Float4 => 1021,
        BaseType::Float8 => 1022,
        BaseType::Oid => 1028,
        BaseType::Timestamp => 1115,
        BaseType::Date => 1182,
        BaseType::Time => 1183,
        BaseType::TimestampTz => 1185,
        BaseType::Interval => 1187,
        BaseType::Numeric => 1231,
        BaseType::Json => 199,
        BaseType::Regclass => 2210,
        BaseType::Uuid => 2951,
        BaseType::PgLsn => 3221,
        BaseType::Jsonb => 3807,
        BaseType::Void | BaseType::TextArray | BaseType::Int8Array | BaseType::UuidArray => 0,
    }
}

fn supported_types() -> &'static [BaseType] {
    &[
        BaseType::Bool,
        BaseType::Int2,
        BaseType::Int4,
        BaseType::Int8,
        BaseType::Oid,
        BaseType::Float4,
        BaseType::Float8,
        BaseType::Numeric,
        BaseType::Text,
        BaseType::Varchar,
        BaseType::Bpchar,
        BaseType::Bytea,
        BaseType::Uuid,
        BaseType::Date,
        BaseType::Time,
        BaseType::Timestamp,
        BaseType::TimestampTz,
        BaseType::Interval,
        BaseType::Json,
        BaseType::Jsonb,
        BaseType::PgLsn,
        BaseType::Regclass,
        BaseType::TextArray,
        BaseType::Int8Array,
        BaseType::UuidArray,
    ]
}

fn type_category(base: BaseType) -> &'static str {
    match base {
        BaseType::Bool => "B",
        BaseType::Int2
        | BaseType::Int4
        | BaseType::Int8
        | BaseType::Oid
        | BaseType::Regclass
        | BaseType::Float4
        | BaseType::Float8
        | BaseType::Numeric => "N",
        BaseType::Text | BaseType::Varchar | BaseType::Bpchar => "S",
        BaseType::Date | BaseType::Time | BaseType::Timestamp | BaseType::TimestampTz => "D",
        BaseType::Interval => "T",
        BaseType::TextArray | BaseType::Int8Array | BaseType::UuidArray => "A",
        _ => "U",
    }
}

pub(crate) fn resolve_regclass(catalog: &Catalog, input: &str) -> Result<Option<Oid>> {
    resolve_regclass_inner(catalog, input, false)
}

pub(crate) fn resolve_regclass_lenient(catalog: &Catalog, input: &str) -> Result<Option<Oid>> {
    resolve_regclass_inner(catalog, input, true)
}

fn resolve_regclass_inner(catalog: &Catalog, input: &str, lenient: bool) -> Result<Option<Oid>> {
    let input = input.trim();
    if input == "-" {
        return Ok(Some(0));
    }
    if !input.is_empty() && input.bytes().all(|byte| byte.is_ascii_digit()) {
        return match input.parse::<Oid>() {
            Ok(oid) => Ok(Some(oid)),
            Err(_) if lenient => Ok(None),
            Err(_) => Err(crate::error::PgError::create(
                SqlState::NumericValueOutOfRange,
                format!("value {input:?} is out of range for type oid"),
            )),
        };
    }
    let name = match crate::executor::normalize_sequence_name(input) {
        Ok(name) => name,
        Err(error) if lenient && error.sqlstate == SqlState::InvalidName => return Ok(None),
        Err(error) => return Err(error),
    };
    if describe_visible_system_relation(catalog, &name).is_some() {
        return Ok(system_relation_oid(&name.name));
    }
    let resolved = match catalog.resolve_relation_name(&name) {
        Ok(resolved) => resolved,
        Err(error)
            if matches!(
                error.sqlstate,
                SqlState::UndefinedTable | SqlState::InvalidSchemaName
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let name = RelationName::create(
        Some(catalog.get_schema_name(resolved.schema_id).to_owned()),
        resolved.name.clone(),
    );
    if let Ok(table) = catalog.require_named_table(&name) {
        return Ok(Some(map_table_oid(table.id)));
    }
    if let Ok(view) = catalog.require_named_view(&name) {
        return Ok(Some(map_view_oid(view.id)));
    }
    if let Ok(sequence) = catalog.require_named_sequence(&name) {
        return Ok(Some(map_sequence_oid(sequence.id)));
    }
    if let Ok((_, index)) = catalog.require_named_index(&name) {
        return Ok(Some(map_index_oid(index.id)));
    }
    if let Some((_, id)) = catalog.resolve_constraint_index(&resolved) {
        return Ok(Some(map_constraint_index_oid(id)));
    }
    Ok(None)
}

pub(crate) fn format_regclass(catalog: &Catalog, oid: Oid) -> Result<String> {
    if oid == 0 {
        return Ok("-".into());
    }
    if let Some(name) = system_relation_name(oid) {
        let unqualified = RelationName::create_unqualified(name);
        if describe_visible_system_relation(catalog, &unqualified).is_some() {
            return Ok(format_identifier(name));
        }
        return Ok(format!("pg_catalog.{}", format_identifier(name)));
    }
    for table in catalog.iterate_tables() {
        if map_table_oid(table.id) == oid {
            return format_relation_name(catalog, table.schema_id, &table.name, oid);
        }
        if let Some(index) = table
            .indexes
            .iter()
            .find(|index| map_index_oid(index.id) == oid)
        {
            return format_relation_name(catalog, table.schema_id, &index.name, oid);
        }
        if let Some(name) = table
            .constraints
            .iter()
            .find_map(|constraint| match constraint {
                crate::catalog::Constraint::PrimaryKey { id, name, .. }
                | crate::catalog::Constraint::Unique { id, name, .. }
                    if map_constraint_index_oid(*id) == oid =>
                {
                    Some(name.as_str())
                }
                _ => None,
            })
        {
            return format_relation_name(catalog, table.schema_id, name, oid);
        }
    }
    if let Some(view) = catalog
        .iterate_views()
        .find(|view| map_view_oid(view.id) == oid)
    {
        return format_relation_name(catalog, view.schema_id, &view.name, oid);
    }
    if let Some(sequence) = catalog
        .iterate_sequences()
        .find(|sequence| map_sequence_oid(sequence.id) == oid)
    {
        return format_relation_name(catalog, sequence.schema_id, &sequence.name, oid);
    }
    Ok(oid.to_string())
}

fn format_relation_name(
    catalog: &Catalog,
    schema_id: SchemaId,
    name: &str,
    oid: Oid,
) -> Result<String> {
    if resolve_regclass(catalog, &format_identifier(name))? == Some(oid) {
        return Ok(format_identifier(name));
    }
    Ok(format!(
        "{}.{}",
        format_identifier(catalog.get_schema_name(schema_id)),
        format_identifier(name)
    ))
}

pub(crate) fn format_type(oid: Oid, typmod: i32) -> Result<String> {
    let Some(base) = BaseType::resolve_oid(oid) else {
        return Ok("???".into());
    };
    let plain = match base {
        BaseType::Bool => "boolean",
        BaseType::Int2 => "smallint",
        BaseType::Int4 => "integer",
        BaseType::Int8 => "bigint",
        BaseType::Float4 => "real",
        BaseType::Float8 => "double precision",
        BaseType::Bpchar => "character",
        BaseType::Varchar => "character varying",
        BaseType::Timestamp => "timestamp without time zone",
        BaseType::TimestampTz => "timestamp with time zone",
        BaseType::TextArray => "text[]",
        BaseType::Int8Array => "bigint[]",
        BaseType::UuidArray => "uuid[]",
        _ => base.get_postgres_name(),
    };
    if typmod == PgType::NO_TYPEMOD {
        return Ok(plain.into());
    }
    match base {
        BaseType::Bpchar | BaseType::Varchar | BaseType::Numeric if typmod <= 4 => Ok(plain.into()),
        BaseType::Bpchar | BaseType::Varchar => Ok(format!("{plain}({})", typmod - 4)),
        BaseType::Numeric => {
            let encoded = typmod - 4;
            let encoded_scale = encoded & 0x7ff;
            let scale = if encoded_scale & 0x400 == 0 {
                encoded_scale
            } else {
                encoded_scale | !0x7ff
            };
            Ok(format!("numeric({},{scale})", encoded >> 16))
        }
        BaseType::Time | BaseType::Timestamp | BaseType::TimestampTz => {
            let (prefix, suffix) = match base {
                BaseType::Time => ("time", "without time zone"),
                BaseType::Timestamp => ("timestamp", "without time zone"),
                BaseType::TimestampTz => ("timestamp", "with time zone"),
                _ => unreachable!(),
            };
            Ok(format!("{prefix}({typmod}) {suffix}"))
        }
        _ => Ok(plain.into()),
    }
}

fn system_relation_oid(name: &str) -> Option<Oid> {
    match name {
        "pg_class" => Some(1259),
        "pg_attribute" => Some(1249),
        "pg_type" => Some(1247),
        "pg_namespace" => Some(2615),
        _ => None,
    }
}

fn system_relation_name(oid: Oid) -> Option<&'static str> {
    match oid {
        1259 => Some("pg_class"),
        1249 => Some("pg_attribute"),
        1247 => Some("pg_type"),
        2615 => Some("pg_namespace"),
        _ => None,
    }
}

fn map_schema_oid(id: SchemaId) -> Oid {
    50_000u32
        .checked_add(u32::try_from(id.0).expect("schema id fits oid"))
        .expect("schema oid fits")
}

fn map_table_oid(id: crate::catalog::TableId) -> Oid {
    100_000u32
        .checked_add(u32::try_from(id.0).expect("table id fits oid"))
        .expect("table oid fits")
}

fn map_sequence_oid(id: crate::catalog::SequenceId) -> Oid {
    200_000u32
        .checked_add(u32::try_from(id.0).expect("sequence id fits oid"))
        .expect("sequence oid fits")
}

fn map_view_oid(id: crate::catalog::ViewId) -> Oid {
    300_000u32
        .checked_add(u32::try_from(id.0).expect("view id fits oid"))
        .expect("view oid fits")
}

fn map_index_oid(id: crate::catalog::IndexId) -> Oid {
    400_000u32
        .checked_add(u32::try_from(id.0).expect("index id fits oid"))
        .expect("index oid fits")
}

fn map_constraint_index_oid(id: crate::catalog::ConstraintId) -> Oid {
    500_000u32
        .checked_add(u32::try_from(id.0).expect("constraint id fits oid"))
        .expect("constraint index oid fits")
}

fn format_identifier(identifier: &str) -> String {
    super::sequences::format_postgres_identifier(identifier)
}
