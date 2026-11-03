//! What differs per target, behind one API: tasks, timers and files.
//! Natively these are tokio and the filesystem. On wasm32, a browser's
//! dedicated Worker, they are its event loop, its clock and Rocky's OPFS
//! pool. Code above this crate is the same on every target.
pub mod fs;
pub mod task;
pub mod time;
