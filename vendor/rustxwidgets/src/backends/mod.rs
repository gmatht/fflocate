use std::error::Error as StdError;

/// Type alias for boxed backend errors
pub type BackendError = Box<dyn StdError + Send + Sync>;

/// Backend application abstraction. Concrete backends provide an implementor boxed via `init()`.
pub trait BackendApp {
    /// Run the backend main loop. Consumes the backend app.
    fn run(self: Box<Self>) -> Result<(), BackendError>;
}

/// Priority chain: each backend module is always compiled when its feature is on,
/// but `init` is re-exported only for the highest-priority backend available.
/// Platform-specific backends (gtk, nwg, wasm, android) naturally exclude each
/// other. Pancurses is a fallback when no platform-native backend applies.

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
pub mod gtk;
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
pub use self::gtk::init;

#[cfg(all(windows, not(feature = "zork")))]
pub mod nwg;
#[cfg(all(windows, not(feature = "zork")))]
pub use self::nwg::init;

#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
pub mod wasm;
#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
pub use self::wasm::init;

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub mod android;
#[cfg(all(target_os = "android", not(feature = "zork")))]
pub use self::android::init_backend as init;

/// pancurses is a cross-platform fallback; its `init` is always available
/// at `backends::pancurses::init()` when compiled, and also re-exported as
/// `init_pancurses` for combined-backend builds.
#[cfg(feature = "pancurses")]
pub mod pancurses;
#[cfg(feature = "pancurses")]
pub use self::pancurses::init as init_pancurses;

#[cfg(feature = "zork")]
pub mod zork;
#[cfg(feature = "zork")]
pub use self::zork::init;
