//! OS adapters collect evidence; portable application services decide how to publish it.
#[cfg(target_os = "linux")]
pub(crate) mod linux;
