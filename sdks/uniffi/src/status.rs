//! UniFFI exports the native declarations without a second handwritten model.
use rocky_sdk::*;
include!(concat!(env!("OUT_DIR"), "/status.rs"));
