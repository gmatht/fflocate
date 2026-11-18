use crate::error::Error;
use crate::loader::RawLib as Library;
use std::ffi::c_void;

/// Every `char*` in these signatures is spelled `*const u8` / `*mut u8`.
///
/// The C type is `const char*` / `char*`, whose Rust spelling is
/// `*const c_char` / `*mut c_char` — and `c_char` is **platform-dependent**:
/// `i8` on x86 and every ARM, `u8` on PowerPC/s390x/other BSDs. Writing `i8`
/// out by hand made this crate fail to typecheck on exactly those platforms
/// (76 errors on `aarch64-linux-android`), while compiling fine on
/// `x86_64-linux-android` because there `c_char` happens to be `u8` — the
/// emulator ABI hid the bug and a real device would not.
///
/// `u8` is the right choice for two reasons, and the second is the important
/// one:
///
/// 1. It is the type of `CString::as_ptr()` and `CStr::as_ptr()`
///    unconditionally, so every call site here typechecks on every target
///    without a cast.
/// 2. It does not actually change the ABI. These are `extern "C"` function
///    pointers, so the *pointed-to* element type is not part of the calling
///    convention — a pointer to a signed byte and a pointer to an unsigned byte
///    are passed in the same register on every ABI. Only the pointee's
///    alignment and signedness differ, and C string literals are never
///    dereferenced through a signedness-sensitive operation here.
///
/// The alternative, `*const c_char`, would be the pedantically correct
/// spelling but would need a `as *const c_char` at each of the ~60 call
/// sites, and would be wrong on the targets where `c_char` is not `u8`
/// anyway. So: `u8`, with this comment as the record of why.
pub type GMainLoopNew = unsafe extern "C" fn(context: *mut c_void, is_running: i32) -> *mut c_void;
pub type GMainLoopRun = unsafe extern "C" fn(loop_: *mut c_void);
pub type GMainLoopQuit = unsafe extern "C" fn(loop_: *mut c_void);

pub type GObjectRef = unsafe extern "C" fn(obj: *mut c_void) -> *mut c_void;
pub type GObjectUnref = unsafe extern "C" fn(obj: *mut c_void);
pub type GObjectRefSink = unsafe extern "C" fn(obj: *mut c_void) -> *mut c_void;

pub type GSignalConnectData = unsafe extern "C" fn(instance: *mut c_void, detailed_signal: *const u8, c_handler: *mut c_void, data: *mut c_void, destroy_data: Option<unsafe extern "C" fn(data: *mut c_void, closure: *mut c_void)>, connect_flags: u32) -> u64;
pub type GSignalConnect = unsafe extern "C" fn(instance: *mut c_void, detailed_signal: *const u8, c_handler: *mut c_void, data: *mut c_void) -> u64;
pub type GSignalEmitByName = unsafe extern "C" fn(instance: *mut c_void, detailed_signal: *const u8) -> u64;
/// `g_signal_emit_by_name` for `GtkEventControllerKey::key-pressed` /
/// `key-released`, whose C signature is (keyval, keycode, state) -> bool.
/// The symbol itself is variadic, so the same pointer is reinterpreted with
/// the three-argument shape rather than declared twice.
pub type GSignalEmitKeyByName = unsafe extern "C" fn(
    instance: *mut c_void,
    detailed_signal: *const u8,
    keyval: u32,
    keycode: u32,
    state: u32,
) -> u64;

pub type GtkWindowNew = unsafe extern "C" fn(window_type: i32) -> *mut c_void;
pub type GtkWindowSetTitle = unsafe extern "C" fn(window: *mut c_void, title: *const u8);
pub type GtkButtonNewWithLabel = unsafe extern "C" fn(label: *const u8) -> *mut c_void;
pub type GtkLabelNew = unsafe extern "C" fn(str: *const u8) -> *mut c_void;
pub type GtkLabelSetText = unsafe extern "C" fn(label: *mut c_void, str: *const u8);
pub type GtkLabelGetText = unsafe extern "C" fn(label: *mut c_void) -> *const u8;
pub type GtkBoxNew = unsafe extern "C" fn(orientation: i32, spacing: i32) -> *mut c_void;

pub type GtkBoxAppend = unsafe extern "C" fn(box_: *mut c_void, child: *mut c_void);
pub type GtkBoxPackStart = unsafe extern "C" fn(box_: *mut c_void, child: *mut c_void, expand: i32, fill: i32, padding: u32);
pub type GtkContainerAdd = unsafe extern "C" fn(container: *mut c_void, widget: *mut c_void);
pub type GtkContainerRemove = unsafe extern "C" fn(container: *mut c_void, widget: *mut c_void);
pub type GtkWindowSetChild = unsafe extern "C" fn(window: *mut c_void, child: *mut c_void);
/// `void gtk_widget_show(GtkWidget*)` (GTK3; in GTK4 this is deprecated but
/// still present as a no-op, so calling it is safe on both).
pub type GtkWidgetShow = unsafe extern "C" fn(widget: *mut c_void);
pub type GtkWidgetHide = unsafe extern "C" fn(widget: *mut c_void);
pub type GtkWidgetShowAll = unsafe extern "C" fn(widget: *mut c_void);
pub type GtkWindowPresent = unsafe extern "C" fn(window: *mut c_void);
pub type GtkInit = unsafe extern "C" fn(argc: *mut libc::c_int, argv: *mut *mut *mut libc::c_char);
pub type GtkLabelSetMarkup = unsafe extern "C" fn(label: *mut c_void, markup: *const u8);
pub type GtkWidgetSetVisible = unsafe extern "C" fn(widget: *mut c_void, visible: i32);
pub type GtkWidgetGrabFocus = unsafe extern "C" fn(widget: *mut c_void);
pub type GtkWidgetHasFocus = unsafe extern "C" fn(widget: *mut c_void) -> i32;
pub type GtkWidgetGetStyleContext = unsafe extern "C" fn(widget: *mut c_void) -> *mut c_void;
pub type GtkStyleContextAddClass = unsafe extern "C" fn(context: *mut c_void, class_name: *const u8);
pub type GtkStyleContextRemoveClass = unsafe extern "C" fn(context: *mut c_void, class_name: *const u8);
pub type GtkCssProviderNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkCssProviderLoadFromData = unsafe extern "C" fn(provider: *mut c_void, data: *const u8, length: isize, error: *mut *mut c_void) -> i32;
pub type GtkStyleContextAddProvider = unsafe extern "C" fn(context: *mut c_void, provider: *mut c_void, priority: u32);
// GApplication / GMenu / Actions (gio)
pub type GtkApplicationNew = unsafe extern "C" fn(application_id: *const u8, flags: u32) -> *mut c_void;
pub type GApplicationRun = unsafe extern "C" fn(application: *mut c_void, argc: i32, argv: *mut *mut u8) -> i32;
pub type GApplicationRegister = unsafe extern "C" fn(application: *mut c_void, cancellable: *mut c_void, error: *mut *mut c_void) -> i32;
pub type GSimpleActionNew = unsafe extern "C" fn(name: *const u8, parameter_type: *mut c_void) -> *mut c_void;
pub type GSimpleActionGroupNew = unsafe extern "C" fn() -> *mut c_void;
pub type GActionMapAddAction = unsafe extern "C" fn(map: *mut c_void, action: *mut c_void);
pub type GActionGroupActivateAction = unsafe extern "C" fn(group: *mut c_void, action_name: *const u8, parameter: *mut c_void);
pub type GActionMapLookupAction = unsafe extern "C" fn(map: *mut c_void, action_name: *const u8) -> *mut c_void;
pub type GActionActivate = unsafe extern "C" fn(action: *mut c_void, parameter: *mut c_void);
pub type GMenuNew = unsafe extern "C" fn() -> *mut c_void;
pub type GMenuAppend = unsafe extern "C" fn(menu: *mut c_void, label: *const u8, detailed_action: *const u8);
pub type GApplicationSetAppMenu = unsafe extern "C" fn(application: *mut c_void, menu: *mut c_void);
pub type GApplicationSetMenubar = unsafe extern "C" fn(application: *mut c_void, menu: *mut c_void);
pub type GMenuAppendSubmenu = unsafe extern "C" fn(menu: *mut c_void, label: *const u8, submenu: *mut c_void);
pub type GtkPopoverMenuBarNewFromModel = unsafe extern "C" fn(model: *mut c_void) -> *mut c_void;
// GTK3 menu bar
pub type GtkMenuBarNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkMenuNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkMenuItemNewWithLabel = unsafe extern "C" fn(label: *const u8) -> *mut c_void;
pub type GtkMenuItemNewWithMnemonic = unsafe extern "C" fn(label: *const u8) -> *mut c_void;
pub type GtkMenuShellAppend = unsafe extern "C" fn(shell: *mut c_void, child: *mut c_void);
pub type GtkMenuItemSetSubmenu = unsafe extern "C" fn(item: *mut c_void, submenu: *mut c_void);
pub type GtkWindowSetApplication = unsafe extern "C" fn(window: *mut c_void, application: *mut c_void);
// Widget action group resolution (needed for GtkPopoverMenuBar to find actions)
pub type GtkWidgetInsertActionGroup = unsafe extern "C" fn(widget: *mut c_void, name: *const u8, group: *mut c_void);
// GtkActionable (for setting action names on GTK3 menu items)
pub type GtkActionableSetDetailedActionName = unsafe extern "C" fn(actionable: *mut c_void, detailed_action_name: *const u8);
// GMenuModel iteration (for GTK3 fallback)
pub type GMenuModelGetNItems = unsafe extern "C" fn(model: *mut c_void) -> i32;
pub type GMenuModelGetItemAttributeValue = unsafe extern "C" fn(model: *mut c_void, item_index: i32, attribute: *const u8, expected_type: *const c_void) -> *mut c_void;
pub type GMenuModelGetItemLink = unsafe extern "C" fn(model: *mut c_void, item_index: i32, link: *const u8) -> *mut c_void;
pub type GVariantGetString = unsafe extern "C" fn(value: *mut c_void, length: *mut usize) -> *const u8;
pub type GVariantUnref = unsafe extern "C" fn(value: *mut c_void);

// File chooser / native dialog
pub type GtkFileChooserNativeNew = unsafe extern "C" fn(title: *const u8, parent: *mut c_void, action: i32, accept_label: *const u8, cancel_label: *const u8) -> *mut c_void;
pub type GtkNativeDialogRun = unsafe extern "C" fn(native: *mut c_void) -> i32;
pub type GtkFileChooserGetFilename = unsafe extern "C" fn(chooser: *mut c_void) -> *const u8;
// File filters (GtkFileFilter + GtkFileChooser filter API, stable across
// GTK 3.20+ and GTK 4): optional best-effort enhancement for save dialogs.
pub type GtkFileFilterNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkFileFilterSetName = unsafe extern "C" fn(filter: *mut c_void, name: *const u8);
pub type GtkFileFilterAddPattern = unsafe extern "C" fn(filter: *mut c_void, pattern: *const u8);
pub type GtkFileChooserAddFilter = unsafe extern "C" fn(chooser: *mut c_void, filter: *mut c_void);
pub type GtkFileChooserSetFilter = unsafe extern "C" fn(chooser: *mut c_void, filter: *mut c_void);
pub type GtkFileChooserSetCurrentName = unsafe extern "C" fn(chooser: *mut c_void, name: *const u8) -> i32;
pub type GtkWidgetDestroy = unsafe extern "C" fn(widget: *mut c_void);
pub type GtkWindowClose = unsafe extern "C" fn(window: *mut c_void);
pub type GFree = unsafe extern "C" fn(ptr: *mut c_void);
// gdk event helpers
pub type GdkEventGetKeyval = unsafe extern "C" fn(event: *mut c_void, keyval: *mut u32) -> i32;
pub type GdkEventGetState = unsafe extern "C" fn(event: *mut c_void, state: *mut u32);
// GdkEventType discriminator: GDK_KEY_PRESS = 8, GDK_KEY_RELEASE = 9
// (stable across GTK3/GTK4). Needed because the unfiltered "event" signal
// also delivers releases, which are indistinguishable downstream.
pub type GdkEventGetEventType = unsafe extern "C" fn(event: *const c_void) -> i32;
pub type GdkKeyvalFromName = unsafe extern "C" fn(name: *const u8) -> u32;
pub type GdkDisplayGetDefault = unsafe extern "C" fn() -> *mut c_void;
pub type GdkScreenGetDefault = unsafe extern "C" fn() -> *mut c_void;
pub type GtkStyleContextAddProviderForDisplay = unsafe extern "C" fn(display: *mut c_void, provider: *mut c_void, priority: u32);
pub type GtkStyleContextAddProviderForScreen = unsafe extern "C" fn(screen: *mut c_void, provider: *mut c_void, priority: u32);
pub type GtkLabelSetXalign = unsafe extern "C" fn(label: *mut c_void, xalign: f32);
pub type GtkEventControllerKeyNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkEventControllerFocusNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkWidgetAddController = unsafe extern "C" fn(widget: *mut c_void, controller: *mut c_void);
pub type GtkEventControllerSetPropagationPhase = unsafe extern "C" fn(controller: *mut c_void, phase: u32);
pub type GtkScrolledWindowGetVadjustment = unsafe extern "C" fn(sw: *mut c_void) -> *mut c_void;
pub type GtkScrolledWindowGetHadjustment = unsafe extern "C" fn(sw: *mut c_void) -> *mut c_void;
pub type GtkAdjustmentGetValue = unsafe extern "C" fn(adj: *mut c_void) -> f64;
// Atomically configure a GtkAdjustment (value/lower/upper/step/page). One
// call keeps scrollbar range, thumb size and position consistent.
pub type GtkAdjustmentConfigure = unsafe extern "C" fn(adj: *mut c_void, value: f64, lower: f64, upper: f64, step: f64, page: f64, page_size: f64);
pub type GtkGestureClickNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkWidgetSetCanTarget = unsafe extern "C" fn(widget: *mut c_void, can_target: i32);
pub type GtkWidgetSetHalign = unsafe extern "C" fn(widget: *mut c_void, align: i32);
pub type GtkWidgetSetValign = unsafe extern "C" fn(widget: *mut c_void, align: i32);
/// `gint gtk_widget_get_width_request(GtkWidget*)`.
pub type GtkWidgetGetWidthRequest = unsafe extern "C" fn(widget: *mut c_void) -> i32;
pub type GtkWidgetGetAllocatedWidth = unsafe extern "C" fn(widget: *mut c_void) -> i32;
pub type GtkWidgetGetAllocatedHeight = unsafe extern "C" fn(widget: *mut c_void) -> i32;
pub type GtkWidgetGetMapped = unsafe extern "C" fn(widget: *mut c_void) -> i32;
pub type GtkWidgetGetDisplay = unsafe extern "C" fn(widget: *mut c_void) -> *mut c_void;

