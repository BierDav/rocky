use rocky_sdk::{
    Affinity, Column, Failure, IndexColumn, IndexDefinition, Migration, MigrationStep, Schema,
    Table,
};
use serde::Deserialize;

pub const ITEMS: &str = "items";
pub const PROBES: &str = "probes";
pub const TAGGED_VERSION: u32 = 2;
const RETAG: &str = "UPDATE items SET tag = substr(title, 1, 4) || '-' || (n % 97)";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    #[default]
    Flat,
    PartitionedByGroup,
}

fn items(tagged: bool, layout: Layout) -> Table {
    let mut table = Table::replayed(ITEMS)
        .column(Column::new("grp", Affinity::Text).required())
        .column(Column::new("title", Affinity::Text).required())
        .column(Column::new("body", Affinity::Text).required())
        .column(Column::new("n", Affinity::Integer).required().default(0));
    if layout == Layout::PartitionedByGroup {
        table = table.primary_key(["grp", "id"]).partition_by(["grp"]);
    }
    if !tagged {
        return table;
    }
    table
        .column(Column::new("tag", Affinity::Text).required().default(""))
        .index(IndexDefinition {
            name: "items_tag".into(),
            columns: vec![IndexColumn {
                name: "tag".into(),
                descending: false,
            }],
            unique: false,
            expressions: Vec::new(),
            predicate: None,
        })
}

fn probes() -> Table {
    Table::replayed(PROBES)
        .column(Column::new("origin", Affinity::Text).required())
        .column(Column::new("seq", Affinity::Integer).required())
        .column(Column::new("sent", Affinity::Integer).required())
}

pub fn schema(version: u32, layout: Layout) -> Result<Schema, Failure> {
    let tagged = version >= TAGGED_VERSION;
    let mut schema = Schema::builder("mesh", version)
        .table(items(tagged, layout))
        .table(probes())
        .build()?;
    if tagged {
        schema.migrations.push(Migration {
            version: TAGGED_VERSION,
            statements: vec![MigrationStep::Sql(RETAG.into())],
        });
    }
    Ok(schema)
}
