use crate::common::Failure;
use rocky_sdk::{Affinity, Column, Role, RoleName, Schema, Table};
use std::path::Path;

const DOCTOR: RoleName = RoleName::from_static("doctor");

fn clinic() -> Result<Schema, rocky_sdk::Error> {
    let charts = Table::replayed("charts")
        .column(Column::new("ward", Affinity::Text).required())
        .column(Column::new("note", Affinity::Text).required())
        .primary_key(["ward", "id"])
        .partition_by(["ward"]);
    let doctor = Role::new(DOCTOR)
        .read("wards")
        .append("charts")
        .update_columns("charts", ["note"])
        .per_partition("charts");
    Ok(Schema::builder("clinic", 1)
        .table(Table::replayed("wards").column(Column::new("name", Affinity::Text).required()))
        .table(charts)
        .role(doctor)
        .build()?)
}

mod one_device {
    include!("../../examples/one_device.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

mod refused_write {
    include!("../../examples/refused_write.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

mod renamed_column {
    include!("../../examples/renamed_column.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

mod two_devices {
    include!("../../examples/two_devices.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

fn repository() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

#[test]
fn every_rust_block_in_the_readme_appears_verbatim_in_its_example_file() -> Result<(), Failure> {
    let readme = std::fs::read_to_string(repository().join("README.md"))?;
    let mut lines = readme.lines();
    let mut checked = Vec::new();
    while let Some(line) = lines.next() {
        let Some(info) = line.strip_prefix("```rust") else {
            continue;
        };
        let file = info.trim();
        assert!(
            !file.is_empty(),
            "a Rust block in the README must name its example file after ```rust"
        );
        let block: String = lines
            .by_ref()
            .take_while(|line| *line != "```")
            .map(|line| format!("{line}\n"))
            .collect();
        let source = std::fs::read_to_string(repository().join(file))?;
        assert!(
            source.contains(&block),
            "the README's block for {file} differs from the file; copy one into the other"
        );
        checked.push(file.to_owned());
    }
    assert!(
        checked.len() >= 2,
        "the README shows the one- and two-device examples: {checked:?}"
    );
    Ok(())
}

#[test]
fn the_readme_role_recipe_names_its_partition_group_after_the_table() -> Result<(), Failure> {
    clinic()?;
    Ok(())
}

#[test]
fn the_one_device_example_reads_back_its_note() -> Result<(), Box<dyn std::error::Error>> {
    one_device::run()
}

#[test]
fn the_two_device_example_syncs_a_note_over_loopback_tcp() -> Result<(), Box<dyn std::error::Error>>
{
    two_devices::run()
}

#[test]
fn the_refused_write_example_matches_a_clock_value_in_a_replayed_write()
-> Result<(), Box<dyn std::error::Error>> {
    refused_write::run()
}

#[test]
fn the_renamed_column_example_lands_an_offline_write_under_the_new_name()
-> Result<(), Box<dyn std::error::Error>> {
    renamed_column::run()
}