// GtkPopoverMenuBarItem — get the popover from a bar item
pub type GtkPopoverMenuBarItemGetPopover = unsafe extern "C" fn(item: *mut c_void) -> *mut c_void;
// GtkPopover — get/set child widget
pub type GtkPopoverGetChild = unsafe extern "C" fn(popover: *mut c_void) -> *mut c_void;
pub type GtkPopoverNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkPopoverSetChild = unsafe extern "C" fn(popover: *mut c_void, child: *mut c_void);

// GtkMenuButton — for manual menu bar construction
pub type GtkMenuButtonNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkMenuButtonSetPopover = unsafe extern "C" fn(button: *mut c_void, popover: *mut c_void);
pub type GtkMenuButtonSetLabel = unsafe extern "C" fn(button: *mut c_void, label: *const u8);
pub type GtkMenuButtonGetPopover = unsafe extern "C" fn(button: *mut c_void) -> *mut c_void;
pub type GtkMenuButtonSetHasFrame = unsafe extern "C" fn(button: *mut c_void, has_frame: i32);
pub type GtkMenuButtonSetPrimary = unsafe extern "C" fn(button: *mut c_void, primary: i32);
pub type GtkButtonSetHasFrame = unsafe extern "C" fn(button: *mut c_void, has_frame: i32);
pub type GtkWidgetAddCssClass = unsafe extern "C" fn(widget: *mut c_void, css_class: *const u8);



// GtkPopoverMenu — new from model (submenu-level)
pub type GtkPopoverMenuNewFromModel = unsafe extern "C" fn(model: *mut c_void) -> *mut c_void;

// Display sync — flushes pending display requests and waits for server round-trip
pub type GdkDisplaySync = unsafe extern "C" fn(display: *mut c_void);

// Dialog
pub type GtkDialogNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkDialogAddButton = unsafe extern "C" fn(dialog: *mut c_void, button_text: *const u8, response_id: i32) -> *mut c_void;
pub type GtkDialogSetDefaultResponse = unsafe extern "C" fn(dialog: *mut c_void, response_id: i32);
pub type GtkDialogGetContentArea = unsafe extern "C" fn(dialog: *mut c_void) -> *mut c_void;
pub type GtkDialogRun = unsafe extern "C" fn(dialog: *mut c_void) -> i32;
// Dropdown - GTK3 ComboBoxText
pub type GtkComboBoxTextNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkComboBoxTextAppendText = unsafe extern "C" fn(combo: *mut c_void, text: *const u8);
pub type GtkComboBoxTextGetActiveText = unsafe extern "C" fn(combo: *mut c_void) -> *const u8;
pub type GtkComboBoxSetActive = unsafe extern "C" fn(combo: *mut c_void, index_: i32);
pub type GtkComboBoxGetActive = unsafe extern "C" fn(combo: *mut c_void) -> i32;

// Dropdown - GTK4 DropDown
pub type GtkDropDownNew = unsafe extern "C" fn(model: *mut c_void, expression: *mut c_void) -> *mut c_void;
pub type GtkDropDownSetSelected = unsafe extern "C" fn(dropdown: *mut c_void, selected: u32);
pub type GtkDropDownGetSelected = unsafe extern "C" fn(dropdown: *mut c_void) -> u32;
pub type GtkStringListNew = unsafe extern "C" fn(strings: *const *const u8) -> *mut c_void;

// Checkbox / CheckButton
pub type GtkCheckButtonNewWithLabel = unsafe extern "C" fn(label: *const u8) -> *mut c_void;
pub type GtkCheckButtonGetActive = unsafe extern "C" fn(check_button: *mut c_void) -> i32;
pub type GtkCheckButtonSetActive = unsafe extern "C" fn(check_button: *mut c_void, is_active: i32);
pub type GtkCheckButtonSetGroup = unsafe extern "C" fn(check_button: *mut c_void, group: *mut c_void);
pub type GtkToggleButtonGetActive = unsafe extern "C" fn(toggle_button: *mut c_void) -> i32;
pub type GtkToggleButtonSetActive = unsafe extern "C" fn(toggle_button: *mut c_void, is_active: i32);

// RadioButton
pub type GtkRadioButtonNewWithLabel = unsafe extern "C" fn(group: *mut c_void, label: *const u8) -> *mut c_void;
// Returns the GSList* group of a radio button (NOT a widget pointer: passing
// a widget as the ctor's `group` arg segfaults inside GTK).
pub type GtkRadioButtonGetGroup = unsafe extern "C" fn(widget: *mut c_void) -> *mut c_void;

// TextView / TextArea
pub type GtkTextViewNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkTextBufferNew = unsafe extern "C" fn(table: *mut c_void) -> *mut c_void;
pub type GtkTextViewGetBuffer = unsafe extern "C" fn(text_view: *mut c_void) -> *mut c_void;
pub type GtkTextBufferSetText = unsafe extern "C" fn(buffer: *mut c_void, text: *const u8, len: i32);
pub type GtkTextBufferGetText = unsafe extern "C" fn(buffer: *mut c_void, start: *mut c_void, end: *mut c_void, include_hidden_chars: i32) -> *mut c_void;
pub type GtkTextBufferGetStartIter = unsafe extern "C" fn(buffer: *mut c_void, iter: *mut c_void);
pub type GtkTextBufferGetEndIter = unsafe extern "C" fn(buffer: *mut c_void, iter: *mut c_void);
/// `gtk_text_buffer_insert_at_cursor(buffer, iter, text, len)`; the iterator
/// is honoured, which is how `TextView::append_text` targets the end.
pub type GtkTextBufferInsertAtCursor = unsafe extern "C" fn(buffer: *mut c_void, iter: *mut c_void, text: *const u8, len: i32);
pub type GtkTextIterCopy = unsafe extern "C" fn(iter: *mut c_void) -> *mut c_void;
pub type GtkTextIterFree = unsafe extern "C" fn(iter: *mut c_void);
pub type GtkTextViewSetWrapMode = unsafe extern "C" fn(text_view: *mut c_void, wrap_mode: i32);

// GtkWidget helper for visibility/event handling
pub type GtkWidgetSetCanFocus = unsafe extern "C" fn(widget: *mut c_void, can_focus: i32);
pub type GtkWidgetSetHexpand = unsafe extern "C" fn(widget: *mut c_void, expand: i32);
pub type GtkWidgetSetVexpand = unsafe extern "C" fn(widget: *mut c_void, expand: i32);
pub type GtkWidgetGetHexpand = unsafe extern "C" fn(widget: *mut c_void) -> i32;
pub type GtkWidgetGetVexpand = unsafe extern "C" fn(widget: *mut c_void) -> i32;

// GtkEditable (GTK4 replacement for gtk_entry_get_text/set_text)
pub type GtkEditableGetText = unsafe extern "C" fn(editable: *mut c_void) -> *const u8;
pub type GtkEditableSetText = unsafe extern "C" fn(editable: *mut c_void, text: *const u8);
pub type GtkEditableSetPosition = unsafe extern "C" fn(editable: *mut c_void, position: i32);
/// `int gtk_editable_get_position(GtkEditable*)` (GTK3/GTK4).
pub type GtkEditableGetPosition = unsafe extern "C" fn(editable: *mut c_void) -> i32;

// GtkWidget parent handling
pub type GtkWidgetUnparent = unsafe extern "C" fn(widget: *mut c_void);
pub type GtkWidgetGetParent = unsafe extern "C" fn(widget: *mut c_void) -> *mut c_void;

// GtkWidget tree traversal
/// `GList* gtk_container_get_children(GtkContainer*)` (GTK3). The returned list
/// is owned by the caller and must be freed with `g_list_free`.
pub type GtkContainerGetChildren = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
/// `GtkWidget* gtk_menu_item_get_submenu(GtkMenuItem*)`.
pub type GtkMenuItemGetSubmenu = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
/// `gboolean gtk_widget_is_visible(GtkWidget*)`.
pub type GtkWidgetIsVisible = unsafe extern "C" fn(*mut c_void) -> i32;
/// `void g_list_free(GList*)`.
pub type GListFree = unsafe extern "C" fn(*mut GListC);
/// `void gtk_container_foreach(GtkContainer*, GtkCallback, gpointer)`.
pub type GtkContainerForeach =
    unsafe extern "C" fn(*mut c_void, unsafe extern "C" fn(*mut c_void, *mut c_void), *mut c_void);
/// `void gtk_menu_item_activate(GtkMenuItem*)`.
pub type GtkMenuItemActivate = unsafe extern "C" fn(*mut c_void);
/// `gboolean gtk_widget_translate_coordinates(GtkWidget *src, GtkWidget *dest,
/// gint src_x, gint src_y, gint *dest_x, gint *dest_y)`.
pub type GtkWidgetTranslateCoordinates = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    i32,
    i32,
    *mut i32,
    *mut i32,
) -> i32;
/// `GdkWindow *gtk_widget_get_window(GtkWidget*)`.
pub type GtkWidgetGetWindow = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
/// `gint gdk_window_get_origin(GdkWindow*, gint *x, gint *y)` — window origin
/// in root/screen coordinates.
pub type GdkWindowGetOrigin = unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> i32;

/// `void gtk_menu_popup(GtkMenu*, GtkWidget*, GtkWidget*, GtkMenuPositionFunc,
/// gpointer, guint, guint32)`.
pub type GtkMenuPopup = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    u32,
    u32,
);
/// `GList`: `{ gpointer data; GList *next; GList *prev; }`.
#[repr(C)]
pub struct GListC {
    pub data: *mut c_void,
    pub next: *mut GListC,
    pub prev: *mut GListC,
}
pub type GtkWidgetGetFirstChild = unsafe extern "C" fn(widget: *mut c_void) -> *mut c_void;
pub type GtkWidgetGetNextSibling = unsafe extern "C" fn(widget: *mut c_void) -> *mut c_void;

// GtkWidget activate (triggers the default action, e.g. opening a submenu popover)
pub type GtkWidgetActivate = unsafe extern "C" fn(widget: *mut c_void);

// GtkWidget get_visible — checks the widget's visible flag
pub type GtkWidgetGetVisible = unsafe extern "C" fn(widget: *mut c_void) -> i32;

// GtkWidget activate_action — activates a named action on a widget
pub type GtkWidgetActivateAction = unsafe extern "C" fn(widget: *mut c_void, detailed_name: *const u8, parameter: *mut c_void);

// GtkMenuButton (GTK4 submenu opener)
pub type GtkMenuButtonGetLabel = unsafe extern "C" fn(button: *mut c_void) -> *const u8;
pub type GtkMenuButtonSetActive = unsafe extern "C" fn(button: *mut c_void, active: i32);

// GtkWindow default size
/// `void gtk_window_set_transient_for(GtkWindow*, GtkWindow*)`.
pub type GtkWindowSetTransientFor = unsafe extern "C" fn(window: *mut c_void, parent: *mut c_void);
/// `void gtk_window_set_position(GtkWindow*, GtkWindowPosition)`.
pub type GtkWindowSetPosition = unsafe extern "C" fn(window: *mut c_void, position: i32);
pub type GtkWindowSetDefaultSize = unsafe extern "C" fn(window: *mut c_void, width: i32, height: i32);
pub type GtkWindowResize = unsafe extern "C" fn(window: *mut c_void, width: i32, height: i32);
pub type GtkWindowMove = unsafe extern "C" fn(window: *mut c_void, x: i32, y: i32);

