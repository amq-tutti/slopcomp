//! anime-comp core library.
//!
//! * [`source`] — frame-accurate random access to video frames (ffms2)
//! * [`convert`] — YUV → RGB conversion (zimg) and PNG output

pub mod convert;
pub mod error;
pub mod source;

pub use error::{Error, Result};
