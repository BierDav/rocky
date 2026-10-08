# PostgreSQL full-node backend

Status: deferred idea, not accepted release scope. [V4](../proposals/architecture-proposal-v4.md)
keeps SQLite as the first engine. Language/ORM independence does not imply engine
interchangeability. A PostgreSQL adapter needs its own transaction/capture,
checkpoint, admission and recovery evidence before it can serve Rocky data.
