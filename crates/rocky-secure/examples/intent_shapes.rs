//! Aggregates the shapes `scripts/intent-shapes` captured: one JSON-lines
//! file per source in, one JSON summary out, Markdown tables on stdout.
//! Usage: intent_shapes <captured directory> <summary.json>
use rocky_secure::shapes::{Class, Op, Origin, Shape};
use serde::Serialize;
use std::collections::BTreeMap;

/// Phase 6 takes the operations present in at least this share of transactions.
const FAST_PATH_SHARE: f64 = 0.05;
/// More refusals than this share in any source stops before phase 4.
const REFUSED_STOP_SHARE: f64 = 0.10;
const ROUTINE_SEAL_TRANSACTIONS: usize = 1024;
/// Rocky's reserved issuer registry, written by the SDK rather than the application.
const RESERVED: &[&str] = &["issuer_registry"];

fn internal(shape: &Shape) -> bool {
    !shape.tables.is_empty()
        && shape
            .tables
            .iter()
            .all(|table| RESERVED.contains(&table.as_str()))
}

#[derive(Default, Serialize)]
struct Summary {
    transactions: usize,
    /// Commits that changed no replicated row.
    without_replicated_rows: usize,
    migrations: usize,
    /// Commits only to Rocky's reserved tables.
    internal: usize,
    classes: BTreeMap<Class, usize>,
    statements: usize,
    max_statements: usize,
    writes: usize,
    collapses: usize,
    algebra: BTreeMap<Op, usize>,
    algebra_only: usize,
    refusals: BTreeMap<String, usize>,
    /// Most transactions one process committed within any 60 seconds.
    peak_per_minute: usize,
}
impl Summary {
    fn add(&mut self, shape: &Shape) {
        if shape.origin == Origin::Migration {
            self.migrations += 1;
            return;
        }
        let Some(class) = shape.class else {
            self.without_replicated_rows += 1;
            return;
        };
        if internal(shape) {
            self.internal += 1;
            return;
        }
        self.transactions += 1;
        *self.classes.entry(class).or_default() += 1;
        self.statements += shape.statements;
        self.max_statements = self.max_statements.max(shape.statements);
        self.writes += shape.writes;
        self.collapses += usize::from(shape.collapses);
        self.algebra_only += usize::from(shape.algebra_only);
        for op in &shape.algebra {
            *self.algebra.entry(*op).or_default() += 1;
        }
        if let Some(refusal) = &shape.refusal {
            *self.refusals.entry(refusal.clone()).or_default() += 1;
        }
    }
    fn share(&self, count: usize) -> f64 {
        count as f64 / self.transactions.max(1) as f64
    }
}
fn peak_per_minute(shapes: &[Shape]) -> usize {
    let mut by_process: BTreeMap<u32, Vec<u64>> = BTreeMap::new();
    let counted = |shape: &&Shape| {
        shape.class.is_some() && shape.origin != Origin::Migration && !internal(shape)
    };
    for shape in shapes.iter().filter(counted) {
        by_process
            .entry(shape.process)
            .or_default()
            .push(shape.at_ms);
    }
    by_process
        .values_mut()
        .map(|times| {
            times.sort_unstable();
            let mut start = 0;
            (0..times.len())
                .map(|end| {
                    while times[end] - times[start] >= 60_000 {
                        start += 1;
                    }
                    end + 1 - start
                })
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

#[derive(Serialize)]
struct Decisions {
    fast_path_operations: Vec<Op>,
    refused_stop_sources: Vec<String>,
    shopping_peak_per_minute: Option<usize>,
    routine_seal_transactions: usize,
}
#[derive(Serialize)]
struct Report {
    sources: BTreeMap<String, Summary>,
    total: Summary,
    decisions: Decisions,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        return Err("usage: intent_shapes <captured directory> <summary.json>".into());
    };
    let mut sources = BTreeMap::new();
    let mut total = Summary::default();
    let mut paths: Vec<_> = std::fs::read_dir(&input)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    paths.sort();
    for path in paths
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
    {
        let source = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or("source file name")?
            .to_owned();
        let shapes = std::fs::read_to_string(path)?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<Vec<Shape>, _>>()?;
        let mut summary = Summary::default();
        for shape in &shapes {
            summary.add(shape);
            total.add(shape);
        }
        summary.peak_per_minute = peak_per_minute(&shapes);
        total.peak_per_minute = total.peak_per_minute.max(summary.peak_per_minute);
        sources.insert(source, summary);
    }
    let fast_path_operations = total
        .algebra
        .iter()
        .filter(|(_, count)| total.share(**count) >= FAST_PATH_SHARE)
        .map(|(op, _)| *op)
        .collect();
    let refused_stop_sources = sources
        .iter()
        .filter(|(_, summary)| {
            summary.share(summary.classes.get(&Class::Refused).copied().unwrap_or(0))
                > REFUSED_STOP_SHARE
        })
        .map(|(source, _)| source.clone())
        .collect();
    let shopping_peak_per_minute = sources
        .iter()
        .filter(|(source, _)| source.starts_with("shopping"))
        .map(|(_, summary)| summary.peak_per_minute)
        .max();
    let routine_seal_transactions = match shopping_peak_per_minute {
        Some(peak) if peak > ROUTINE_SEAL_TRANSACTIONS => (peak + 1).next_power_of_two(),
        _ => ROUTINE_SEAL_TRANSACTIONS,
    };
    let report = Report {
        decisions: Decisions {
            fast_path_operations,
            refused_stop_sources,
            shopping_peak_per_minute,
            routine_seal_transactions,
        },
        sources,
        total,
    };
    std::fs::write(&output, serde_json::to_string_pretty(&report)? + "\n")?;
    print_tables(&report);
    Ok(())
}

fn print_tables(report: &Report) {
    let classes = [
        Class::Refused,
        Class::Coordinated,
        Class::Constrained,
        Class::Keyed,
        Class::Predicate,
        Class::Split,
    ];
    let ops = [
        Op::Add,
        Op::Assign,
        Op::Append,
        Op::KeyedDelete,
        Op::FreshInsert,
    ];
    let rows = report
        .sources
        .iter()
        .map(|(source, summary)| (source.as_str(), summary))
        .chain([("**total**", &report.total)]);
    println!(
        "| Source | Transactions | Refused | Coordinated | Constrained | Keyed | Predicate | Split | Statements (mean / max) | Collapse | Not replicated | Migration | Rocky-internal |"
    );
    println!(
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    );
    for (source, summary) in rows.clone() {
        let counts: Vec<String> = classes
            .iter()
            .map(|class| summary.classes.get(class).copied().unwrap_or(0).to_string())
            .collect();
        println!(
            "| {source} | {} | {} | {:.2} / {} | {} | {} | {} | {} |",
            summary.transactions,
            counts.join(" | "),
            summary.statements as f64 / summary.transactions.max(1) as f64,
            summary.max_statements,
            summary.collapses,
            summary.without_replicated_rows,
            summary.migrations,
            summary.internal,
        );
    }
    println!();
    println!("| Source | Add | Assign | Append | Keyed delete | Fresh insert | Algebra only |");
    println!("| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for (source, summary) in rows {
        let counts: Vec<String> = ops
            .iter()
            .map(|op| summary.algebra.get(op).copied().unwrap_or(0).to_string())
            .collect();
        println!(
            "| {source} | {} | {} |",
            counts.join(" | "),
            summary.algebra_only
        );
    }
    println!();
    println!(
        "decisions: {}",
        serde_json::to_string(&report.decisions).unwrap_or_default()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadence_counts_moves_but_not_migrations_or_internal_writes() {
        let moved = Shape {
            process: 1,
            at_ms: 0,
            origin: Origin::Move,
            class: Some(Class::Keyed),
            refusal: None,
            statements: 0,
            writes: 1,
            rows: 0,
            partitions: 2,
            collapses: false,
            algebra: Default::default(),
            algebra_only: false,
            tables: Default::default(),
            sql: Vec::new(),
        };
        let mut shapes = vec![moved.clone(); 3];
        shapes[1].at_ms = 59_999;
        shapes[2].at_ms = 60_000;
        let mut migration = moved.clone();
        migration.origin = Origin::Migration;
        shapes.push(migration);
        let mut registry = moved.clone();
        registry.tables.insert("issuer_registry".into());
        shapes.push(registry);
        let mut other_process = moved;
        other_process.process = 2;
        shapes.push(other_process);
        assert_eq!(peak_per_minute(&shapes), 2);
    }
}