// Grid/Entry related
pub type GtkGridNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkGridAttach = unsafe extern "C" fn(grid: *mut c_void, child: *mut c_void, left: i32, top: i32, width: i32, height: i32);
pub type GtkEntryNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkEntrySetText = unsafe extern "C" fn(entry: *mut c_void, text: *const u8);
pub type GtkEntryGetText = unsafe extern "C" fn(entry: *mut c_void) -> *const u8;
pub type GtkEntrySetWidthChars = unsafe extern "C" fn(entry: *mut c_void, n_chars: i32);
pub type GtkWidgetSetSizeRequest = unsafe extern "C" fn(widget: *mut c_void, width: i32, height: i32);
pub type GtkEntrySetHasFrame = unsafe extern "C" fn(entry: *mut c_void, has_frame: i32);
pub type GtkEntrySetEditable = unsafe extern "C" fn(entry: *mut c_void, editable: i32);
pub type GtkDrawingAreaNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkOverlayNew = unsafe extern "C" fn() -> *mut c_void;
pub type GtkOverlayAddOverlay = unsafe extern "C" fn(overlay: *mut c_void, widget: *mut c_void);
pub type GtkOverlaySetOverlayPassThrough = unsafe extern "C" fn(overlay: *mut c_void, widget: *mut c_void, pass_through: i32);
pub type GtkOverlaySetChild = unsafe extern "C" fn(overlay: *mut c_void, child: *mut c_void);
pub type GtkWidgetSetMarginStart = unsafe extern "C" fn(widget: *mut c_void, margin: i32);
/// `GtkWidget* gtk_fixed_new(void)`.
pub type GtkFixedNew = unsafe extern "C" fn() -> *mut c_void;
/// `void gtk_fixed_put(GtkFixed*, GtkWidget*, gint x, gint y)`.
pub type GtkFixedPut = unsafe extern "C" fn(fixed: *mut c_void, child: *mut c_void, x: i32, y: i32);
/// `void gtk_fixed_move(GtkFixed*, GtkWidget*, gint x, gint y)`.
pub type GtkFixedMove = unsafe extern "C" fn(fixed: *mut c_void, child: *mut c_void, x: i32, y: i32);
pub type GtkWidgetSetMarginTop = unsafe extern "C" fn(widget: *mut c_void, margin: i32);
pub type GtkWidgetAddEvents = unsafe extern "C" fn(widget: *mut c_void, events: i32);

// ScrolledWindow
pub type GtkScrolledWindowNew = unsafe extern "C" fn(hadjustment: *mut c_void, vadjustment: *mut c_void) -> *mut c_void;
pub type GtkScrolledWindowSetOverlayScrolling = unsafe extern "C" fn(sw: *mut c_void, overlay: i32);
pub type GtkScrolledWindowSetPolicy = unsafe extern "C" fn(scrolled: *mut c_void, h_policy: u32, v_policy: u32);
pub type GtkScrolledWindowSetChild = unsafe extern "C" fn(scrolled: *mut c_void, child: *mut c_void);

// DrawingArea canvas
pub type GtkDrawingAreaSetDrawFunc = unsafe extern "C" fn(area: *mut c_void, draw_func: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, i32, i32, *mut c_void)>, user_data: *mut c_void, destroy: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>);
pub type GtkDrawingAreaSetContentWidth = unsafe extern "C" fn(area: *mut c_void, width: i32);
pub type GtkDrawingAreaSetContentHeight = unsafe extern "C" fn(area: *mut c_void, height: i32);

// Frame clock (GTK4's display-synchronized animation clock)
pub type GtkWidgetGetFrameClock = unsafe extern "C" fn(widget: *mut c_void) -> *mut c_void;
pub type GdkFrameClockRequestPhase = unsafe extern "C" fn(clock: *mut c_void, phase: u32);
pub type GdkFrameClockBeginUpdating = unsafe extern "C" fn(clock: *mut c_void);
pub type GdkFrameClockEndUpdating = unsafe extern "C" fn(clock: *mut c_void);

// Cairo additions
pub type CairoTextExtents = unsafe extern "C" fn(cr: *mut c_void, utf8: *const u8, extents: *mut c_void);
pub type CairoSave = unsafe extern "C" fn(cr: *mut c_void);
pub type CairoRestore = unsafe extern "C" fn(cr: *mut c_void);
pub type CairoClip = unsafe extern "C" fn(cr: *mut c_void);

// Cairo line drawing
pub type CairoLineTo = unsafe extern "C" fn(cr: *mut c_void, x: f64, y: f64);
pub type CairoPaint = unsafe extern "C" fn(cr: *mut c_void);

// GdkSurface — for direct rendering bypassing frame clock
pub type GtkNativeGetSurface = unsafe extern "C" fn(native: *mut c_void) -> *mut c_void;
pub type GdkSurfaceCreateCairoContext = unsafe extern "C" fn(surface: *mut c_void) -> *mut c_void;
pub type GdkSurfaceGetWidth = unsafe extern "C" fn(surface: *mut c_void) -> i32;

// GdkSurface begin/end draw frame (GTK 4.0-4.14, deprecated in 4.14+)
pub type GdkSurfaceBeginDrawFrame = unsafe extern "C" fn(surface: *mut c_void, region: *mut c_void) -> *mut c_void;
pub type GdkSurfaceEndDrawFrame = unsafe extern "C" fn(surface: *mut c_void, context: *mut c_void);
pub type GdkDrawContextGetCairoContext = unsafe extern "C" fn(context: *mut c_void) -> *mut c_void;
pub type GdkSurfaceGetHeight = unsafe extern "C" fn(surface: *mut c_void) -> i32;

#[repr(C)]
pub struct CairoTextExtentsT {
    pub x_bearing: f64,
    pub y_bearing: f64,
    pub width: f64,
    pub height: f64,
    pub x_advance: f64,
    pub y_advance: f64,
}

pub struct Symbols {
    // glib
    pub g_main_loop_new: Option<GMainLoopNew>,
    pub g_main_loop_run: Option<GMainLoopRun>,
    pub g_main_loop_quit: Option<GMainLoopQuit>,

    // gobject
    pub g_object_ref: Option<GObjectRef>,
    pub g_object_unref: Option<GObjectUnref>,
    pub g_object_ref_sink: Option<GObjectRefSink>,
    pub g_signal_connect_data: Option<GSignalConnectData>,
    pub g_signal_connect: Option<GSignalConnect>,

    // gtk
    pub gtk_window_new: Option<GtkWindowNew>,
    pub gtk_window_set_title: Option<GtkWindowSetTitle>,
    pub gtk_button_new_with_label: Option<GtkButtonNewWithLabel>,
    pub gtk_label_new: Option<GtkLabelNew>,
    pub gtk_label_set_text: Option<GtkLabelSetText>,
    pub gtk_label_get_text: Option<GtkLabelGetText>,
    pub gtk_box_new: Option<GtkBoxNew>,
    pub gtk_box_append: Option<GtkBoxAppend>,
    pub gtk_box_pack_start: Option<GtkBoxPackStart>,
    pub gtk_container_add: Option<GtkContainerAdd>,
    pub gtk_container_remove: Option<GtkContainerRemove>,
    pub gtk_window_set_child: Option<GtkWindowSetChild>,
    pub gtk_widget_show_all: Option<GtkWidgetShowAll>,
    pub gtk_widget_show: Option<GtkWidgetShow>,
    pub gtk_widget_hide: Option<GtkWidgetHide>,
    pub gtk_window_present: Option<GtkWindowPresent>,
    pub gtk_window_set_application: Option<GtkWindowSetApplication>,
    pub gtk_widget_insert_action_group: Option<GtkWidgetInsertActionGroup>,
    pub gtk_actionable_set_detailed_action_name: Option<GtkActionableSetDetailedActionName>,
    pub gtk_grid_new: Option<GtkGridNew>,
    pub gtk_grid_attach: Option<GtkGridAttach>,
    pub gtk_entry_new: Option<GtkEntryNew>,
    pub gtk_entry_set_text: Option<GtkEntrySetText>,
    pub gtk_entry_get_text: Option<GtkEntryGetText>,
    pub gtk_entry_set_width_chars: Option<GtkEntrySetWidthChars>,
    pub gtk_widget_set_size_request: Option<GtkWidgetSetSizeRequest>,
    pub gtk_entry_set_has_frame: Option<GtkEntrySetHasFrame>,
    pub gtk_entry_set_editable: Option<GtkEntrySetEditable>,
    pub gtk_drawing_area_new: Option<GtkDrawingAreaNew>,
    pub gtk_label_set_markup: Option<GtkLabelSetMarkup>,
    pub gtk_widget_set_visible: Option<GtkWidgetSetVisible>,
    pub gtk_widget_grab_focus: Option<GtkWidgetGrabFocus>,
    pub gtk_widget_has_focus: Option<GtkWidgetHasFocus>,
    pub gtk_widget_get_style_context: Option<GtkWidgetGetStyleContext>,
    pub gtk_style_context_add_class: Option<GtkStyleContextAddClass>,
    pub gtk_style_context_remove_class: Option<GtkStyleContextRemoveClass>,
    pub gtk_css_provider_new: Option<GtkCssProviderNew>,
    pub gtk_css_provider_load_from_data: Option<GtkCssProviderLoadFromData>,
    pub gtk_style_context_add_provider: Option<GtkStyleContextAddProvider>,
    pub gtk_overlay_new: Option<GtkOverlayNew>,
    pub gtk_overlay_add_overlay: Option<GtkOverlayAddOverlay>,
    pub gtk_overlay_set_overlay_pass_through: Option<GtkOverlaySetOverlayPassThrough>,
    pub gtk_overlay_set_child: Option<GtkOverlaySetChild>,
    pub gtk_widget_set_margin_start: Option<GtkWidgetSetMarginStart>,
    pub gtk_fixed_new: Option<GtkFixedNew>,
    pub gtk_fixed_put: Option<GtkFixedPut>,
    pub gtk_fixed_move: Option<GtkFixedMove>,
    pub gtk_widget_set_margin_top: Option<GtkWidgetSetMarginTop>,
    pub gtk_widget_add_events: Option<GtkWidgetAddEvents>,
    pub gtk_widget_activate: Option<GtkWidgetActivate>,
    pub gtk_widget_get_visible: Option<GtkWidgetGetVisible>,
    pub gtk_widget_activate_action: Option<GtkWidgetActivateAction>,
    pub gtk_file_chooser_native_new: Option<GtkFileChooserNativeNew>,
    pub gtk_native_dialog_run: Option<GtkNativeDialogRun>,
    pub gtk_file_chooser_get_filename: Option<GtkFileChooserGetFilename>,
    pub gtk_file_filter_new: Option<GtkFileFilterNew>,
    pub gtk_file_filter_set_name: Option<GtkFileFilterSetName>,
    pub gtk_file_filter_add_pattern: Option<GtkFileFilterAddPattern>,
    pub gtk_file_chooser_add_filter: Option<GtkFileChooserAddFilter>,
    pub gtk_file_chooser_set_filter: Option<GtkFileChooserSetFilter>,
    pub gtk_file_chooser_set_current_name: Option<GtkFileChooserSetCurrentName>,
    pub gtk_widget_destroy: Option<GtkWidgetDestroy>,
    pub gtk_window_close: Option<GtkWindowClose>,
    pub g_free: Option<GFree>,
    pub gdk_display_get_default: Option<GdkDisplayGetDefault>,
    pub gdk_screen_get_default: Option<GdkScreenGetDefault>,
    pub gtk_style_context_add_provider_for_display: Option<GtkStyleContextAddProviderForDisplay>,
    pub gtk_style_context_add_provider_for_screen: Option<GtkStyleContextAddProviderForScreen>,
    pub gdk_event_get_keyval: Option<GdkEventGetKeyval>,
    pub gdk_event_get_state: Option<GdkEventGetState>,
    pub gdk_event_get_event_type: Option<GdkEventGetEventType>,
    pub gdk_keyval_from_name: Option<GdkKeyvalFromName>,
    // application/menu/action
    pub gtk_application_new: Option<GtkApplicationNew>,
    pub g_application_run: Option<GApplicationRun>,
    pub g_application_register: Option<GApplicationRegister>,
    pub g_simple_action_new: Option<GSimpleActionNew>,
    pub g_simple_action_group_new: Option<GSimpleActionGroupNew>,
    pub g_action_map_add_action: Option<GActionMapAddAction>,
    pub g_action_group_activate_action: Option<GActionGroupActivateAction>,
    pub g_action_map_lookup_action: Option<GActionMapLookupAction>,
    pub g_action_activate: Option<GActionActivate>,
    pub g_menu_new: Option<GMenuNew>,
    pub g_menu_append: Option<GMenuAppend>,
    pub g_application_set_app_menu: Option<GApplicationSetAppMenu>,
    pub g_application_set_menubar: Option<GApplicationSetMenubar>,
    pub g_menu_append_submenu: Option<GMenuAppendSubmenu>,
    pub gtk_popover_menu_bar_new_from_model: Option<GtkPopoverMenuBarNewFromModel>,
    pub gtk_menu_bar_new: Option<GtkMenuBarNew>,
    pub gtk_menu_new: Option<GtkMenuNew>,
    pub gtk_menu_item_new_with_label: Option<GtkMenuItemNewWithLabel>,
    pub gtk_menu_item_new_with_mnemonic: Option<GtkMenuItemNewWithMnemonic>,
    pub gtk_menu_shell_append: Option<GtkMenuShellAppend>,
    pub gtk_menu_item_set_submenu: Option<GtkMenuItemSetSubmenu>,
    pub gtk_init: Option<GtkInit>,
    pub g_signal_emit_by_name: Option<GSignalEmitByName>,
    pub g_idle_add: Option<unsafe extern "C" fn(func: Option<unsafe extern "C" fn(*mut c_void) -> i32>, data: *mut c_void) -> u32>,
    pub g_timeout_add: Option<unsafe extern "C" fn(interval_ms: u32, func: Option<unsafe extern "C" fn(*mut c_void) -> i32>, data: *mut c_void) -> u32>,
    // pango (optional)
    pub pango_layout_new: Option<unsafe extern "C" fn(context: *mut c_void) -> *mut c_void>,
    pub pango_layout_set_text: Option<unsafe extern "C" fn(layout: *mut c_void, text: *const u8, len: i32)>,
    pub pango_layout_get_size: Option<unsafe extern "C" fn(layout: *mut c_void, width: *mut i32, height: *mut i32)>,
    // cairo surface/context helpers
    pub cairo_create: Option<unsafe extern "C" fn(surface: *mut c_void) -> *mut c_void>,
    pub cairo_font_face_destroy: Option<unsafe extern "C" fn(face: *mut c_void)>,
    pub cairo_move_to: Option<unsafe extern "C" fn(cr: *mut c_void, x: f64, y: f64)>,
    pub cairo_set_source_rgb: Option<unsafe extern "C" fn(cr: *mut c_void, r: f64, g: f64, b: f64)>,
    pub cairo_set_source_rgba: Option<unsafe extern "C" fn(cr: *mut c_void, r: f64, g: f64, b: f64, a: f64)>,
    // Image blitting, for `DrawContext::draw_rgba_image`.
    pub cairo_image_surface_create: Option<unsafe extern "C" fn(format: i32, width: i32, height: i32) -> *mut c_void>,
    pub cairo_image_surface_create_for_data: Option<unsafe extern "C" fn(data: *mut u8, format: i32, width: i32, height: i32, stride: i32) -> *mut c_void>,
    pub cairo_image_surface_get_data: Option<unsafe extern "C" fn(surface: *mut c_void) -> *mut u8>,
    pub cairo_image_surface_get_stride: Option<unsafe extern "C" fn(surface: *mut c_void) -> i32>,
    pub cairo_image_surface_mark_dirty: Option<unsafe extern "C" fn(surface: *mut c_void)>,
    pub cairo_surface_destroy: Option<unsafe extern "C" fn(surface: *mut c_void)>,
    pub cairo_set_source_surface: Option<unsafe extern "C" fn(cr: *mut c_void, surface: *mut c_void, x: f64, y: f64)>,
    pub cairo_pattern_create_for_surface: Option<unsafe extern "C" fn(surface: *mut c_void) -> *mut c_void>,
    pub cairo_pattern_set_filter: Option<unsafe extern "C" fn(pattern: *mut c_void, filter: i32)>,
    pub cairo_set_source_pattern: Option<unsafe extern "C" fn(cr: *mut c_void, pattern: *mut c_void)>,
    pub cairo_pattern_destroy: Option<unsafe extern "C" fn(pattern: *mut c_void)>,
    pub cairo_scale: Option<unsafe extern "C" fn(cr: *mut c_void, sx: f64, sy: f64)>,
    pub cairo_rectangle: Option<unsafe extern "C" fn(cr: *mut c_void, x: f64, y: f64, w: f64, h: f64)>,
    pub cairo_fill: Option<unsafe extern "C" fn(cr: *mut c_void)>,
    pub cairo_stroke: Option<unsafe extern "C" fn(cr: *mut c_void)>,
    pub cairo_set_line_width: Option<unsafe extern "C" fn(cr: *mut c_void, w: f64)>,
    pub cairo_select_font_face: Option<unsafe extern "C" fn(cr: *mut c_void, family: *const u8, slant: i32, weight: i32)>,
    pub cairo_set_font_size: Option<unsafe extern "C" fn(cr: *mut c_void, size: f64)>,
    pub cairo_show_text: Option<unsafe extern "C" fn(cr: *mut c_void, utf8: *const u8)>,
    // widget helpers
    pub gtk_widget_queue_draw: Option<unsafe extern "C" fn(widget: *mut c_void)>,
    pub gtk_widget_set_can_focus: Option<GtkWidgetSetCanFocus>,
    pub gtk_label_set_xalign: Option<GtkLabelSetXalign>,
    pub gtk_event_controller_key_new: Option<GtkEventControllerKeyNew>,
    pub gtk_event_controller_focus_new: Option<GtkEventControllerFocusNew>,
    pub gtk_widget_add_controller: Option<GtkWidgetAddController>,
    pub gtk_event_controller_set_propagation_phase: Option<GtkEventControllerSetPropagationPhase>,
    pub gtk_widget_set_can_target: Option<GtkWidgetSetCanTarget>,
    pub gtk_widget_set_halign: Option<GtkWidgetSetHalign>,
    pub gtk_widget_set_valign: Option<GtkWidgetSetValign>,
    pub gtk_widget_get_allocated_width: Option<GtkWidgetGetAllocatedWidth>,
    pub gtk_widget_get_width_request: Option<GtkWidgetGetWidthRequest>,
    pub gtk_widget_get_allocated_height: Option<GtkWidgetGetAllocatedHeight>,
    pub gtk_widget_get_mapped: Option<GtkWidgetGetMapped>,
    pub gtk_widget_get_display: Option<GtkWidgetGetDisplay>,
    pub gdk_display_sync: Option<GdkDisplaySync>,
    pub gtk_widget_get_frame_clock: Option<GtkWidgetGetFrameClock>,
    pub gdk_frame_clock_request_phase: Option<GdkFrameClockRequestPhase>,
    pub gdk_frame_clock_begin_updating: Option<GdkFrameClockBeginUpdating>,
    pub gdk_frame_clock_end_updating: Option<GdkFrameClockEndUpdating>,
    pub gtk_gesture_click_new: Option<GtkGestureClickNew>,
    pub gtk_scrolled_window_new: Option<GtkScrolledWindowNew>,
    pub gtk_scrolled_window_set_overlay_scrolling: Option<GtkScrolledWindowSetOverlayScrolling>,
    pub gtk_scrolled_window_set_policy: Option<GtkScrolledWindowSetPolicy>,
    pub gtk_scrolled_window_set_child: Option<GtkScrolledWindowSetChild>,
    pub gtk_scrolled_window_get_vadjustment: Option<GtkScrolledWindowGetVadjustment>,
    pub gtk_scrolled_window_get_hadjustment: Option<GtkScrolledWindowGetHadjustment>,
    pub gtk_adjustment_get_value: Option<GtkAdjustmentGetValue>,
    pub gtk_adjustment_configure: Option<GtkAdjustmentConfigure>,
    // GMenuModel iteration (for GTK3 fallback)
    pub g_menu_model_get_n_items: Option<GMenuModelGetNItems>,
    pub g_menu_model_get_item_attribute_value: Option<GMenuModelGetItemAttributeValue>,
    pub g_menu_model_get_item_link: Option<GMenuModelGetItemLink>,
    pub g_variant_get_string: Option<GVariantGetString>,
    pub g_variant_unref: Option<GVariantUnref>,

