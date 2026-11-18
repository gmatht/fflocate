// Re-export all adapter functions and types.
// Using glob to stay in sync with backends_gtk_adapter_impl automatically.
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
pub use crate::backends_gtk_adapter_impl::*;
