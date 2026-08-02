use std::{borrow::Cow, fmt, sync::Arc};

use pg_fake::value::Value as CoreValue;
use sqlx::{Column, ColumnIndex, Row, Value, ValueRef};

use crate::{PgFake, PgFakeTypeInfo};

#[derive(Debug, Clone)]
pub struct PgFakeColumn {
    pub(crate) ordinal: usize,
    pub(crate) name: String,
    pub(crate) type_info: PgFakeTypeInfo,
}

impl Column for PgFakeColumn {
    type Database = PgFake;

    fn ordinal(&self) -> usize {
        self.ordinal
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn type_info(&self) -> &PgFakeTypeInfo {
        &self.type_info
    }
}

#[derive(Debug, Clone)]
pub struct PgFakeValue {
    pub(crate) value: CoreValue,
    pub(crate) type_info: PgFakeTypeInfo,
}

#[derive(Debug, Clone, Copy)]
pub struct PgFakeValueRef<'r> {
    pub(crate) value: &'r CoreValue,
    pub(crate) type_info: PgFakeTypeInfo,
}

impl Value for PgFakeValue {
    type Database = PgFake;

    fn as_ref(&self) -> PgFakeValueRef<'_> {
        PgFakeValueRef {
            value: &self.value,
            type_info: self.type_info,
        }
    }

    fn type_info(&self) -> Cow<'_, PgFakeTypeInfo> {
        Cow::Borrowed(&self.type_info)
    }

    fn is_null(&self) -> bool {
        self.value.is_null()
    }
}

impl<'r> ValueRef<'r> for PgFakeValueRef<'r> {
    type Database = PgFake;

    fn to_owned(&self) -> PgFakeValue {
        PgFakeValue {
            value: self.value.clone(),
            type_info: self.type_info,
        }
    }

    fn type_info(&self) -> Cow<'_, PgFakeTypeInfo> {
        Cow::Owned(self.type_info)
    }

    fn is_null(&self) -> bool {
        self.value.is_null()
    }
}

#[derive(Clone)]
pub struct PgFakeRow {
    pub(crate) columns: Arc<Vec<PgFakeColumn>>,
    pub(crate) values: Vec<PgFakeValue>,
}

impl fmt::Debug for PgFakeRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PgFakeRow")
            .field("columns", &self.columns)
            .field("values", &self.values)
            .finish()
    }
}

impl Row for PgFakeRow {
    type Database = PgFake;

    fn columns(&self) -> &[PgFakeColumn] {
        &self.columns
    }

    fn try_get_raw<I>(&self, index: I) -> Result<PgFakeValueRef<'_>, sqlx::Error>
    where
        I: ColumnIndex<Self>,
    {
        Ok(self.values[index.index(self)?].as_ref())
    }
}

sqlx_core::impl_column_index_for_row!(PgFakeRow);

impl ColumnIndex<PgFakeRow> for str {
    fn index(&self, row: &PgFakeRow) -> Result<usize, sqlx::Error> {
        row.columns
            .iter()
            .position(|column| column.name == self)
            .ok_or_else(|| sqlx::Error::ColumnNotFound(self.to_owned()))
    }
}