    // Dialog
    pub gtk_dialog_new: Option<GtkDialogNew>,
    pub gtk_dialog_add_button: Option<GtkDialogAddButton>,
    pub gtk_dialog_set_default_response: Option<GtkDialogSetDefaultResponse>,
    pub gtk_dialog_get_content_area: Option<GtkDialogGetContentArea>,
    pub gtk_dialog_run: Option<GtkDialogRun>,

    // Dropdown - GTK3 ComboBoxText
    pub gtk_combo_box_text_new: Option<GtkComboBoxTextNew>,
    pub gtk_combo_box_text_append_text: Option<GtkComboBoxTextAppendText>,
    pub gtk_combo_box_text_get_active_text: Option<GtkComboBoxTextGetActiveText>,
    pub gtk_combo_box_set_active: Option<GtkComboBoxSetActive>,
    pub gtk_combo_box_get_active: Option<GtkComboBoxGetActive>,

    // Dropdown - GTK4 DropDown
    pub gtk_drop_down_new: Option<GtkDropDownNew>,
    pub gtk_drop_down_set_selected: Option<GtkDropDownSetSelected>,
    pub gtk_drop_down_get_selected: Option<GtkDropDownGetSelected>,
    pub gtk_string_list_new: Option<GtkStringListNew>,

    // Checkbox / CheckButton
    pub gtk_check_button_new_with_label: Option<GtkCheckButtonNewWithLabel>,
    pub gtk_check_button_get_active: Option<GtkCheckButtonGetActive>,
    pub gtk_check_button_set_active: Option<GtkCheckButtonSetActive>,
    pub gtk_check_button_set_group: Option<GtkCheckButtonSetGroup>,
    pub gtk_toggle_button_get_active: Option<GtkToggleButtonGetActive>,
    pub gtk_toggle_button_set_active: Option<GtkToggleButtonSetActive>,

    // RadioButton
    pub gtk_radio_button_new_with_label: Option<GtkRadioButtonNewWithLabel>,
    pub gtk_radio_button_get_group: Option<GtkRadioButtonGetGroup>,

    // TextView / TextArea
    pub gtk_text_view_new: Option<GtkTextViewNew>,
    pub gtk_text_buffer_new: Option<GtkTextBufferNew>,
    pub gtk_text_view_get_buffer: Option<GtkTextViewGetBuffer>,
    pub gtk_text_buffer_set_text: Option<GtkTextBufferSetText>,
    pub gtk_text_buffer_get_text: Option<GtkTextBufferGetText>,
    pub gtk_text_buffer_get_start_iter: Option<GtkTextBufferGetStartIter>,
    pub gtk_text_buffer_get_end_iter: Option<GtkTextBufferGetEndIter>,
    pub gtk_text_buffer_insert_at_cursor: Option<GtkTextBufferInsertAtCursor>,
    pub gtk_text_iter_copy: Option<GtkTextIterCopy>,
    pub gtk_text_iter_free: Option<GtkTextIterFree>,
    pub gtk_text_view_set_wrap_mode: Option<GtkTextViewSetWrapMode>,

    // Widget helpers
    pub gtk_widget_set_hexpand: Option<GtkWidgetSetHexpand>,
    pub gtk_widget_set_vexpand: Option<GtkWidgetSetVexpand>,
    pub gtk_widget_get_hexpand: Option<GtkWidgetGetHexpand>,
    pub gtk_widget_get_vexpand: Option<GtkWidgetGetVexpand>,

    // GtkEditable (GTK4)
    pub gtk_editable_get_text: Option<GtkEditableGetText>,
    pub gtk_editable_set_text: Option<GtkEditableSetText>,
    pub gtk_editable_set_position: Option<GtkEditableSetPosition>,
    pub gtk_editable_get_position: Option<GtkEditableGetPosition>,

    // GtkWidget parent handling
    pub gtk_widget_unparent: Option<GtkWidgetUnparent>,
    pub gtk_widget_get_parent: Option<GtkWidgetGetParent>,

    // GtkWidget tree traversal
    pub gtk_widget_get_first_child: Option<GtkWidgetGetFirstChild>,
    /// GTK3 equivalent of `gtk_widget_get_first_child`: the dynamic child list
    /// of a `GtkContainer`. GTK3 has no `gtk_widget_get_first_child` at all
    /// (verified: `nm -D libgtk-3.so.0` does not export it), so anything that
    /// walks a widget's children must use this on GTK3 or fail outright.
    /// `GtkWidget* gtk_menu_item_get_submenu(GtkMenuItem*)`.
    pub gtk_menu_item_get_submenu: Option<GtkMenuItemGetSubmenu>,
    /// `gboolean gtk_widget_is_visible(GtkWidget*)`.
    pub gtk_widget_is_visible: Option<GtkWidgetIsVisible>,
    pub gtk_container_get_children: Option<GtkContainerGetChildren>,
    /// `void g_list_free(GList*)` — `gtk_container_get_children` returns an
    /// owned list.
    pub g_list_free: Option<GListFree>,
    /// `void gtk_container_foreach(GtkContainer*, GtkCallback, gpointer)` — the
    /// way to iterate a GTK3 container without freeing a GList.
    pub gtk_container_foreach: Option<GtkContainerForeach>,
    /// `void gtk_menu_item_activate(GtkMenuItem*)` — opens a submenu. GTK3's
    /// `GtkMenuBar` children are `GtkMenuItem`s; there is no
    /// `gtk_menu_shell_activate_item` shortcut for a mnemonic.
    /// `void gtk_menu_popup(GtkMenu*, GtkWidget*, GtkWidget*, GtkMenuPositionFunc,
    /// gpointer, guint button, guint32 activate_time)` — the GTK3 way to open a
    /// menu programmatically.
    ///
    /// `gtk_menu_item_activate` does NOT do this, which is worth stating plainly
    /// because it looks like it should: verified in a 20-line C program against
    /// this machine's GTK3, activating a `GtkMenuItem` that has a submenu leaves
    /// that submenu `visible=0 mapped=0` - nothing appears on screen and no
    /// error is raised. `gtk_menu_popup` on the same menu gives `1/1`.
    pub gtk_menu_popup: Option<GtkMenuPopup>,
    /// Coordinates of a widget's top-left in screen space. Used to place a
    /// context menu (a right-click on a sheet tab) at the pointer.
    pub gtk_widget_translate_coordinates: Option<GtkWidgetTranslateCoordinates>,
    pub gtk_widget_get_window: Option<GtkWidgetGetWindow>,
    pub gdk_window_get_origin: Option<GdkWindowGetOrigin>,
    pub gtk_menu_item_activate: Option<GtkMenuItemActivate>,
    pub gtk_widget_get_next_sibling: Option<GtkWidgetGetNextSibling>,

