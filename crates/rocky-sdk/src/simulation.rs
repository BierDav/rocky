//! A seeded, in-process simulation of several nodes on one thread and a
//! paused clock: random schedules of writes, syncs, link faults, restarts,
//! crashes, seals, bookmarks and membership changes, with safety checked
//! after every step and convergence after healing. A seed fixes the
//! schedule and every hub fault; a failure prints both.
mod cluster;
mod driver;
mod regressions;
mod schedule;
mod tests;
