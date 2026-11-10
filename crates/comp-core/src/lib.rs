//! anime-comp core library.
//!
//! * [`source`] — frame-accurate random access to video frames (ffms2)
//! * [`convert`] — YUV → RGB conversion (zimg) and PNG output
//! * [`select`] — frame selection on the reference timeline
//! * [`comp`] — end-to-end comparison runner

pub mod comp;
pub mod convert;
pub mod error;
pub mod select;
pub mod source;

pub use error::{Error, Result};