    // GtkPopoverMenuBarItem
    pub gtk_popover_menu_bar_item_get_popover: Option<GtkPopoverMenuBarItemGetPopover>,
    // GtkPopover
    pub gtk_popover_get_child: Option<GtkPopoverGetChild>,
    pub gtk_popover_new: Option<GtkPopoverNew>,
    pub gtk_popover_set_child: Option<GtkPopoverSetChild>,

    // GtkMenuButton — manual menu bar construction
    pub gtk_menu_button_new: Option<GtkMenuButtonNew>,
    pub gtk_menu_button_set_popover: Option<GtkMenuButtonSetPopover>,
    pub gtk_menu_button_set_label: Option<GtkMenuButtonSetLabel>,
        pub gtk_menu_button_set_has_frame: Option<GtkMenuButtonSetHasFrame>,
    pub gtk_menu_button_set_primary: Option<GtkMenuButtonSetPrimary>,
    pub gtk_button_set_has_frame: Option<GtkButtonSetHasFrame>,
    pub gtk_widget_add_css_class: Option<GtkWidgetAddCssClass>,
    pub gtk_menu_button_get_popover: Option<GtkMenuButtonGetPopover>,

    // GtkPopoverMenu — new from model
    pub gtk_popover_menu_new_from_model: Option<GtkPopoverMenuNewFromModel>,

    // GtkMenuButton
    pub gtk_menu_button_get_label: Option<GtkMenuButtonGetLabel>,
    pub gtk_menu_button_set_active: Option<GtkMenuButtonSetActive>,

    // GtkWindow default size
    pub gtk_window_resize: Option<GtkWindowResize>,
    pub gtk_window_move: Option<GtkWindowMove>,
    pub gtk_window_set_default_size: Option<GtkWindowSetDefaultSize>,
    pub gtk_window_set_transient_for: Option<GtkWindowSetTransientFor>,
    pub gtk_window_set_position: Option<GtkWindowSetPosition>,

    // DrawingArea canvas (GTK4)
    pub gtk_drawing_area_set_draw_func: Option<GtkDrawingAreaSetDrawFunc>,
    pub gtk_drawing_area_set_content_width: Option<GtkDrawingAreaSetContentWidth>,
    pub gtk_drawing_area_set_content_height: Option<GtkDrawingAreaSetContentHeight>,

    // GtkNative (GtkWindow implements this) — for direct surface access
    pub gtk_native_get_surface: Option<GtkNativeGetSurface>,

    // GdkSurface operations — for direct Cairo rendering bypassing frame clock
    pub gdk_surface_create_cairo_context: Option<GdkSurfaceCreateCairoContext>,
    pub gdk_surface_get_width: Option<GdkSurfaceGetWidth>,
    pub gdk_surface_get_height: Option<GdkSurfaceGetHeight>,
    // GdkSurface begin/end draw frame (GTK 4.0-4.14, fallback when gdk_surface_create_cairo_context unavailable)
    pub gdk_surface_begin_draw_frame: Option<GdkSurfaceBeginDrawFrame>,
    pub gdk_surface_end_draw_frame: Option<GdkSurfaceEndDrawFrame>,
    pub gdk_draw_context_get_cairo_context: Option<GdkDrawContextGetCairoContext>,

    // Cairo canvas drawing
    pub cairo_text_extents: Option<CairoTextExtents>,
    pub cairo_save: Option<CairoSave>,
    pub cairo_restore: Option<CairoRestore>,
    pub cairo_clip: Option<CairoClip>,
    pub cairo_line_to: Option<CairoLineTo>,
    pub cairo_paint: Option<CairoPaint>,
    // Cairo context lifecycle
    pub cairo_destroy: Option<unsafe extern "C" fn(cr: *mut c_void)>,
}

impl Symbols {
    pub fn empty() -> Self {
        unsafe { std::mem::zeroed() }
    }

