use diesel::prelude::*;
use serde::Serialize;

#[derive(Queryable, Selectable, Serialize, Insertable, Clone)]
#[diesel(table_name = crate::schema::links)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Link {
    pub id: String,
    pub url: String,
    pub original_hash: Option<String>,
    pub transcoded_hash: Option<String>,
}
