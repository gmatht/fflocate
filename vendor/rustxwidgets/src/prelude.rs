pub use crate::core::{App, DrawContext, Error, HandlerId, Widget};
#[cfg(all(feature = "gtk4-rs", target_os = "linux", not(feature = "pancurses"), not(feature = "zork")))]
pub use crate::common::{Window, WidgetBox, Label, Entry, Canvas, Menu, MenuBar, SimpleAction, Dialog, Orientation};
#[cfg(all(feature = "gtk4-rs", target_os = "linux", not(feature = "pancurses"), not(feature = "zork")))]
pub use crate::backends_gtk_adapter::{Button, Grid, DropDown, CheckButton, RadioButton, TextView, Overlay, Spreadsheet, ScrolledWindow};
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "pancurses"), not(feature = "zork"), not(feature = "gtk4-rs")))]
pub use crate::common::{Window, WidgetBox, Label, Entry, Canvas, Menu, MenuBar, SimpleAction, Dialog, Orientation};
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "pancurses"), not(feature = "zork"), not(feature = "gtk4-rs")))]
pub use crate::backends_gtk_adapter::{Button, Grid, DropDown, CheckButton, RadioButton, TextView, Overlay, Spreadsheet, ScrolledWindow};
#[cfg(all(windows, not(feature = "pancurses"), not(feature = "zork")))]
pub use crate::common::{Window, WidgetBox, Label, Entry, Canvas, Menu, MenuBar, SimpleAction, Dialog, Orientation};
#[cfg(all(windows, not(feature = "pancurses"), not(feature = "zork")))]
pub use crate::backends_nwg_adapter::{Button, Grid, DropDown, CheckButton, RadioButton, TextView, Appendable};
#[cfg(all(target_arch = "wasm32", not(feature = "pancurses"), not(feature = "zork")))]
pub use crate::common::{Window, WidgetBox, Label, Entry, Canvas, Menu, MenuBar, SimpleAction, Dialog, Orientation};
#[cfg(all(target_arch = "wasm32", not(feature = "pancurses"), not(feature = "zork")))]
pub use crate::backends_wasm_adapter::{Button, Grid, DropDown, CheckButton, RadioButton, TextView, Overlay, ScrolledWindow};
#[cfg(all(target_os = "android", not(feature = "zork")))]
pub use crate::common::{Window, WidgetBox, Label, Entry, Canvas, Menu, MenuBar, SimpleAction, Dialog, Orientation};
#[cfg(all(target_os = "android", not(feature = "zork")))]
pub use crate::backends_android_adapter::{Button, Grid, DropDown, CheckButton, RadioButton, Dialog as AndroidDialog, TextView, Overlay, ScrolledWindow};
// iOS / macOS: the same shape as android above — the common wrappers, and the
// adapter-local widget types that are not part of the common set. Without these
// arms the prelude exported nothing on Apple, so `rswidgets::prelude::DropDown`
// (and friends) did not resolve in a `gui-mobile` / `gui-macos` build.
#[cfg(all(target_os = "ios", not(feature = "pancurses"), not(feature = "zork")))]
pub use crate::common::{Window, WidgetBox, Label, Entry, Canvas, Menu, MenuBar, SimpleAction, Dialog, Orientation, ScrolledWindow};
#[cfg(all(target_os = "ios", not(feature = "pancurses"), not(feature = "zork")))]
pub use crate::backends_ios_adapter::{Button, Grid, DropDown, CheckButton, RadioButton, TextView, Overlay};
#[cfg(all(target_os = "macos", not(feature = "pancurses"), not(feature = "zork")))]
pub use crate::common::{Window, WidgetBox, Label, Entry, Canvas, Menu, MenuBar, SimpleAction, Dialog, Orientation, ScrolledWindow};
#[cfg(all(target_os = "macos", not(feature = "pancurses"), not(feature = "zork")))]
pub use crate::backends_macos_adapter::{Button, Grid, DropDown, CheckButton, RadioButton, TextView, Overlay};

#[cfg(feature = "pancurses")]
pub use crate::backends_pancurses_adapter::{Window, Button, Label, BoxWidget, Grid, Entry, Menu, MenuBar, SimpleAction, Dialog, DropDown, CheckButton, RadioButton, TextView, Orientation, Spreadsheet};
#[cfg(feature = "pancurses")]
pub use crate::backends::pancurses::set_frame_hook;
#[cfg(feature = "zork")]
pub use crate::backends_zork_adapter::{Window, Button, Label, BoxWidget, Grid, Entry, Menu, MenuBar, SimpleAction, Dialog, DropDown, CheckButton, RadioButton, TextView, Orientation};