    pub fn load(libs: &std::collections::HashMap<String, std::sync::Arc<Library>>) -> Result<Self, Error> {
        // helper to lookup in libgtk first then glib/gobject as appropriate
        let gtk = libs.get("libgtk").expect("libgtk missing");
        let glib = libs.get("libglib").expect("libglib missing");
        let gobject = libs.get("libgobject").expect("libgobject missing");

        unsafe fn sym<T: Copy>(lib: &Library, name: &str) -> Option<T> {
            match lib.get::<T>(name.as_bytes()) {
                Ok(s) => Some(*s),
                Err(_) => None,
            }
        }

        macro_rules! open_sym_try {
            ($libs:ident, $key:expr, $t:ty, $name:expr) => {
                if let Some(lib) = $libs.get($key) { unsafe { match lib.get::<$t>($name.as_bytes()) { Ok(s) => Some(*s), Err(_) => None } } } else { None }
            };
        }

        // glib
        let g_main_loop_new = unsafe { sym::<GMainLoopNew>(glib, "g_main_loop_new") };
        let g_main_loop_run = unsafe { sym::<GMainLoopRun>(glib, "g_main_loop_run") };
        let g_main_loop_quit = unsafe { sym::<GMainLoopQuit>(glib, "g_main_loop_quit") };

        // gobject
        let g_object_ref = unsafe { sym::<GObjectRef>(gobject, "g_object_ref") };
        let g_object_unref = unsafe { sym::<GObjectUnref>(gobject, "g_object_unref") };
        let g_object_ref_sink = unsafe { sym::<GObjectRefSink>(gobject, "g_object_ref_sink") };
        let g_signal_connect_data = unsafe { sym::<GSignalConnectData>(gobject, "g_signal_connect_data") };
        let g_signal_connect = unsafe { sym::<GSignalConnect>(gobject, "g_signal_connect") };

        // gtk symbols
        let gtk_window_new = unsafe { sym::<GtkWindowNew>(gtk, "gtk_window_new") };
        let gtk_window_set_title = unsafe { sym::<GtkWindowSetTitle>(gtk, "gtk_window_set_title") };
        let gtk_button_new_with_label = unsafe { sym::<GtkButtonNewWithLabel>(gtk, "gtk_button_new_with_label") };
        let gtk_label_new = unsafe { sym::<GtkLabelNew>(gtk, "gtk_label_new") };
        let gtk_label_set_text = unsafe { sym::<GtkLabelSetText>(gtk, "gtk_label_set_text") };
        let gtk_label_get_text = unsafe { sym::<GtkLabelGetText>(gtk, "gtk_label_get_text") };
        let gtk_box_new = unsafe { sym::<GtkBoxNew>(gtk, "gtk_box_new") };
        let gtk_box_append = unsafe { sym::<GtkBoxAppend>(gtk, "gtk_box_append") };
        let gtk_box_pack_start = unsafe { sym::<GtkBoxPackStart>(gtk, "gtk_box_pack_start") };
        let gtk_container_add = unsafe { sym::<GtkContainerAdd>(gtk, "gtk_container_add") };
        let gtk_container_remove = unsafe { sym::<GtkContainerRemove>(gtk, "gtk_container_remove") };
        let gtk_window_set_child = unsafe { sym::<GtkWindowSetChild>(gtk, "gtk_window_set_child") };
        let gtk_widget_show_all = unsafe { sym::<GtkWidgetShowAll>(gtk, "gtk_widget_show_all") };
        let gtk_widget_show = unsafe { sym::<GtkWidgetShow>(gtk, "gtk_widget_show") };
        let gtk_widget_hide = unsafe { sym::<GtkWidgetHide>(gtk, "gtk_widget_hide") };
        let gtk_window_present = unsafe { sym::<GtkWindowPresent>(gtk, "gtk_window_present") };
        let _gtk_window_set_application = unsafe { sym::<GtkWindowSetApplication>(gtk, "gtk_window_set_application") };
        let gtk_file_chooser_native_new = open_sym_try!(libs, "libgio", GtkFileChooserNativeNew, "gtk_file_chooser_native_new").or_else(|| unsafe { sym::<GtkFileChooserNativeNew>(gtk, "gtk_file_chooser_native_new") });
        let gtk_native_dialog_run = open_sym_try!(libs, "libgio", GtkNativeDialogRun, "gtk_native_dialog_run").or_else(|| unsafe { sym::<GtkNativeDialogRun>(gtk, "gtk_native_dialog_run") });
        let gtk_file_chooser_get_filename = open_sym_try!(libs, "libgio", GtkFileChooserGetFilename, "gtk_file_chooser_get_filename").or_else(|| unsafe { sym::<GtkFileChooserGetFilename>(gtk, "gtk_file_chooser_get_filename") });
        let gtk_file_filter_new = open_sym_try!(libs, "libgio", GtkFileFilterNew, "gtk_file_filter_new").or_else(|| unsafe { sym::<GtkFileFilterNew>(gtk, "gtk_file_filter_new") });
        let gtk_file_filter_set_name = open_sym_try!(libs, "libgio", GtkFileFilterSetName, "gtk_file_filter_set_name").or_else(|| unsafe { sym::<GtkFileFilterSetName>(gtk, "gtk_file_filter_set_name") });
        let gtk_file_filter_add_pattern = open_sym_try!(libs, "libgio", GtkFileFilterAddPattern, "gtk_file_filter_add_pattern").or_else(|| unsafe { sym::<GtkFileFilterAddPattern>(gtk, "gtk_file_filter_add_pattern") });
        let gtk_file_chooser_add_filter = open_sym_try!(libs, "libgio", GtkFileChooserAddFilter, "gtk_file_chooser_add_filter").or_else(|| unsafe { sym::<GtkFileChooserAddFilter>(gtk, "gtk_file_chooser_add_filter") });
        let gtk_file_chooser_set_filter = open_sym_try!(libs, "libgio", GtkFileChooserSetFilter, "gtk_file_chooser_set_filter").or_else(|| unsafe { sym::<GtkFileChooserSetFilter>(gtk, "gtk_file_chooser_set_filter") });
        let gtk_file_chooser_set_current_name = open_sym_try!(libs, "libgio", GtkFileChooserSetCurrentName, "gtk_file_chooser_set_current_name").or_else(|| unsafe { sym::<GtkFileChooserSetCurrentName>(gtk, "gtk_file_chooser_set_current_name") });
        let gtk_widget_destroy = unsafe { sym::<GtkWidgetDestroy>(gtk, "gtk_widget_destroy") };
        let gtk_window_close = unsafe { sym::<GtkWindowClose>(gtk, "gtk_window_close") };
        let g_free = unsafe { sym::<GFree>(glib, "g_free") };
        let gdk_display_get_default = open_sym_try!(libs, "libgdk", GdkDisplayGetDefault, "gdk_display_get_default").or_else(|| unsafe { sym::<GdkDisplayGetDefault>(gtk, "gdk_display_get_default") });
        let gdk_screen_get_default = open_sym_try!(libs, "libgdk", GdkScreenGetDefault, "gdk_screen_get_default").or_else(|| unsafe { sym::<GdkScreenGetDefault>(gtk, "gdk_screen_get_default") });
        let gtk_style_context_add_provider_for_display = unsafe { sym::<GtkStyleContextAddProviderForDisplay>(gtk, "gtk_style_context_add_provider_for_display") };
        let gtk_style_context_add_provider_for_screen = unsafe { sym::<GtkStyleContextAddProviderForScreen>(gtk, "gtk_style_context_add_provider_for_screen") };
        let gdk_event_get_keyval = open_sym_try!(libs, "libgdk", GdkEventGetKeyval, "gdk_event_get_keyval").or_else(|| unsafe { sym::<GdkEventGetKeyval>(gtk, "gdk_event_get_keyval") });
        let gdk_event_get_state = open_sym_try!(libs, "libgdk", GdkEventGetState, "gdk_event_get_state").or_else(|| unsafe { sym::<GdkEventGetState>(gtk, "gdk_event_get_state") });
        let gdk_event_get_event_type = open_sym_try!(libs, "libgdk", GdkEventGetEventType, "gdk_event_get_event_type").or_else(|| unsafe { sym::<GdkEventGetEventType>(gtk, "gdk_event_get_event_type") });
        let gdk_keyval_from_name = open_sym_try!(libs, "libgdk", GdkKeyvalFromName, "gdk_keyval_from_name").or_else(|| unsafe { sym::<GdkKeyvalFromName>(gtk, "gdk_keyval_from_name") });
        let gtk_grid_new = unsafe { sym::<GtkGridNew>(gtk, "gtk_grid_new") };
        let gtk_grid_attach = unsafe { sym::<GtkGridAttach>(gtk, "gtk_grid_attach") };
        let gtk_entry_new = unsafe { sym::<GtkEntryNew>(gtk, "gtk_entry_new") };
        let gtk_entry_set_text = unsafe { sym::<GtkEntrySetText>(gtk, "gtk_entry_set_text") };
        let gtk_entry_get_text = unsafe { sym::<GtkEntryGetText>(gtk, "gtk_entry_get_text") };
        let gtk_entry_set_width_chars = unsafe { sym::<GtkEntrySetWidthChars>(gtk, "gtk_entry_set_width_chars") };
        let gtk_widget_set_size_request = unsafe { sym::<GtkWidgetSetSizeRequest>(gtk, "gtk_widget_set_size_request") };
        let gtk_entry_set_has_frame = unsafe { sym::<GtkEntrySetHasFrame>(gtk, "gtk_entry_set_has_frame") };
        let gtk_entry_set_editable = unsafe { sym::<GtkEntrySetEditable>(gtk, "gtk_editable_set_editable") };
        let gtk_drawing_area_new = unsafe { sym::<GtkDrawingAreaNew>(gtk, "gtk_drawing_area_new") };
        let gtk_label_set_markup = unsafe { sym::<GtkLabelSetMarkup>(gtk, "gtk_label_set_markup") };
        let gtk_widget_set_visible = unsafe { sym::<GtkWidgetSetVisible>(gtk, "gtk_widget_set_visible") };
        let gtk_widget_grab_focus = unsafe { sym::<GtkWidgetGrabFocus>(gtk, "gtk_widget_grab_focus") };
        // Optional: a missing symbol stays None (load never fails on it)
        // and reads as "never focused" — every caller treats None as
        // false, so exotic builds keep today's behavior.
        let gtk_widget_has_focus = unsafe { sym::<GtkWidgetHasFocus>(gtk, "gtk_widget_has_focus") };
        let gtk_widget_get_style_context = unsafe { sym::<GtkWidgetGetStyleContext>(gtk, "gtk_widget_get_style_context") };
        let gtk_style_context_add_class = unsafe { sym::<GtkStyleContextAddClass>(gtk, "gtk_style_context_add_class") };
        let gtk_style_context_remove_class = unsafe { sym::<GtkStyleContextRemoveClass>(gtk, "gtk_style_context_remove_class") };
        let gtk_css_provider_new = unsafe { sym::<GtkCssProviderNew>(gtk, "gtk_css_provider_new") };
        let gtk_css_provider_load_from_data = unsafe { sym::<GtkCssProviderLoadFromData>(gtk, "gtk_css_provider_load_from_data") };
        let gtk_style_context_add_provider = unsafe { sym::<GtkStyleContextAddProvider>(gtk, "gtk_style_context_add_provider") };
        let gtk_overlay_new = unsafe { sym::<GtkOverlayNew>(gtk, "gtk_overlay_new") };
        let gtk_overlay_add_overlay = unsafe { sym::<GtkOverlayAddOverlay>(gtk, "gtk_overlay_add_overlay") };
        let gtk_overlay_set_overlay_pass_through = unsafe { sym::<GtkOverlaySetOverlayPassThrough>(gtk, "gtk_overlay_set_overlay_pass_through") };
        let gtk_overlay_set_child = unsafe { sym::<GtkOverlaySetChild>(gtk, "gtk_overlay_set_child") };
        let gtk_widget_set_margin_start = unsafe { sym::<GtkWidgetSetMarginStart>(gtk, "gtk_widget_set_margin_start") };
        let gtk_fixed_new = unsafe { sym::<GtkFixedNew>(gtk, "gtk_fixed_new") };
        let gtk_fixed_put = unsafe { sym::<GtkFixedPut>(gtk, "gtk_fixed_put") };
        let gtk_fixed_move = unsafe { sym::<GtkFixedMove>(gtk, "gtk_fixed_move") };
        let gtk_widget_set_margin_top = unsafe { sym::<GtkWidgetSetMarginTop>(gtk, "gtk_widget_set_margin_top") };
        let gtk_widget_add_events = unsafe { sym::<GtkWidgetAddEvents>(gtk, "gtk_widget_add_events") };
        let gtk_widget_activate = unsafe { sym::<GtkWidgetActivate>(gtk, "gtk_widget_activate") };
        let gtk_widget_get_visible = unsafe { sym::<GtkWidgetGetVisible>(gtk, "gtk_widget_get_visible") };
        let gtk_widget_activate_action = unsafe { sym::<GtkWidgetActivateAction>(gtk, "gtk_widget_activate_action") };
        let gtk_init = unsafe { sym::<GtkInit>(gtk, "gtk_init") };
        let g_signal_emit_by_name = unsafe { sym::<GSignalEmitByName>(gobject, "g_signal_emit_by_name") };
        let g_idle_add = unsafe { sym::<unsafe extern "C" fn(func: Option<unsafe extern "C" fn(*mut c_void) -> i32>, data: *mut c_void) -> u32>(glib, "g_idle_add") };
        let g_timeout_add = unsafe { sym::<unsafe extern "C" fn(interval_ms: u32, func: Option<unsafe extern "C" fn(*mut c_void) -> i32>, data: *mut c_void) -> u32>(glib, "g_timeout_add") };
        // pango symbols are optional; we try to resolve them from the gtk lib too (some symbols may be available)
        let pango_layout_new = None;
        let pango_layout_set_text = None;
        let pango_layout_get_size = None;

        // cairo symbols (optional)
        let cairo_create = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void) -> *mut c_void, "cairo_create").or_else(|| None);
        let cairo_font_face_destroy = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void), "cairo_font_face_destroy").or_else(|| None);
        let cairo_move_to = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, f64, f64), "cairo_move_to").or_else(|| None);
        let cairo_set_source_rgb = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, f64, f64, f64), "cairo_set_source_rgb").or_else(|| None);
        let cairo_set_source_rgba = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, f64, f64, f64, f64), "cairo_set_source_rgba").or_else(|| None);
        let cairo_rectangle = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, f64, f64, f64, f64), "cairo_rectangle").or_else(|| None);
        // Image blitting for `DrawContext::draw_rgba_image`. All optional: an
        // older libcairo without them just means the blit reports `false` and
        // the caller falls back to vector drawing.
        let cairo_image_surface_create = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(i32, i32, i32) -> *mut c_void, "cairo_image_surface_create").or_else(|| None);
        let cairo_image_surface_create_for_data = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut u8, i32, i32, i32, i32) -> *mut c_void, "cairo_image_surface_create_for_data").or_else(|| None);
        let cairo_image_surface_get_data = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void) -> *mut u8, "cairo_image_surface_get_data").or_else(|| None);
        let cairo_image_surface_get_stride = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void) -> i32, "cairo_image_surface_get_stride").or_else(|| None);
        let cairo_image_surface_mark_dirty = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void), "cairo_image_surface_mark_dirty").or_else(|| None);
        let cairo_surface_destroy = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void), "cairo_surface_destroy").or_else(|| None);
        let cairo_set_source_surface = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, *mut c_void, f64, f64), "cairo_set_source_surface").or_else(|| None);
        let cairo_pattern_create_for_surface = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void) -> *mut c_void, "cairo_pattern_create_for_surface").or_else(|| None);
        let cairo_pattern_set_filter = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, i32), "cairo_pattern_set_filter").or_else(|| None);
        let cairo_set_source_pattern = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, *mut c_void), "cairo_set_source_pattern").or_else(|| None);
        let cairo_pattern_destroy = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void), "cairo_pattern_destroy").or_else(|| None);
        let cairo_scale = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, f64, f64), "cairo_scale").or_else(|| None);
        let cairo_fill = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void), "cairo_fill").or_else(|| None);
        let cairo_stroke = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void), "cairo_stroke").or_else(|| None);
        let cairo_set_line_width = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, f64), "cairo_set_line_width").or_else(|| None);
        let cairo_select_font_face = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, *const u8, i32, i32), "cairo_select_font_face").or_else(|| None);
        let cairo_set_font_size = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, f64), "cairo_set_font_size").or_else(|| None);
        let cairo_show_text = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void, *const u8), "cairo_show_text").or_else(|| None);
        let cairo_text_extents = open_sym_try!(libs, "libcairo", CairoTextExtents, "cairo_text_extents").or_else(|| None);
        let cairo_save = open_sym_try!(libs, "libcairo", CairoSave, "cairo_save").or_else(|| None);
        let cairo_restore = open_sym_try!(libs, "libcairo", CairoRestore, "cairo_restore").or_else(|| None);
        let cairo_clip = open_sym_try!(libs, "libcairo", CairoClip, "cairo_clip").or_else(|| None);
        let cairo_line_to = open_sym_try!(libs, "libcairo", CairoLineTo, "cairo_line_to").or_else(|| None);
        let cairo_paint = open_sym_try!(libs, "libcairo", CairoPaint, "cairo_paint").or_else(|| None);
        let cairo_destroy = open_sym_try!(libs, "libcairo", unsafe extern "C" fn(*mut c_void), "cairo_destroy").or_else(|| None);
        let gtk_drawing_area_set_draw_func = unsafe { sym::<GtkDrawingAreaSetDrawFunc>(gtk, "gtk_drawing_area_set_draw_func") };
        let gtk_drawing_area_set_content_width = unsafe { sym::<GtkDrawingAreaSetContentWidth>(gtk, "gtk_drawing_area_set_content_width") };
        let gtk_drawing_area_set_content_height = unsafe { sym::<GtkDrawingAreaSetContentHeight>(gtk, "gtk_drawing_area_set_content_height") };
        let gtk_native_get_surface = unsafe { sym::<GtkNativeGetSurface>(gtk, "gtk_native_get_surface") };
        let gdk_surface_create_cairo_context = unsafe { sym::<GdkSurfaceCreateCairoContext>(gtk, "gdk_surface_create_cairo_context") };
        let gdk_surface_get_width = unsafe { sym::<GdkSurfaceGetWidth>(gtk, "gdk_surface_get_width") };
        let gdk_surface_get_height = unsafe { sym::<GdkSurfaceGetHeight>(gtk, "gdk_surface_get_height") };
        // Fallback direct-drawing symbols (GTK 4.0-4.14, deprecated in favor of gdk_surface_create_cairo_context)
        let gdk_surface_begin_draw_frame = unsafe { sym::<GdkSurfaceBeginDrawFrame>(gtk, "gdk_surface_begin_draw_frame") };
        let gdk_surface_end_draw_frame = unsafe { sym::<GdkSurfaceEndDrawFrame>(gtk, "gdk_surface_end_draw_frame") };
        let gdk_draw_context_get_cairo_context = unsafe { sym::<GdkDrawContextGetCairoContext>(gtk, "gdk_draw_context_get_cairo_context") };
        let gtk_widget_set_can_focus = unsafe { sym::<GtkWidgetSetCanFocus>(gtk, "gtk_widget_set_can_focus") };
        let gtk_widget_queue_draw = unsafe { sym::<unsafe extern "C" fn(*mut c_void)>(gtk, "gtk_widget_queue_draw") };
        let gtk_label_set_xalign = unsafe { sym::<GtkLabelSetXalign>(gtk, "gtk_label_set_xalign") };
        let gtk_event_controller_key_new = unsafe { sym::<GtkEventControllerKeyNew>(gtk, "gtk_event_controller_key_new") };
        let gtk_event_controller_focus_new = unsafe { sym::<GtkEventControllerFocusNew>(gtk, "gtk_event_controller_focus_new") };
        let gtk_widget_add_controller = unsafe { sym::<GtkWidgetAddController>(gtk, "gtk_widget_add_controller") };
        let gtk_event_controller_set_propagation_phase = unsafe { sym::<GtkEventControllerSetPropagationPhase>(gtk, "gtk_event_controller_set_propagation_phase") };
        let gtk_widget_set_halign = unsafe { sym::<GtkWidgetSetHalign>(gtk, "gtk_widget_set_halign") };
        let gtk_widget_set_valign = unsafe { sym::<GtkWidgetSetValign>(gtk, "gtk_widget_set_valign") };
        let gtk_widget_get_allocated_width = unsafe { sym::<GtkWidgetGetAllocatedWidth>(gtk, "gtk_widget_get_allocated_width") };
        let gtk_widget_get_width_request = unsafe { sym::<GtkWidgetGetWidthRequest>(gtk, "gtk_widget_get_width_request") };
        let gtk_widget_get_allocated_height = unsafe { sym::<GtkWidgetGetAllocatedHeight>(gtk, "gtk_widget_get_allocated_height") };
        let gtk_widget_get_mapped = unsafe { sym::<GtkWidgetGetMapped>(gtk, "gtk_widget_get_mapped") };
        let gtk_widget_get_display = unsafe { sym::<GtkWidgetGetDisplay>(gtk, "gtk_widget_get_display") };
        let gdk_display_sync = unsafe { sym::<GdkDisplaySync>(gtk, "gdk_display_sync") };
        let gtk_widget_get_frame_clock = unsafe { sym::<GtkWidgetGetFrameClock>(gtk, "gtk_widget_get_frame_clock") };
        let gdk_frame_clock_request_phase = unsafe { sym::<GdkFrameClockRequestPhase>(gtk, "gdk_frame_clock_request_phase") };
        let gdk_frame_clock_begin_updating = unsafe { sym::<GdkFrameClockBeginUpdating>(gtk, "gdk_frame_clock_begin_updating") };
        let gdk_frame_clock_end_updating = unsafe { sym::<GdkFrameClockEndUpdating>(gtk, "gdk_frame_clock_end_updating") };
        let gtk_gesture_click_new = unsafe { sym::<GtkGestureClickNew>(gtk, "gtk_gesture_click_new") };
        let gtk_scrolled_window_new = unsafe { sym::<GtkScrolledWindowNew>(gtk, "gtk_scrolled_window_new") };
        let gtk_scrolled_window_set_overlay_scrolling = unsafe { sym::<GtkScrolledWindowSetOverlayScrolling>(gtk, "gtk_scrolled_window_set_overlay_scrolling") };
        let gtk_scrolled_window_set_policy = unsafe { sym::<GtkScrolledWindowSetPolicy>(gtk, "gtk_scrolled_window_set_policy") };
        let gtk_scrolled_window_set_child = unsafe { sym::<GtkScrolledWindowSetChild>(gtk, "gtk_scrolled_window_set_child") };
        let gtk_scrolled_window_get_vadjustment = unsafe { sym::<GtkScrolledWindowGetVadjustment>(gtk, "gtk_scrolled_window_get_vadjustment") };
        let gtk_scrolled_window_get_hadjustment = unsafe { sym::<GtkScrolledWindowGetHadjustment>(gtk, "gtk_scrolled_window_get_hadjustment") };
        let gtk_adjustment_get_value = unsafe { sym::<GtkAdjustmentGetValue>(gtk, "gtk_adjustment_get_value") };
        let gtk_adjustment_configure = unsafe { sym::<GtkAdjustmentConfigure>(gtk, "gtk_adjustment_configure") };

        // application/menu/action symbols (try glib/gio)
        // try gio first then glib for the app/menu symbols
        let gtk_application_new = open_sym_try!(libs, "libgio", GtkApplicationNew, "g_application_new").or_else(|| unsafe { sym::<GtkApplicationNew>(glib, "g_application_new") });
        let g_application_run = open_sym_try!(libs, "libgio", GApplicationRun, "g_application_run").or_else(|| unsafe { sym::<GApplicationRun>(glib, "g_application_run") });
        let g_application_register = open_sym_try!(libs, "libgio", GApplicationRegister, "g_application_register").or_else(|| unsafe { sym::<GApplicationRegister>(glib, "g_application_register") });
        let g_simple_action_new = open_sym_try!(libs, "libgio", GSimpleActionNew, "g_simple_action_new").or_else(|| unsafe { sym::<GSimpleActionNew>(glib, "g_simple_action_new") });
        let g_simple_action_group_new = open_sym_try!(libs, "libgio", GSimpleActionGroupNew, "g_simple_action_group_new").or_else(|| unsafe { sym::<GSimpleActionGroupNew>(glib, "g_simple_action_group_new") });
        let g_action_map_add_action = open_sym_try!(libs, "libgio", GActionMapAddAction, "g_action_map_add_action").or_else(|| unsafe { sym::<GActionMapAddAction>(glib, "g_action_map_add_action") });
        let g_action_group_activate_action = open_sym_try!(libs, "libgio", GActionGroupActivateAction, "g_action_group_activate_action").or_else(|| unsafe { sym::<GActionGroupActivateAction>(glib, "g_action_group_activate_action") });
        let g_action_map_lookup_action = open_sym_try!(libs, "libgio", GActionMapLookupAction, "g_action_map_lookup_action").or_else(|| unsafe { sym::<GActionMapLookupAction>(glib, "g_action_map_lookup_action") });
        let g_action_activate = open_sym_try!(libs, "libgio", GActionActivate, "g_action_activate").or_else(|| unsafe { sym::<GActionActivate>(glib, "g_action_activate") });
        let g_menu_new = open_sym_try!(libs, "libgio", GMenuNew, "g_menu_new").or_else(|| unsafe { sym::<GMenuNew>(glib, "g_menu_new") });
        let g_menu_append = open_sym_try!(libs, "libgio", GMenuAppend, "g_menu_append").or_else(|| unsafe { sym::<GMenuAppend>(glib, "g_menu_append") });
        let g_application_set_app_menu = open_sym_try!(libs, "libgio", GApplicationSetAppMenu, "g_application_set_app_menu").or_else(|| unsafe { sym::<GApplicationSetAppMenu>(glib, "g_application_set_app_menu") });
        let g_application_set_menubar = open_sym_try!(libs, "libgio", GApplicationSetMenubar, "g_application_set_menubar").or_else(|| unsafe { sym::<GApplicationSetMenubar>(glib, "g_application_set_menubar") });
        let g_menu_append_submenu = open_sym_try!(libs, "libgio", GMenuAppendSubmenu, "g_menu_append_submenu").or_else(|| unsafe { sym::<GMenuAppendSubmenu>(glib, "g_menu_append_submenu") });
        let gtk_popover_menu_bar_new_from_model = unsafe { sym::<GtkPopoverMenuBarNewFromModel>(gtk, "gtk_popover_menu_bar_new_from_model") };
        let gtk_menu_bar_new = unsafe { sym::<GtkMenuBarNew>(gtk, "gtk_menu_bar_new") };
        let gtk_menu_new = unsafe { sym::<GtkMenuNew>(gtk, "gtk_menu_new") };
        let gtk_menu_item_new_with_label = unsafe { sym::<GtkMenuItemNewWithLabel>(gtk, "gtk_menu_item_new_with_label") };
        let gtk_menu_item_new_with_mnemonic = unsafe { sym::<GtkMenuItemNewWithMnemonic>(gtk, "gtk_menu_item_new_with_mnemonic") };
        let gtk_menu_shell_append = unsafe { sym::<GtkMenuShellAppend>(gtk, "gtk_menu_shell_append") };
        let gtk_menu_item_set_submenu = unsafe { sym::<GtkMenuItemSetSubmenu>(gtk, "gtk_menu_item_set_submenu") };
        let gtk_window_set_application = unsafe { sym::<GtkWindowSetApplication>(gtk, "gtk_window_set_application") };
        let gtk_widget_insert_action_group = unsafe { sym::<GtkWidgetInsertActionGroup>(gtk, "gtk_widget_insert_action_group") };
        let gtk_actionable_set_detailed_action_name = unsafe { sym::<GtkActionableSetDetailedActionName>(gtk, "gtk_actionable_set_detailed_action_name") };
        let g_menu_model_get_n_items = open_sym_try!(libs, "libgio", GMenuModelGetNItems, "g_menu_model_get_n_items").or_else(|| unsafe { sym::<GMenuModelGetNItems>(glib, "g_menu_model_get_n_items") });
        let g_menu_model_get_item_attribute_value = open_sym_try!(libs, "libgio", GMenuModelGetItemAttributeValue, "g_menu_model_get_item_attribute_value").or_else(|| unsafe { sym::<GMenuModelGetItemAttributeValue>(glib, "g_menu_model_get_item_attribute_value") });
        let g_menu_model_get_item_link = open_sym_try!(libs, "libgio", GMenuModelGetItemLink, "g_menu_model_get_item_link").or_else(|| unsafe { sym::<GMenuModelGetItemLink>(glib, "g_menu_model_get_item_link") });
        let g_variant_get_string = unsafe { sym::<GVariantGetString>(glib, "g_variant_get_string") };
        let g_variant_unref = unsafe { sym::<GVariantUnref>(glib, "g_variant_unref") };

        // Dialog
        let gtk_dialog_new = unsafe { sym::<GtkDialogNew>(gtk, "gtk_dialog_new") };
        let gtk_dialog_add_button = unsafe { sym::<GtkDialogAddButton>(gtk, "gtk_dialog_add_button") };
        let gtk_dialog_set_default_response = unsafe { sym::<GtkDialogSetDefaultResponse>(gtk, "gtk_dialog_set_default_response") };
        let gtk_dialog_get_content_area = unsafe { sym::<GtkDialogGetContentArea>(gtk, "gtk_dialog_get_content_area") };
        let gtk_dialog_run = unsafe { sym::<GtkDialogRun>(gtk, "gtk_dialog_run") };

        // Dropdown - GTK3 ComboBoxText
        let gtk_combo_box_text_new = unsafe { sym::<GtkComboBoxTextNew>(gtk, "gtk_combo_box_text_new") };
        let gtk_combo_box_text_append_text = unsafe { sym::<GtkComboBoxTextAppendText>(gtk, "gtk_combo_box_text_append_text") };
        let gtk_combo_box_text_get_active_text = unsafe { sym::<GtkComboBoxTextGetActiveText>(gtk, "gtk_combo_box_text_get_active_text") };
        let gtk_combo_box_set_active = unsafe { sym::<GtkComboBoxSetActive>(gtk, "gtk_combo_box_set_active") };
        let gtk_combo_box_get_active = unsafe { sym::<GtkComboBoxGetActive>(gtk, "gtk_combo_box_get_active") };

        // Dropdown - GTK4 DropDown
        let gtk_drop_down_new = unsafe { sym::<GtkDropDownNew>(gtk, "gtk_drop_down_new") };
        let gtk_drop_down_set_selected = unsafe { sym::<GtkDropDownSetSelected>(gtk, "gtk_drop_down_set_selected") };
        let gtk_drop_down_get_selected = unsafe { sym::<GtkDropDownGetSelected>(gtk, "gtk_drop_down_get_selected") };
        let gtk_string_list_new = unsafe { sym::<GtkStringListNew>(gtk, "gtk_string_list_new") };

        // Checkbox / CheckButton
        let gtk_check_button_new_with_label = unsafe { sym::<GtkCheckButtonNewWithLabel>(gtk, "gtk_check_button_new_with_label") };
        let gtk_check_button_get_active = unsafe { sym::<GtkCheckButtonGetActive>(gtk, "gtk_check_button_get_active") };
        let gtk_check_button_set_active = unsafe { sym::<GtkCheckButtonSetActive>(gtk, "gtk_check_button_set_active") };
        let gtk_check_button_set_group = unsafe { sym::<GtkCheckButtonSetGroup>(gtk, "gtk_check_button_set_group") };
        let gtk_toggle_button_get_active = unsafe { sym::<GtkToggleButtonGetActive>(gtk, "gtk_toggle_button_get_active") };
        let gtk_toggle_button_set_active = unsafe { sym::<GtkToggleButtonSetActive>(gtk, "gtk_toggle_button_set_active") };

        // RadioButton
        let gtk_radio_button_new_with_label = unsafe { sym::<GtkRadioButtonNewWithLabel>(gtk, "gtk_radio_button_new_with_label") };
        let gtk_radio_button_get_group = unsafe { sym::<GtkRadioButtonGetGroup>(gtk, "gtk_radio_button_get_group") };

        // TextView / TextArea
        let gtk_text_view_new = unsafe { sym::<GtkTextViewNew>(gtk, "gtk_text_view_new") };
        let gtk_text_buffer_new = unsafe { sym::<GtkTextBufferNew>(gtk, "gtk_text_buffer_new") };
        let gtk_text_view_get_buffer = unsafe { sym::<GtkTextViewGetBuffer>(gtk, "gtk_text_view_get_buffer") };
        let gtk_text_buffer_set_text = unsafe { sym::<GtkTextBufferSetText>(gtk, "gtk_text_buffer_set_text") };
        let gtk_text_buffer_get_text = unsafe { sym::<GtkTextBufferGetText>(gtk, "gtk_text_buffer_get_text") };
        let gtk_text_buffer_get_start_iter = unsafe { sym::<GtkTextBufferGetStartIter>(gtk, "gtk_text_buffer_get_start_iter") };
        let gtk_text_buffer_get_end_iter = unsafe { sym::<GtkTextBufferGetEndIter>(gtk, "gtk_text_buffer_get_end_iter") };
        let gtk_text_buffer_insert_at_cursor = unsafe { sym::<GtkTextBufferInsertAtCursor>(gtk, "gtk_text_buffer_insert_at_cursor") };
        let gtk_text_iter_copy = unsafe { sym::<GtkTextIterCopy>(gtk, "gtk_text_iter_copy") };
        let gtk_text_iter_free = unsafe { sym::<GtkTextIterFree>(gtk, "gtk_text_iter_free") };
        let gtk_text_view_set_wrap_mode = unsafe { sym::<GtkTextViewSetWrapMode>(gtk, "gtk_text_view_set_wrap_mode") };

        // Widget helpers
        let gtk_widget_set_hexpand = unsafe { sym::<GtkWidgetSetHexpand>(gtk, "gtk_widget_set_hexpand") };
        let gtk_widget_set_vexpand = unsafe { sym::<GtkWidgetSetVexpand>(gtk, "gtk_widget_set_vexpand") };
        let gtk_widget_get_hexpand = unsafe { sym::<GtkWidgetGetHexpand>(gtk, "gtk_widget_get_hexpand") };
        let gtk_widget_get_vexpand = unsafe { sym::<GtkWidgetGetVexpand>(gtk, "gtk_widget_get_vexpand") };

        // GtkEditable (GTK4, replaces gtk_entry_get_text/set_text)
        let gtk_editable_get_text = unsafe { sym::<GtkEditableGetText>(gtk, "gtk_editable_get_text") };
        let gtk_editable_set_text = unsafe { sym::<GtkEditableSetText>(gtk, "gtk_editable_set_text") };
        let gtk_editable_set_position = unsafe { sym::<GtkEditableSetPosition>(gtk, "gtk_editable_set_position") };
        let gtk_editable_get_position = unsafe { sym::<GtkEditableGetPosition>(gtk, "gtk_editable_get_position") };

        // GtkWidget parent handling
        let gtk_widget_unparent = unsafe { sym::<GtkWidgetUnparent>(gtk, "gtk_widget_unparent") };
        let gtk_widget_get_parent = unsafe { sym::<GtkWidgetGetParent>(gtk, "gtk_widget_get_parent") };

        // GtkWidget tree traversal
        let gtk_widget_get_first_child = unsafe { sym::<GtkWidgetGetFirstChild>(gtk, "gtk_widget_get_first_child") };
        let gtk_container_get_children = unsafe { sym::<GtkContainerGetChildren>(gtk, "gtk_container_get_children") };
        let gtk_menu_item_get_submenu = unsafe { sym::<GtkMenuItemGetSubmenu>(gtk, "gtk_menu_item_get_submenu") };
        let gtk_widget_is_visible = unsafe { sym::<GtkWidgetIsVisible>(gtk, "gtk_widget_is_visible") };
        let g_list_free = unsafe { sym::<GListFree>(gtk, "g_list_free") };
        let gtk_container_foreach = unsafe { sym::<GtkContainerForeach>(gtk, "gtk_container_foreach") };
        let gtk_menu_item_activate = unsafe { sym::<GtkMenuItemActivate>(gtk, "gtk_menu_item_activate") };
        let gtk_menu_popup = unsafe { sym::<GtkMenuPopup>(gtk, "gtk_menu_popup") };
        let gtk_widget_get_next_sibling = unsafe { sym::<GtkWidgetGetNextSibling>(gtk, "gtk_widget_get_next_sibling") };
        // Screen-space placement of a context menu. `gtk_widget_get_window`
        // and `gdk_window_get_origin` live in different libraries, like the
        // other Gdk calls, so each is resolved with a fallback.
        let gtk_widget_translate_coordinates =
            unsafe { sym::<GtkWidgetTranslateCoordinates>(gtk, "gtk_widget_translate_coordinates") };
        let gtk_widget_get_window = unsafe { sym::<GtkWidgetGetWindow>(gtk, "gtk_widget_get_window") };
        // All three resolve from libgtk-3 (verified with nm -D): GTK3 links
        // against GDK directly, so there is no separate libgdk to search.
        let gdk_window_get_origin = unsafe { sym::<GdkWindowGetOrigin>(gtk, "gdk_window_get_origin") };

        // GtkPopoverMenuBarItem
        let gtk_popover_menu_bar_item_get_popover = unsafe { sym::<GtkPopoverMenuBarItemGetPopover>(gtk, "gtk_popover_menu_bar_item_get_popover") };
        // GtkPopover
        let gtk_popover_get_child = unsafe { sym::<GtkPopoverGetChild>(gtk, "gtk_popover_get_child") };
        let gtk_popover_new = unsafe { sym::<GtkPopoverNew>(gtk, "gtk_popover_new") };
        let gtk_popover_set_child = unsafe { sym::<GtkPopoverSetChild>(gtk, "gtk_popover_set_child") };

        // GtkMenuButton — manual menu bar construction
        let gtk_menu_button_new = unsafe { sym::<GtkMenuButtonNew>(gtk, "gtk_menu_button_new") };
        let gtk_menu_button_set_popover = unsafe { sym::<GtkMenuButtonSetPopover>(gtk, "gtk_menu_button_set_popover") };
        let gtk_menu_button_set_label = unsafe { sym::<GtkMenuButtonSetLabel>(gtk, "gtk_menu_button_set_label") };
                let gtk_menu_button_set_has_frame = unsafe { sym::<GtkMenuButtonSetHasFrame>(gtk, "gtk_menu_button_set_has_frame") };
        let gtk_menu_button_set_primary = unsafe { sym::<GtkMenuButtonSetPrimary>(gtk, "gtk_menu_button_set_primary") };
        let gtk_button_set_has_frame = unsafe { sym::<GtkButtonSetHasFrame>(gtk, "gtk_button_set_has_frame") };
        let gtk_widget_add_css_class = unsafe { sym::<GtkWidgetAddCssClass>(gtk, "gtk_widget_add_css_class") };
        let gtk_menu_button_get_popover = unsafe { sym::<GtkMenuButtonGetPopover>(gtk, "gtk_menu_button_get_popover") };
        let gtk_widget_set_can_target = unsafe { sym::<GtkWidgetSetCanTarget>(gtk, "gtk_widget_set_can_target") };

        // GtkPopoverMenu — new from model
        let gtk_popover_menu_new_from_model = unsafe { sym::<GtkPopoverMenuNewFromModel>(gtk, "gtk_popover_menu_new_from_model") };

        // GtkMenuButton
        let gtk_menu_button_get_label = unsafe { sym::<GtkMenuButtonGetLabel>(gtk, "gtk_menu_button_get_label") };
        let gtk_menu_button_set_active = unsafe { sym::<GtkMenuButtonSetActive>(gtk, "gtk_menu_button_set_active") };

        // GtkWindow default size
        let gtk_window_resize = unsafe { sym::<GtkWindowResize>(gtk, "gtk_window_resize") };
        let gtk_window_move = unsafe { sym::<GtkWindowMove>(gtk, "gtk_window_move") };
        let gtk_window_set_default_size = unsafe { sym::<GtkWindowSetDefaultSize>(gtk, "gtk_window_set_default_size") };
        let gtk_window_set_transient_for = unsafe { sym::<GtkWindowSetTransientFor>(gtk, "gtk_window_set_transient_for") };
        let gtk_window_set_position = unsafe { sym::<GtkWindowSetPosition>(gtk, "gtk_window_set_position") };

        Ok(Symbols {
            g_main_loop_new, g_main_loop_run, g_main_loop_quit,
            g_object_ref, g_object_unref, g_object_ref_sink, g_signal_connect_data, g_signal_connect,
            gtk_window_new, gtk_window_set_title, gtk_button_new_with_label, gtk_label_new, gtk_label_set_text,
            gtk_box_new, gtk_box_append, gtk_box_pack_start, gtk_container_add, gtk_container_remove, gtk_window_set_child,
            gtk_widget_show_all, gtk_widget_show, gtk_widget_hide, gtk_window_present,
            gtk_grid_new, gtk_grid_attach, gtk_entry_new, gtk_entry_set_text, gtk_entry_get_text,
            gtk_entry_set_width_chars, gtk_widget_set_size_request, gtk_entry_set_has_frame,
            gtk_label_set_markup, gtk_widget_set_visible, gtk_widget_grab_focus, gtk_widget_has_focus,
            gtk_widget_get_style_context, gtk_style_context_add_class, gtk_style_context_remove_class,
            gtk_css_provider_new, gtk_css_provider_load_from_data, gtk_style_context_add_provider,
            gtk_overlay_new, gtk_overlay_add_overlay, gtk_overlay_set_overlay_pass_through, gtk_overlay_set_child,
            gtk_widget_set_margin_start, gtk_widget_set_margin_top, gtk_widget_add_events,
            gtk_fixed_new, gtk_fixed_put, gtk_fixed_move,
            gtk_widget_activate, gtk_widget_get_visible, gtk_widget_activate_action,
            gtk_init,
            g_signal_emit_by_name,
            gtk_label_get_text,
            gtk_entry_set_editable,
            gtk_drawing_area_new,
            g_idle_add, g_timeout_add,
            pango_layout_new, pango_layout_set_text, pango_layout_get_size,
            cairo_create, cairo_font_face_destroy,
            cairo_move_to, cairo_set_source_rgb, cairo_set_source_rgba, cairo_rectangle, cairo_fill, cairo_stroke, cairo_set_line_width, cairo_select_font_face, cairo_set_font_size, cairo_show_text,
            cairo_image_surface_create, cairo_image_surface_create_for_data, cairo_image_surface_get_data, cairo_image_surface_get_stride, cairo_image_surface_mark_dirty, cairo_surface_destroy, cairo_set_source_surface, cairo_pattern_create_for_surface, cairo_pattern_set_filter, cairo_set_source_pattern, cairo_pattern_destroy, cairo_scale,
            gtk_widget_queue_draw,
            gtk_widget_set_can_focus,
            gtk_file_chooser_native_new, gtk_native_dialog_run, gtk_file_chooser_get_filename, gtk_file_filter_new, gtk_file_filter_set_name, gtk_file_filter_add_pattern, gtk_file_chooser_add_filter, gtk_file_chooser_set_filter, gtk_file_chooser_set_current_name, gtk_widget_destroy, gtk_window_close, g_free, gdk_display_get_default, gdk_screen_get_default, gtk_style_context_add_provider_for_display, gtk_style_context_add_provider_for_screen, gdk_event_get_keyval, gdk_event_get_state, gdk_event_get_event_type, gdk_keyval_from_name,
            gtk_application_new, g_application_run, g_application_register, g_simple_action_new, g_simple_action_group_new, g_action_map_add_action, g_action_group_activate_action, g_action_map_lookup_action, g_action_activate,
            g_menu_new, g_menu_append, g_application_set_app_menu, g_application_set_menubar, g_menu_append_submenu, gtk_popover_menu_bar_new_from_model, gtk_menu_bar_new, gtk_menu_new, gtk_menu_item_new_with_label, gtk_menu_item_new_with_mnemonic, gtk_menu_shell_append, gtk_menu_item_set_submenu, gtk_window_set_application, gtk_widget_insert_action_group, gtk_actionable_set_detailed_action_name, g_menu_model_get_n_items, g_menu_model_get_item_attribute_value, g_menu_model_get_item_link, g_variant_get_string, g_variant_unref,
            gtk_label_set_xalign,
            gtk_event_controller_key_new,
            gtk_event_controller_focus_new,
            gtk_widget_add_controller,
            gtk_event_controller_set_propagation_phase,
            gtk_widget_set_halign,
            gtk_widget_set_valign,
            gtk_widget_get_allocated_width, gtk_widget_get_width_request,
            gtk_widget_get_allocated_height,
            gtk_widget_get_mapped,
            gtk_widget_get_display,
            gdk_display_sync,
            gtk_widget_get_frame_clock,
            gdk_frame_clock_request_phase,
            gdk_frame_clock_begin_updating,
            gdk_frame_clock_end_updating,
            gtk_gesture_click_new,
            gtk_scrolled_window_new, gtk_scrolled_window_set_overlay_scrolling,
            gtk_scrolled_window_set_policy,
            gtk_scrolled_window_set_child,
            gtk_scrolled_window_get_vadjustment,
            gtk_scrolled_window_get_hadjustment,
            gtk_adjustment_get_value, gtk_adjustment_configure,
            gtk_dialog_new, gtk_dialog_add_button, gtk_dialog_set_default_response, gtk_dialog_get_content_area, gtk_dialog_run,
            gtk_combo_box_text_new, gtk_combo_box_text_append_text, gtk_combo_box_text_get_active_text, gtk_combo_box_set_active, gtk_combo_box_get_active,
            gtk_drop_down_new, gtk_drop_down_set_selected, gtk_drop_down_get_selected, gtk_string_list_new,
            gtk_check_button_new_with_label, gtk_check_button_get_active, gtk_check_button_set_active, gtk_check_button_set_group, gtk_toggle_button_get_active, gtk_toggle_button_set_active,
            gtk_radio_button_new_with_label, gtk_radio_button_get_group,
            gtk_text_view_new, gtk_text_buffer_new, gtk_text_view_get_buffer, gtk_text_buffer_set_text, gtk_text_buffer_get_text, gtk_text_buffer_get_start_iter, gtk_text_buffer_get_end_iter, gtk_text_buffer_insert_at_cursor, gtk_text_iter_copy, gtk_text_iter_free, gtk_text_view_set_wrap_mode,
            gtk_widget_set_hexpand, gtk_widget_set_vexpand,
            gtk_widget_get_hexpand, gtk_widget_get_vexpand,
            gtk_editable_get_text, gtk_editable_set_text, gtk_editable_set_position,
            gtk_editable_get_position,
            gtk_widget_unparent, gtk_widget_get_parent,
            gtk_widget_get_first_child, gtk_widget_get_next_sibling,
            gtk_container_get_children, gtk_container_foreach, gtk_menu_item_activate, g_list_free, gtk_menu_item_get_submenu, gtk_widget_is_visible, gtk_menu_popup,
            gtk_widget_translate_coordinates, gtk_widget_get_window, gdk_window_get_origin,
            gtk_popover_menu_bar_item_get_popover, gtk_popover_get_child, gtk_popover_new, gtk_popover_set_child,
            gtk_menu_button_get_label, gtk_menu_button_set_active,
            gtk_menu_button_new, gtk_menu_button_set_popover, gtk_menu_button_set_label,
            gtk_menu_button_set_has_frame, gtk_menu_button_set_primary,
            gtk_button_set_has_frame, gtk_widget_add_css_class,
            gtk_menu_button_get_popover, gtk_widget_set_can_target,
            gtk_popover_menu_new_from_model,
            gtk_window_resize, gtk_window_move, gtk_window_set_default_size,
            gtk_window_set_transient_for, gtk_window_set_position,
            gtk_drawing_area_set_draw_func, gtk_drawing_area_set_content_width, gtk_drawing_area_set_content_height,
            gtk_native_get_surface, gdk_surface_create_cairo_context, gdk_surface_get_width, gdk_surface_get_height,
            gdk_surface_begin_draw_frame, gdk_surface_end_draw_frame, gdk_draw_context_get_cairo_context,
            cairo_text_extents, cairo_save, cairo_restore, cairo_clip, cairo_line_to, cairo_paint, cairo_destroy,
        })
    }
}
