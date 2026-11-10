//! anime-comp core library.
//!
//! * [`source`] — frame-accurate random access to video frames (ffms2)
//! * [`convert`] — YUV → RGB conversion (zimg) and PNG output
//! * [`filter`] — IVTC / deinterlacing with verified frame-exact random access
//! * [`select`] — frame selection on the reference timeline
//! * [`comp`] — end-to-end comparison runner

pub mod chapters;
pub mod comp;
pub mod convert;
pub mod crop;
pub mod edit;
pub mod error;
pub mod filter;
pub mod season;
pub mod select;
pub mod pairing;
pub mod project;
pub mod source;
pub mod stats;
pub mod sync;
pub mod viewer;

pub use error::{Error, Result};
