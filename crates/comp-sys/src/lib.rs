//! Raw, unsafe FFI bindings to ffms2 and zimg. Use `comp-core` for the safe API.
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code, clippy::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

/// zimg's API version macro, which bindgen can't evaluate.
pub const ZIMG_API_VERSION: u32 = (ZIMG_API_VERSION_MAJOR << 8) | ZIMG_API_VERSION_MINOR;

/// `ZIMG_BUFFER_MAX` (`(unsigned)-1`): marks a fully allocated plane.
pub const ZIMG_BUFFER_MAX: u32 = u32::MAX;
