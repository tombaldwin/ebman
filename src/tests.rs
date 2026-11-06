//! Test support for the whole crate: `scan` reads the source tree for
//! the drift guards, `packaging` holds what may be published. Compiled
//! only under `cfg(test)`.

pub(crate) mod packaging;
pub(crate) mod scan;
