pub(crate) use rocky_storage::storage::*;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE rocky_replica(id INTEGER PRIMARY KEY CHECK(id=1),identity BLOB NOT NULL,sequence INTEGER NOT NULL,lamport INTEGER NOT NULL,revision INTEGER NOT NULL)",
    "CREATE TABLE rocky_operations(node BLOB NOT NULL,sequence INTEGER NOT NULL,bytes BLOB NOT NULL,lamport INTEGER NOT NULL,status TEXT NOT NULL,origin TEXT NOT NULL,PRIMARY KEY(node,sequence))",
    "CREATE TABLE rocky_mutation_handles(handle BLOB PRIMARY KEY,request_hash BLOB NOT NULL,node BLOB NOT NULL,sequence INTEGER NOT NULL)",
    "CREATE TABLE rocky_outbox(node BLOB NOT NULL,sequence INTEGER NOT NULL,PRIMARY KEY(node,sequence))",
];

/// Pending and rejected operations by key, so neither a pass over pending
/// work nor a backlog count scans applied history.
pub(crate) const UNAPPLIED_INDEX: &str = "CREATE INDEX IF NOT EXISTS rocky_operations_unapplied ON rocky_operations(status,node,sequence) WHERE status<>'applied'";

pub(crate) const PEER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS rocky_peer_ack(peer BLOB NOT NULL,node BLOB NOT NULL,sequence INTEGER NOT NULL,applied INTEGER NOT NULL,PRIMARY KEY(peer,node,sequence))";
