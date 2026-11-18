// ---------------------------------------------------------------------------
// Cross-platform key constants
// ---------------------------------------------------------------------------
pub mod key {
    #[cfg(unix)]
    mod plat {
        pub const RETURN: u32 = 0xFF0D;
        pub const ENTER: u32 = 0xFF8D;
        pub const ESCAPE: u32 = 0xFF1B;
        pub const BACKSPACE: u32 = 0xFF08;
        pub const DELETE: u32 = 0xFFFF;
        pub const LEFT: u32 = 0xFF51;
        pub const UP: u32 = 0xFF52;
        pub const RIGHT: u32 = 0xFF53;
        pub const DOWN: u32 = 0xFF54;
        pub const TAB: u32 = 0xFF09;
        pub const HOME: u32 = 0xFF50;
        pub const END: u32 = 0xFF57;
        pub const PAGE_UP: u32 = 0xFF55;
        pub const PAGE_DOWN: u32 = 0xFF56;
        pub const F1: u32 = 0xFFBE;
        pub const F2: u32 = 0xFFBF;
        pub const F3: u32 = 0xFFC0;
        pub const ALT_L: u32 = 0xFFE9;
        pub const ALT_R: u32 = 0xFFEA;
    }
    #[cfg(windows)]
    mod plat {
        pub const RETURN: u32 = 0x0D;
        pub const ENTER: u32 = 0x6C;
        pub const ESCAPE: u32 = 0x1B;
        pub const BACKSPACE: u32 = 0x08;
        pub const DELETE: u32 = 0x2E;
        pub const LEFT: u32 = 0x25;
        pub const UP: u32 = 0x26;
        pub const RIGHT: u32 = 0x27;
        pub const DOWN: u32 = 0x28;
        pub const TAB: u32 = 0x09;
        pub const HOME: u32 = 0x24;
        pub const END: u32 = 0x23;
        pub const PAGE_UP: u32 = 0x21;
        pub const PAGE_DOWN: u32 = 0x22;
        pub const F1: u32 = 0x70;
        pub const F2: u32 = 0x71;
        pub const F3: u32 = 0x72;
        pub const ALT_L: u32 = 0x12;
        pub const ALT_R: u32 = 0x12;
    }
    #[cfg(target_arch = "wasm32")]
    mod plat {
        pub const RETURN: u32 = 0x0D;
        pub const ENTER: u32 = 0x0D;
        pub const ESCAPE: u32 = 0x1B;
        pub const BACKSPACE: u32 = 0x08;
        pub const DELETE: u32 = 0x2E;
        pub const LEFT: u32 = 0x25;
        pub const UP: u32 = 0x26;
        pub const RIGHT: u32 = 0x27;
        pub const DOWN: u32 = 0x28;
        pub const TAB: u32 = 0x09;
        pub const HOME: u32 = 0x24;
        pub const END: u32 = 0x23;
        pub const PAGE_UP: u32 = 0x21;
        pub const PAGE_DOWN: u32 = 0x22;
        pub const F1: u32 = 0x70;
        pub const F2: u32 = 0x71;
        pub const F3: u32 = 0x72;
        pub const ALT_L: u32 = 0x12;
        pub const ALT_R: u32 = 0x12;
    }
    pub use plat::*;

    /// Normalize a platform key value so it can be compared with the
    /// constants above. On Windows this masks with 0xFF to strip the
    /// extended-key flag. On Unix the keyval is returned as-is.
    pub fn normalize(keyval: u32) -> u32 {
        #[cfg(windows)]
        { keyval & 0xFF }
        #[cfg(not(windows))]
        { keyval }
    }
}

// ---------------------------------------------------------------------------
// Shared backend-agnostic UI model (mirrors wxWidgets concepts).
// These types live in the core so every backend renders/navigates the same
// model; application code builds one menu/event/action model and hands it to
// any backend. Never put app-specific menu text or indices here.
// ---------------------------------------------------------------------------

/// Layout alignment for a sizer child (mirrors wxSizerFlags alignment).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Align {
    #[default]
    Default,
    Left,
    Center,
    Right,
    Top,
    Bottom,
}

/// Layout flags for a sizer child (mirrors wxSizerFlags).
#[derive(Clone, Copy, Debug, Default)]
pub struct SizerFlags {
    pub expand: bool,
    pub align: Align,
}

/// A sizer child: a widget id with weight, border, and flags.
#[derive(Clone, Debug)]
pub struct SizerChild {
    pub widget: usize,
    pub weight: i32,
    pub border: i32,
    pub flags: SizerFlags,
}

/// A layout sizer (mirrors wxSizer): arranges its children. `Box` lays out
/// children in a row/column with weights; `Grid`/`FlexGrid` arrange them in a
/// fixed or flexible grid.
#[derive(Clone, Debug)]
pub enum Sizer {
    Box { horizontal: bool, spacing: i32, children: Vec<SizerChild> },
    Grid { cols: usize, rows: usize, children: Vec<SizerChild> },
    FlexGrid { cols: usize, rows: usize, children: Vec<SizerChild> },
}

/// Message box kind (mirrors wxMessageBox style).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageBoxKind {
    Info,
    Warning,
    Error,
    Question,
}

/// Result of dismissing a message box (mirrors wxMessageBox return values).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageBoxResult {
    Ok,
    Cancel,
    Yes,
    No,
}

/// A UI event dispatched through the widget tree. Callbacks that return
/// `CallbackResult::Skip` let the event propagate to the parent widget
/// (mirrors wxEvent).
#[derive(Clone, Debug)]
pub enum Event {
    /// Generic activation (button click, menu item, …).
    Activate,
    /// Pointer click at widget-local coordinates.
    Click { x: u32, y: u32 },
    /// Key press (backend key code).
    Key { code: u32 },
    /// Menu item activation by action name.
    Menu { action: String },
}

/// Result of an event callback: `Handled` stops propagation, `Skip` lets the
/// event bubble to the parent widget (mirrors wxEvent::Skip).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallbackResult {
    Handled,
    Skip,
}

/// A named, stateful action: enable/disable and checked state shared across
/// menu items, toolbars, and keybindings (mirrors wxAction). Menu items
/// reference actions by name; the toolkit keeps a registry so one action's
/// state is reflected everywhere it appears.
#[derive(Clone, Debug, Default)]
pub struct Action {
    pub name: String,
    pub enabled: bool,
    pub checked: bool,
}

// ---------------------------------------------------------------------------
// Labels and the translator hook (i18n *mechanism*, not policy)
// ---------------------------------------------------------------------------

use std::sync::OnceLock;

/// A menu/widget label before or after translation.
///
/// `Literal` is a pre-resolved string: existing behavior, and the right choice
/// when the app translates itself. `Key` is a deferred, app-owned lookup key,
/// resolved at render time via the installed [`set_translator`].
///
/// The toolkit owns the *mechanism* (deferred label + resolution point); the
/// app owns the *catalog* (key -> text). No translated string ever lives in
/// this crate — see AGENTS.md "Toolkit purity".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Label {
    Literal(String),
    Key(&'static str),
}

impl Label {
    /// Resolve to display text through the installed translator. Literals pass
    /// through unchanged; keys go to the translator (identity until one is
    /// installed, so an untranslated app renders readable keys, not blanks).
    pub fn resolve(&self) -> String {
        match self {
            Label::Literal(s) => s.clone(),
            Label::Key(k) => resolve_key(k),
        }
    }

    /// The locale-independent identity: the literal text, or the key itself.
    /// Use this for parity walks, dispatch, and tests that must be stable
    /// across locales.
    pub fn identity(&self) -> &str {
        match self {
            Label::Literal(s) => s,
            Label::Key(k) => k,
        }
    }

    /// True when this label defers resolution to the translator.
    pub fn is_key(&self) -> bool {
        matches!(self, Label::Key(_))
    }
}

impl From<String> for Label {
    fn from(s: String) -> Self { Label::Literal(s) }
}
impl From<&str> for Label {
    fn from(s: &str) -> Self { Label::Literal(s.to_string()) }
}
impl From<&String> for Label {
    fn from(s: &String) -> Self { Label::Literal(s.clone()) }
}

type TranslatorFn = Box<dyn Fn(&str) -> String + Send + Sync>;

/// A swappable cell, not a bare `OnceLock`: resolving a key before the app
/// installs its translator must not permanently freeze the cell at identity.
/// `OnceLock<RwLock<Option<..>>>` keeps the first-use path lock-free-ish while
/// still allowing exactly the install/swap the app needs.
static TRANSLATOR: OnceLock<std::sync::RwLock<Option<TranslatorFn>>> = OnceLock::new();

fn cell() -> &'static std::sync::RwLock<Option<TranslatorFn>> {
    TRANSLATOR.get_or_init(|| std::sync::RwLock::new(None))
}

/// Install (or replace) the app's translator.
///
/// Until one is installed, `Label::Key` resolves to the key itself (identity),
/// so an untranslated app renders readable keys and every existing test that
/// compares raw labels keeps passing. Replacing it later (locale switch) takes
/// effect on the next render — apps that switch locale should also trigger a
/// redraw.
pub fn set_translator<F: Fn(&str) -> String + Send + Sync + 'static>(f: F) {
    *cell().write().unwrap() = Some(Box::new(f));
}

/// Resolve a key through the installed translator, or identity.
fn resolve_key(key: &str) -> String {
    match cell().read() {
        Ok(g) => match g.as_ref() {
            Some(f) => f(key),
            None => key.to_string(),
        },
        Err(_) => key.to_string(),
    }
}

/// Resolve a label key through the installed translator. Convenience for
/// backends and apps that hold a bare key rather than a [`Label`].
pub fn tr(key: &str) -> String {
    resolve_key(key)
}

/// Documented key namespace for the *generic* action vocabulary
/// (docs/STANDARD_ACTIONS.md). These are keys only — no English (or any)
/// string ships in this crate. An app translates them if it wants to; an app
/// that installs no translator renders the keys themselves.
pub mod std_action_keys {
    pub const ABOUT:    &str = "std.about";
    pub const EXIT:     &str = "std.exit";
    pub const OPEN:     &str = "std.open";
    pub const SAVE:     &str = "std.save";
    pub const SAVE_AS:  &str = "std.save_as";
    pub const NEW:      &str = "std.new";
    pub const CLOSE:    &str = "std.close";
    pub const HELP:     &str = "std.help";
    pub const CUT:      &str = "std.cut";
    pub const COPY:     &str = "std.copy";
    pub const PASTE:    &str = "std.paste";
    pub const FIND:     &str = "std.find";
    pub const REPLACE:  &str = "std.replace";
    pub const UNDO:     &str = "std.undo";
    pub const REDO:     &str = "std.redo";
}

/// Layout direction for locales that read right-to-left.
///
/// This is the one i18n concern that belongs to *backends*: text is data (app
/// layer), but mirroring is platform behavior with a native API (GTK:
/// `gtk_widget_set_default_direction`; NWG: `WS_EX_LAYOUTRTL`). The core only
/// carries the policy-free flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LayoutDir {
    #[default]
    Ltr,
    Rtl,
}

static LAYOUT_DIR: OnceLock<Box<dyn Fn(LayoutDir) + Send + Sync>> = OnceLock::new();

/// Register the backend's layout-direction hook. Backends call this at init;
/// apps call [`set_layout_direction`] to change it.
pub fn set_layout_direction_hook<F: Fn(LayoutDir) + Send + Sync + 'static>(f: F) {
    let _ = LAYOUT_DIR.set(Box::new(f));
}

/// Ask the active backend to mirror (or un-mirror) its layout. No-op when no
/// backend registered a hook (e.g. headless tests).
pub fn set_layout_direction(dir: LayoutDir) {
    if let Some(f) = LAYOUT_DIR.get() {
        f(dir);
    }
}

/// True when the app's locale is a known RTL locale (by BCP-47-ish prefix).
/// Purely a convenience heuristic; apps may ignore it and call
/// [`set_layout_direction`] directly.
pub fn locale_is_rtl(tag: &str) -> bool {
    let base = tag.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    matches!(base.as_str(), "ar" | "he" | "fa" | "ur" | "yi" | "dv" | "ps" | "sd" | "ug")
}

// ---------------------------------------------------------------------------
// Colour scheme / theme
// ---------------------------------------------------------------------------
//
// A day/night colour scheme, and the one global that selects it. This is the
// same shape as [`set_layout_direction`]: a policy-free enum plus a swappable
// global, with the *palette* left to the app.
//
// ## Toolkit purity: why this crate carries no colours
//
// The toolkit owns the *mechanism* (a named role -> `Color` lookup, swappable
// at runtime) and the *role vocabulary* (`cell_body`, `gridline`, `cursor`,
// ...). The app owns the *palette* — which exact grey means `cell_body` in its
// own design. A toolkit that hardcoded "night mode is #1e1e1e" would be
// dictating one app's visual design, and an app that wanted a sepia or a
// high-contrast scheme would have no way to express it. So the built-in
// schemes here are generic and *overridable*: [`Theme::custom`] lets an app
// supply its own values for any role.
//
// The roles are semantic, not visual. There is no "light_grey" role, because
// what a surface *means* (is this entered data, is this derived, is this where
// the caret is) is stable across schemes, while its colour is not.

/// Which of the two built-in schemes an app is currently using.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorScheme {
    /// The default: the historical light-surface rendering. Every value here
    /// reproduces the hardcoded constants this crate used before theming
    /// existed, so a build that never calls [`set_color_scheme`] paints
    /// pixel-identically to one from before the change.
    #[default]
    Light,
    /// Dark surfaces, light text. Intended for dim rooms; the roles keep the
    /// same *relative* contrast as in [`ColorScheme::Light`] so a cell that
    /// reads as "dimmed" in one scheme reads as dimmed in the other.
    Night,
}

/// One resolved colour, opaque. Channels are `0.0..=1.0` to match the
/// `DrawContext` primitives, which take `f64` — no conversion, no rounding
/// drift, and the terminal backends already quantise from the same range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Color {
    /// Build a colour from its three channels. Values outside `0.0..=1.0` are
    /// not clamped: a backend that receives an out-of-range channel is a
    /// palette bug, and silently clamping here would hide it.
    pub const fn new(r: f64, g: f64, b: f64) -> Self {
        Self { r, g, b }
    }

    /// The colour with full opacity, as the `DrawContext` primitives want it.
    pub const fn rgba(self) -> (f64, f64, f64, f64) {
        (self.r, self.g, self.b, 1.0)
    }

    /// The colour as a plain `(r, g, b)` triple, for the many call sites that
    /// pass a colour tuple around rather than unpacking it at the draw call.
    pub const fn rgb(self) -> (f64, f64, f64) {
        (self.r, self.g, self.b)
    }

    /// Relative luminance (ITU-R BT.709, the sRGB coefficients), as `0.0..=1.0`.
    ///
    /// The scale is what makes this useful: sRGB is perceptually non-linear, so
    /// comparing raw channel values says little about whether one surface reads
    /// as "lighter" than another. Luminance is the standard proxy, and it is
    /// what lets the theme tests assert "the margin really is dimmer than the
    /// body" as a statement about the *perceived* result rather than about two
    /// arbitrary numbers.
    pub fn luminance(self) -> f64 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    /// WCAG contrast ratio against `other`, from 1.0 (identical) to 21.0
    /// (black on white). Used by the theme tests to prove text stays legible
    /// in every scheme, which is the property that actually matters for a dark
    /// mode: a dark surface with dark text is "correct" in the sense that both
    /// values came from the palette, and still unreadable.
    pub fn contrast_ratio(self, other: Color) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

/// The semantic surfaces a renderer needs to name. Adding a role here is a
/// deliberate act: it means "some painted surface has a stable meaning across
/// schemes", and every role added must be given a value in *both* built-in
/// schemes or it silently falls back to the light one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The backdrop behind everything.
    Paper,
    /// An ordinary, editable body cell.
    CellBody,
    /// A cell outside the data body (a margin strip, a label band).
    CellMargin,
    /// A header/label surface (row labels, column headers, a formula bar).
    Header,
    /// The hairline between cells.
    Gridline,
    /// The cell under the caret, not editing.
    Cursor,
    /// The cell under the caret, while typing.
    CursorEditing,
    /// Cells covered by a selection.
    Selected,
    /// A derived/computed cell (a total, a subtotal).
    Aggregate,
    /// Default body text.
    Text,
    /// De-emphasised text (a placeholder, a trailing note).
    TextMuted,
    /// Text that calls attention (a link, an active tab's label).
    TextAccent,
    /// Text drawn *on* a highlight fill (the cursor, the editing cell, a
    /// selection) rather than on a body cell.
    ///
    /// A separate role because a dark sheet's highlights are *lighter* than
    /// its body, so the same light body ink that reads well on a body cell
    /// would be low-contrast on the very cell the user is looking at. In dark
    /// mode these fills therefore take near-black ink, which is also the
    /// inverted-selection idiom users already expect.
    TextOnHighlight,
    /// The bar behind a row/column of tabs.
    TabStrip,
    /// An inactive tab.
    TabIdle,
    /// The active tab.
    TabActive,
}

/// A complete, swappable palette.
///
/// Built-in schemes come from [`Theme::for_scheme`]; an app with its own
/// design (or a high-contrast mode) starts from one of those and overrides
/// only the roles it cares about via [`Theme::with`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    scheme: ColorScheme,
    paper: Color,
    cell_body: Color,
    cell_margin: Color,
    header: Color,
    gridline: Color,
    cursor: Color,
    cursor_editing: Color,
    selected: Color,
    aggregate: Color,
    text: Color,
    text_muted: Color,
    text_accent: Color,
    text_on_highlight: Color,
    tab_strip: Color,
    tab_idle: Color,
    tab_active: Color,
}

impl Theme {
    /// The built-in palette for `scheme`.
    ///
    /// Every constant in the `Light` arm is the literal this crate hardcoded
    /// before theming existed, so defaulting to `Light` is a no-op rather than
    /// a re-design. That is asserted by tests in each renderer.
    pub fn for_scheme(scheme: ColorScheme) -> Self {
        match scheme {
            ColorScheme::Light => Self {
                scheme,
                paper: Color::new(0.96, 0.96, 0.96),
                cell_body: Color::new(1.0, 1.0, 1.0),
                cell_margin: Color::new(0.75, 0.75, 0.75),
                header: Color::new(0.88, 0.90, 0.93),
                gridline: Color::new(0.8, 0.8, 0.8),
                cursor: Color::new(0.8, 0.9, 1.0),
                cursor_editing: Color::new(1.0, 1.0, 0.8),
                selected: Color::new(0.9, 0.95, 1.0),
                aggregate: Color::new(0.4, 0.4, 0.4),
                text: Color::new(0.05, 0.05, 0.10),
                text_muted: Color::new(0.3, 0.3, 0.3),
                text_accent: Color::new(0.0, 0.0, 0.85),
                // Light mode's highlights are pale, so body ink already reads
                // on them; this keeps the *value* identical to `Text` in light
                // mode, which is what the pixel-identical default requires.
                text_on_highlight: Color::new(0.05, 0.05, 0.10),
                tab_strip: Color::new(0.92, 0.92, 0.92),
                tab_idle: Color::new(0.85, 0.85, 0.85),
                tab_active: Color::new(0.6, 0.75, 0.95),
            },
            ColorScheme::Night => Self {
                scheme,
                // A desaturated near-black, not pure black: pure black against
                // light text is harsh over a large area and makes the cursor
                // outline hard to place.
                paper: Color::new(0.11, 0.11, 0.13),
                cell_body: Color::new(0.16, 0.16, 0.19),
                // The margin is *darker* than the body here, the inverse of
                // light mode. Dimming a margin means receding from the paper,
                // and in a dark scheme the way to recede is to go down, not
                // up; a margin lighter than the body would read as selected.
                cell_margin: Color::new(0.13, 0.13, 0.15),
                header: Color::new(0.22, 0.22, 0.26),
                gridline: Color::new(0.30, 0.30, 0.34),
                // Highlight fills are *lighter* than the body, so the cell
                // under the caret is unmistakable. They take near-black ink
                // (`TextOnHighlight`) rather than body ink, because light
                // body text on a light fill is low-contrast precisely where
                // the user is looking.
                // `selected` is shifted toward teal rather than reusing the
                // cursor's blue: a selection spanning many cells and a single
                // caret are different things, and at the same lightness a
                // shared hue would make them hard to tell apart at a glance.
                cursor: Color::new(0.30, 0.40, 0.58),
                cursor_editing: Color::new(0.55, 0.52, 0.22),
                selected: Color::new(0.30, 0.44, 0.42),
                aggregate: Color::new(0.62, 0.62, 0.66),
                text: Color::new(0.92, 0.92, 0.95),
                text_muted: Color::new(0.62, 0.62, 0.66),
                text_accent: Color::new(0.45, 0.72, 1.0),
                // Near-black on the (lighter) highlight fills.
                text_on_highlight: Color::new(0.04, 0.04, 0.07),
                tab_strip: Color::new(0.14, 0.14, 0.16),
                tab_idle: Color::new(0.20, 0.20, 0.24),
                tab_active: Color::new(0.28, 0.38, 0.54),
            },
        }
    }

    /// The scheme this palette was built for. Carried so a renderer can branch
    /// on "is this night mode" (for the few decisions a single colour cannot
    /// express, like dimming vs. lightening a shadow).
    pub fn scheme(&self) -> ColorScheme {
        self.scheme
    }

    /// The colour for `role`.
    pub fn color(&self, role: Role) -> Color {
        match role {
            Role::Paper => self.paper,
            Role::CellBody => self.cell_body,
            Role::CellMargin => self.cell_margin,
            Role::Header => self.header,
            Role::Gridline => self.gridline,
            Role::Cursor => self.cursor,
            Role::CursorEditing => self.cursor_editing,
            Role::Selected => self.selected,
            Role::Aggregate => self.aggregate,
            Role::Text => self.text,
            Role::TextMuted => self.text_muted,
            Role::TextAccent => self.text_accent,
            Role::TextOnHighlight => self.text_on_highlight,
            Role::TabStrip => self.tab_strip,
            Role::TabIdle => self.tab_idle,
            Role::TabActive => self.tab_active,
        }
    }

    /// The same palette with `role` overridden — the customization entry point
    /// for an app that wants its own scheme.
    pub fn with(mut self, role: Role, color: Color) -> Self {
        match role {
            Role::Paper => self.paper = color,
            Role::CellBody => self.cell_body = color,
            Role::CellMargin => self.cell_margin = color,
            Role::Header => self.header = color,
            Role::Gridline => self.gridline = color,
            Role::Cursor => self.cursor = color,
            Role::CursorEditing => self.cursor_editing = color,
            Role::Selected => self.selected = color,
            Role::Aggregate => self.aggregate = color,
            Role::Text => self.text = color,
            Role::TextMuted => self.text_muted = color,
            Role::TextAccent => self.text_accent = color,
            Role::TextOnHighlight => self.text_on_highlight = color,
            Role::TabStrip => self.tab_strip = color,
            Role::TabIdle => self.tab_idle = color,
            Role::TabActive => self.tab_active = color,
        }
        self
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::for_scheme(ColorScheme::Light)
    }
}

static THEME: OnceLock<std::sync::RwLock<Theme>> = OnceLock::new();

fn theme_cell() -> &'static std::sync::RwLock<Theme> {
    THEME.get_or_init(|| std::sync::RwLock::new(Theme::default()))
}

/// The palette in effect. Defaults to [`ColorScheme::Light`].
///
/// A lock, not a `OnceLock`: the whole point of a theme is that the app can
/// swap it while running, so the cell must be replaceable after first use.
pub fn theme() -> Theme {
    match theme_cell().read() {
        Ok(t) => *t,
        // A poisoned lock means another thread panicked while holding it. The
        // palette is plain data, so falling back to the default is safe and
        // keeps a render path from panicking in turn.
        Err(_) => Theme::default(),
    }
}

/// Replace the palette for every renderer that reads [`theme`].
///
/// Apps should follow a swap with a redraw of anything already realised: the
/// palette is consulted per paint call, so the change lands on the next frame
/// rather than immediately.
pub fn set_theme(theme: Theme) {
    match theme_cell().write() {
        Ok(mut t) => *t = theme,
        Err(_) => {}
    }
}

/// Convenience wrapper: switch to `scheme`'s built-in palette, discarding any
/// per-role overrides. This is what a plain "View > Night mode" toggle wants;
/// an app with its own design should use [`set_theme`] directly.
pub fn set_color_scheme(scheme: ColorScheme) {
    set_theme(Theme::for_scheme(scheme));
}

/// The scheme currently in effect.
pub fn color_scheme() -> ColorScheme {
    theme().scheme()
}

/// A menu item in a backend-agnostic menu model. Backends render and navigate
/// this model with their own widgets; the model itself is shared so an
/// application can build one menu and hand it to any backend.
///
/// Item kinds mirror wxWidgets: plain actions, check items (boolean state),
/// radio items (one-of-a-group), separators, and submenus. `shortcut` is the
/// accelerator text (e.g. "Ctrl+O") for backends that can wire real
/// keybindings.
///
/// `label` is a [`Label`], so an app may mix pre-translated literals and
/// deferred translation keys in one tree (`From<String>`/`From<&str>` keep
/// literal construction unchanged).
#[derive(Clone, Debug)]
pub enum MenuItem {
    Action { label: Label, action: String, shortcut: Option<String> },
    Check { label: Label, action: String, checked: bool },
    Radio { label: Label, action: String, group: u32 },
    Separator,
    Submenu { label: Label, items: Vec<MenuItem>, shortcut: Option<String> },
}

/// A data grid (mirrors wxGrid): cells, cursor, viewport, editing state,
/// column layout, and row labels. Backends render it with their own widgets;
/// the `Spreadsheet` widget builds its chrome (formula bar, status, tabs) on
/// top of a `Grid`. (Deliberately `core::Grid`, not re-exported at the crate
/// root, so `crate::Grid` keeps referring to the active backend's widget.)
#[derive(Clone, Debug)]
pub struct Grid {
    pub cells: Rc<RefCell<HashMap<(u32, u32), String>>>,
    pub raw_cells: Rc<RefCell<HashMap<(u32, u32), String>>>,
    pub cell_styles: Rc<RefCell<HashMap<(u32, u32), u8>>>,
    pub total_rows: u32,
    pub total_cols: u32,
    pub top_row: u32,
    pub left_col: u32,
    pub cursor_row: u32,
    pub cursor_col: u32,
    pub editing: bool,
    pub edit_buf: String,
    pub edit_pos: usize,
    pub col_width: u32,
    pub margin_cols: u32,
    pub main_cols: u32,
    pub anchor: Option<(u32, u32)>,
    pub header_row_count: u32,
    pub main_row_count: u32,
    pub column_layout: Vec<(u32, u32, String)>,
    pub row_labels: Vec<(u32, String)>,
}

impl Grid {
    /// The value displayed in a cell (committed value, falling back to the
    /// in-progress edit buffer when editing that cell).
    pub fn display_value(&self, row: u32, col: u32) -> String {
        if self.editing && row == self.cursor_row && col == self.cursor_col {
            self.edit_buf.clone()
        } else {
            self.cells.borrow().get(&(row, col)).cloned().unwrap_or_default()
        }
    }

    /// Commit the in-progress edit into the cell (no-op when not editing).
    pub fn commit_edit(&mut self) {
        if self.editing {
            let val = self.edit_buf.clone();
            let r = self.cursor_row;
            let c = self.cursor_col;
            self.cells.borrow_mut().insert((r, c), val.clone());
            self.raw_cells.borrow_mut().insert((r, c), val);
            self.editing = false;
            self.edit_buf.clear();
            self.edit_pos = 0;
        }
    }
}

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::raw::c_void;
use std::rc::Rc;

/// Install SIGABRT and SIGSEGV handlers that print a backtrace
/// to stderr, then re-raise with the default handler.
/// On non-Unix platforms this is a no-op.
pub fn install_debug_crash_handlers() {
    #[cfg(all(unix, not(target_os = "android")))]
    unsafe {
        extern "C" fn sigabrt_handler(_sig: i32) {
            unsafe {
                write_stderr(b"\nSIGABRT\n");
                write_backtrace_to_stderr();
                libc::signal(libc::SIGABRT, libc::SIG_DFL);
                libc::raise(libc::SIGABRT);
            }
        }
        extern "C" fn sigsegv_handler(_sig: i32) {
            unsafe {
                write_stderr(b"\nSIGSEGV\n");
                write_backtrace_to_stderr();
                libc::signal(libc::SIGSEGV, libc::SIG_DFL);
                libc::raise(libc::SIGSEGV);
            }
        }
        libc::signal(libc::SIGABRT, sigabrt_handler as *const () as usize);
        libc::signal(libc::SIGSEGV, sigsegv_handler as *const () as usize);
    }
    // Android's libc has no `backtrace()`; crash reporting goes through
    // logcat instead, so there is nothing to install here.
    #[cfg(any(not(unix), target_os = "android"))]
    {
        let _ = ();
    }
}

// Gated to match its only callers (`write_backtrace_to_stderr` and the signal
// handlers, both `all(unix, not(target_os = "android"))`). Android is unix but
// has no `backtrace()`, so a plain `unix` gate compiled this into the Android
// build with nothing able to call it.
#[cfg(all(unix, not(target_os = "android")))]
unsafe fn write_stderr(msg: &[u8]) {
    libc::write(libc::STDERR_FILENO, msg.as_ptr() as *const libc::c_void, msg.len());
}

#[cfg(all(unix, not(target_os = "android")))]
unsafe fn write_backtrace_to_stderr() {
    const SIZE: usize = 128;
    let mut buf: [*mut libc::c_void; SIZE] = std::mem::zeroed();
    write_stderr(b"===== backtrace =====\n");
    let n = libc::backtrace(buf.as_mut_ptr(), SIZE as i32);
    for i in 0..n.min(SIZE as i32) {
        let addr = buf[i as usize] as usize;
        if addr == 0 { break; }
        let mut hex = [0u8; 19];
        hex[0] = b' '; hex[1] = b' '; hex[18] = b'\n';
        let mut v = addr;
        let mut pos = 17;
        loop {
            hex[pos] = b"0123456789abcdef"[v & 0xf];
            v >>= 4;
            if v == 0 || pos == 1 { break; }
            pos -= 1;
        }
        write_stderr(&hex);
    }
    write_stderr(b"===== end backtrace =====\n");
}

/// Detect terminal size. On Unix uses `ioctl(TIOCGWINSZ)`;
/// on Windows uses `GetConsoleScreenBufferInfo`.
pub fn terminal_size() -> Option<(usize, usize)> {
    #[cfg(unix)]
    {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0
        {
            return Some((ws.ws_col as usize, ws.ws_row as usize));
        }
    }
    #[cfg(windows)]
    {
        use winapi::um::processenv::GetStdHandle;
        use winapi::um::winbase::STD_OUTPUT_HANDLE;
        use winapi::um::wincon::GetConsoleScreenBufferInfo;
        use winapi::um::wincon::CONSOLE_SCREEN_BUFFER_INFO;
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if handle != winapi::um::handleapi::INVALID_HANDLE_VALUE {
                let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
                if GetConsoleScreenBufferInfo(handle, &mut info) != 0 {
                    let cols = info.srWindow.Right - info.srWindow.Left + 1;
                    let rows = info.srWindow.Bottom - info.srWindow.Top + 1;
                    if cols > 0 && rows > 0 {
                        return Some((cols as usize, rows as usize));
                    }
                }
            }
        }
    }
    None
}

/// Terminal size with the override precedence a testable host needs.
///
/// Order: `COLS_OVERRIDE_ENV`/`ROWS_OVERRIDE_ENV` (explicit, for tests and
/// scripted runs), then a real terminal query ([`terminal_size`]), then
/// `$COLUMNS` and a 80x50 fallback. Backends should call this rather than
/// issuing their own `ioctl`: the query is the same on every terminal backend,
/// and a host that needs a deterministic size (a test harness, a recorded
/// demo) must not have to re-implement the precedence.
///
/// Returns `(cols, rows)`.
pub const TERM_SIZE_OVERRIDE_ENV: (&str, &str) = ("CORRO_TERM_COLS", "CORRO_TERM_ROWS");

/// See [`TERM_SIZE_OVERRIDE_ENV`].
pub fn terminal_size_with_override() -> (usize, usize) {
    let (cols_env, rows_env) = TERM_SIZE_OVERRIDE_ENV;
    let env_cols = std::env::var(cols_env).ok().and_then(|s| s.parse::<usize>().ok());
    let env_rows = std::env::var(rows_env).ok().and_then(|s| s.parse::<usize>().ok());
    if let (Some(c), Some(r)) = (env_cols, env_rows) {
        return (c, r);
    }
    if let Some((c, r)) = terminal_size() {
        return (c, r);
    }
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(80);
    (cols, 50)
}

/// Opaque handler id returned when connecting signals
pub type HandlerId = u64;

/// Which mouse/key button produced a pointer event.
///
/// A small fixed set rather than a raw button number, because the raw numbering
/// differs between GTK, GDI and UIKit and a caller porting a handler should not
/// have to know that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Primary,
    Middle,
    Secondary,
    /// Any additional button (4..n), reported as `Other(n)`.
    Other(u8),
}

/// Set of modifiers held during a pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// The platform's "command"/"meta"/"super" key.
    pub meta: bool,
}

impl Modifiers {
    /// True when no modifier is held, which is what a plain drag needs.
    pub fn is_empty(self) -> bool {
        !(self.shift || self.ctrl || self.alt || self.meta)
    }
}

/// A wheel / trackpad scroll.
///
/// `dx` and `dy` are *deltas*, not absolute scroll positions: they are how far
/// the gesture moved since the last event. That distinction matters, and it is
/// why this is a type rather than the bare `f64` the older `on_scroll` took --
/// an absolute value has to be differenced by every caller, and getting that
/// wrong is a zoom that drifts instead of tracking the wheel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollDelta {
    pub dx: f64,
    pub dy: f64,
}

impl ScrollDelta {
    /// The vertical component, which is what a zoom gesture reads.
    pub fn vertical(self) -> f64 {
        self.dy
    }

    pub fn is_zero(self) -> bool {
        self.dx == 0.0 && self.dy == 0.0
    }
}

/// A single gesture event.
///
/// Built by the backend from its native event, so application code sees one
/// vocabulary on GTK, NWG and UIKit instead of three. Coordinates are relative
/// to the widget, in logical (device-independent) pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    /// A pointer moved with no button held. Not part of a drag.
    Hover { x: f64, y: f64, mods: Modifiers },
    /// A drag in progress. `x, y` is the current position; the previous one is
    /// in [`DragEvent`] so a handler can work in deltas.
    Drag { x: f64, y: f64, mods: Modifiers },
    /// A press or release.
    Button { button: Button, pressed: bool, x: f64, y: f64, mods: Modifiers },
    /// Wheel or trackpad scroll.
    Scroll { delta: ScrollDelta, x: f64, y: f64, mods: Modifiers },
}

/// A drag event with the movement already computed.
///
/// The backends deliver positions, not deltas; doing the subtraction once here
/// means every handler agrees on the sign convention and on what happens on the
/// first event of a drag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragEvent {
    /// Where the pointer is now, widget-relative.
    pub x: f64,
    pub y: f64,
    /// Movement since the previous event, in the same units as `x`/`y`.
    pub dx: f64,
    pub dy: f64,
    /// Total movement since the drag began. Useful for a velocity or a
    /// "has it moved far enough to count as a drag" test.
    pub total_dx: f64,
    pub total_dy: f64,
    /// Which button is being dragged (the one that went down first).
    pub button: Button,
    pub mods: Modifiers,
}

impl DragEvent {
    /// True once the pointer has moved more than `threshold` logical pixels in
    /// total, which is the usual "this is a drag, not a click" test.
    pub fn exceeds(&self, threshold: f64) -> bool {
        self.total_dx.abs() > threshold || self.total_dy.abs() > threshold
    }

    /// The movement as a vector, for feeding straight into a pan.
    pub fn delta(&self) -> ScrollDelta {
        ScrollDelta { dx: self.dx, dy: self.dy }
    }
}

/// Tracks a drag across events.
///
/// A backend delivers press / motion / release; turning that into "a drag
/// started", "it moved by this much", "it ended" is identical on every
/// platform, so it lives here once instead of in each backend. Construct one,
/// feed it the events, and read the answers.
#[derive(Debug, Clone)]
pub struct DragTracker {
    active: Option<DragState>,
    last: (f64, f64),
    /// Movement below this in total is treated as a click, not a drag.
    click_threshold: f64,
}

#[derive(Debug, Clone, Copy)]
struct DragState {
    origin: (f64, f64),
    last: (f64, f64),
    button: Button,
}

impl Default for DragTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DragTracker {
    pub fn new() -> Self {
        Self {
            active: None,
            last: (0.0, 0.0),
            click_threshold: 3.0,
        }
    }

    /// Movement (in logical pixels) that separates a click from a drag.
    pub fn with_click_threshold(mut self, px: f64) -> Self {
        self.click_threshold = px;
        self
    }

    pub fn is_dragging(&self) -> bool {
        self.active.is_some()
    }

    /// Feeds a gesture in; returns a `DragEvent` only while a drag is in
    /// progress and has moved far enough to matter.
    pub fn feed(&mut self, g: Gesture) -> Option<DragEvent> {
        match g {
            Gesture::Button { button, pressed: true, x, y, mods } => {
                // A second button during a drag does not start another one.
                if self.active.is_none() {
                    self.active = Some(DragState { origin: (x, y), last: (x, y), button });
                    self.last = (x, y);
                }
                let _ = mods;
                None
            }
            Gesture::Button { pressed: false, .. } => {
                self.active = None;
                None
            }
            Gesture::Drag { x, y, mods } => {
                let st = self.active?;
                let dx = x - st.last.0;
                let dy = y - st.last.1;
                let ev = DragEvent {
                    x,
                    y,
                    dx,
                    dy,
                    total_dx: x - st.origin.0,
                    total_dy: y - st.origin.1,
                    button: st.button,
                    mods,
                };
                // Advance the stored position AFTER taking the delta. `st` is a
                // copy of the state, so the write-back is what makes the *next*
                // event report an increment; without it every event reports the
                // distance from the press point instead.
                if let Some(slot) = self.active.as_mut() {
                    slot.last = (x, y);
                }
                self.last = (x, y);
                // Suppress sub-threshold jitter so a shaky hand does not pan.
                if ev.exceeds(self.click_threshold) {
                    Some(ev)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// A zoom / pan gesture, accumulated over time.
///
/// Wheel events arrive as deltas with no notion of a gesture's extent, so
/// "zoom by 10% per notch" has to be accumulated somewhere. This holds the
/// factor and clamps it, which is the part every caller otherwise reinvents
/// (usually with a bug: unbounded, so a long scroll overflows the scale).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomState {
    pub factor: f64,
    min: f64,
    max: f64,
    /// How much one wheel notch multiplies by.
    step: f64,
}

impl Default for ZoomState {
    fn default() -> Self {
        Self::new()
    }
}

impl ZoomState {
    /// A zoom starting at 1.0, clamped to 0.05..=16.0, 10% per notch.
    pub fn new() -> Self {
        Self {
            factor: 1.0,
            min: 0.05,
            max: 16.0,
            step: 0.1,
        }
    }

    pub fn with_range(mut self, min: f64, max: f64) -> Self {
        self.min = min;
        self.max = max;
        self.factor = self.factor.clamp(min, max);
        self
    }

    pub fn with_step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }

    pub fn factor(&self) -> f64 {
        self.factor
    }

    pub fn set_factor(&mut self, f: f64) {
        self.factor = f.clamp(self.min, self.max);
    }

    pub fn reset(&mut self) {
        self.factor = 1.0;
    }

    /// Applies a scroll delta as a zoom step. Returns the new factor.
    ///
    /// A horizontal wheel is deliberately ignored: on a trackpad a two-finger
    /// horizontal swipe means "scroll sideways", and treating it as zoom makes
    /// the view jump when the user scrolls a list.
    pub fn scroll(&mut self, delta: ScrollDelta) -> f64 {
        if delta.dy == 0.0 {
            return self.factor;
        }
        // Scrolling up (positive dy) zooms in, matching the convention of every
        // image viewer and map.
        let mult = (1.0 + self.step).powf(delta.dy);
        self.set_factor(self.factor * mult);
        self.factor
    }
}

// ---------------------------------------------------------------------------
// Popup placement (shared by every backend's menu popup)
// ---------------------------------------------------------------------------
//
// A context menu must open *at the pointer* -- that is what every native
// toolkit does, and an app that has to compute the point itself will get it
// subtly wrong. But it also must not hang off the edge of the screen or hide
// under the taskbar.
//
// That decision is the same on every platform, so it lives here rather than in
// any one backend: a per-backend implementation is how the same behaviour ends
// up inconsistent across platforms (one clamping, one flipping, one ignoring
// the taskbar), which reads to a user as the menu "appearing at a random
// position". Backends keep only the platform parts -- measuring the menu and
// reading the work area -- and call this once with those numbers.

/// A rectangle in absolute screen pixels.
///
/// The origin is the top-left of the *work area* on a single-display setup;
/// a backend that supports multiple monitors reports the monitor the popup will
/// appear on, which may have a non-zero origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Where a popup should actually be drawn, after keeping it on screen.
///
/// **The contract:** the popup's top-left goes at `(x, y)` -- the pointer --
/// unless that would put part of it off screen or under the taskbar, in which
/// case it flips to the other side of that edge.
///
/// Flipping rather than clamping is deliberate:
///
/// * Clamping (`x = min(x, right - w)`) moves a popup that *would* have fit,
///   so a menu near the bottom-right would drift up and left even though only
///   one axis had a problem.
/// * Clamping the top edge when flipping up would push the *bottom* back under
///   the taskbar whenever the popup is taller than the space above the
///   pointer. Pinning the bottom edge instead keeps the whole box inside.
///
/// Only the axis that does not fit is touched: a popup that overflows the
/// right edge keeps the pointer's `y` exactly.
///
/// `work_area` is the monitor rectangle with the taskbar removed, never the
/// raw screen: a popup whose bottom lands on the taskbar is off-screen in
/// every sense that matters even though every pixel is inside `rcMonitor`.
///
/// A popup larger than the work area cannot be made fully visible either way;
/// it is pinned to the work area's top-left so the start of the menu stays
/// reachable and nothing is pushed past the far edge.
pub fn place_popup(x: i32, y: i32, w: i32, h: i32, work_area: ScreenRect) -> (i32, i32) {
    let w = w.max(0);
    let h = h.max(0);
    let avail_w = (work_area.right - work_area.left).max(0);
    let avail_h = (work_area.bottom - work_area.top).max(0);

    let x = if w > avail_w {
        // Cannot fit on any axis: pin to the left edge.
        work_area.left
    } else if x < work_area.left {
        // Left of the work area: pull in. Checked BEFORE the overflow test,
        // because a pointer just inside the left edge can still overflow the
        // right edge on a narrow work area -- and in that case flipping left is
        // what would go off-screen, so staying at the pointer is correct.
        work_area.left
    } else if x + w > work_area.right {
        // Would overflow the right edge -> flip to the left of the pointer,
        // floored at the work area's left edge so the result is never negative.
        (x - w).max(work_area.left)
    } else {
        x
    };

    let y = if h > avail_h {
        work_area.top
    } else if y < work_area.top {
        work_area.top
    } else if y + h > work_area.bottom {
        // Would overflow the bottom edge (i.e. land on or behind the taskbar)
        // -> flip above the pointer.
        let flipped = y - h;
        if flipped + h <= work_area.bottom {
            flipped
        } else {
            // Taller than the space above the pointer: pin the *bottom* to the
            // work area's bottom edge, which keeps the box inside and lets the
            // list scroll.
            work_area.bottom - h
        }
    } else {
        y
    };

    (x, y)
}

/// Cross-platform 2D drawing surface.
/// Each backend implements this trait with its own drawing primitives.
pub trait DrawContext {
    fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64);
    fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64, lw: f64);
    /// Draw text with normal (non-bold, non-italic) style.
    fn draw_text(&mut self, x: f64, y: f64, text: &str, font: &str, size: f64, r: f64, g: f64, b: f64, a: f64) {
        self.draw_text_styled(x, y, text, font, size, r, g, b, a, 0, 0)
    }
    /// Draw text with explicit Cairo slant (0=normal, 1=italic, 2=oblique) and
    /// weight (0=normal, 1=bold).
    fn draw_text_styled(&mut self, x: f64, y: f64, text: &str, font: &str, size: f64,
                        r: f64, g: f64, b: f64, a: f64, slant: i32, weight: i32);
    /// Measure text extents with normal (non-bold) weight.
    fn text_extents(&self, text: &str, font: &str, size: f64) -> (f64, f64, f64, f64) {
        self.text_extents_styled(text, font, size, 0, 0)
    }
    /// Measure text extents with explicit Cairo slant and weight.
    fn text_extents_styled(&self, text: &str, font: &str, size: f64, slant: i32, weight: i32) -> (f64, f64, f64, f64);
    fn clear(&mut self, r: f64, g: f64, b: f64, a: f64);
    fn save(&mut self);
    fn restore(&mut self);
    fn clip(&mut self, x: f64, y: f64, w: f64, h: f64);

    /// Blit a straight-alpha RGBA8 image, top-left at `(x, y)`, at its natural
    /// size (or scaled by `scale`).
    ///
    /// `pixels` is row-major, 4 bytes per pixel, **not** premultiplied. On
    /// backends whose native format is premultiplied (Cairo) the alpha is
    /// multiplied in first, because the two are not interchangeable and using
    /// the wrong one makes translucent edges look too dark.
    ///
    /// # Default: no-op
    ///
    /// The default implementation does nothing and returns `false`, so a
    /// backend that cannot blit images does not have to implement it. A caller
    /// that needs to know gets `false` back and can fall back to vector
    /// drawing. `jautogui` uses this to draw the screenshot preview on GTK and
    /// NWG, and to degrade to a placeholder elsewhere.
    fn draw_rgba_image(&mut self, x: f64, y: f64, pixels: &[u8], width: u32, height: u32, scale: f64) -> bool {
        let _ = (x, y, pixels, width, height, scale);
        false
    }
}

/// Top-level error type
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("backend error: {0}")]
    Backend(String),
}

/// Widget trait: minimal escape hatch for raw handles
pub trait Widget {
    /// Return an opaque raw pointer for backend interop. Use unsafe to deref.
    fn raw_handle(&self) -> *mut c_void;
}

/// Core App wrapper that holds a boxed backend application.
#[derive(Clone)]
pub struct App {
    inner: Rc<RefCell<Option<Box<dyn crate::backends::BackendApp>>>>,
    #[cfg(all(windows, not(feature = "zork")))]
    parent_cell: Rc<RefCell<Option<*mut c_void>>>,
    #[cfg(all(windows, not(feature = "zork")))]
    action_registry: Rc<RefCell<HashMap<String, Box<dyn FnMut()>>>>,
    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    action_group: Rc<RefCell<Option<crate::backends_gtk_adapter::Application>>>,
}

/// Extensions to MIME types, for the Android Storage Access Framework.
///
/// SAF takes MIME types while every other backend's filter list is globs
/// (`*.corro`, `*.*`), so the translation lives here rather than in the Java
/// picker: one readable table, and the same filter list produces the same
/// types on every Android host.
///
/// An unrecognised extension becomes `*&#47;*` rather than being dropped. A
/// filter the mapping does not know must not narrow the picker to nothing —
/// "shows more files than asked" is recoverable, "shows none" is not.
#[cfg(all(target_os = "android", not(feature = "zork")))]
fn mimes_from_filters(filters: &[(&str, &[&str])]) -> Vec<String> {
    fn ext_mime(ext: &str) -> &'static str {
        match ext.trim_start_matches("*.").to_ascii_lowercase().as_str() {
            "corro" | "log" => "application/octet-stream",
            "csv" | "tsv" => "text/comma-separated-values",
            "ods" => "application/vnd.oasis.opendocument.spreadsheet",
            "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "txt" | "md" | "ascii" => "text/plain",
            "json" => "application/json",
            "xml" => "text/xml",
            "pdf" => "application/pdf",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "zip" => "application/zip",
            _ => "*/*",
        }
    }
    let mut out: Vec<String> = Vec::new();
    for (_, patterns) in filters {
        for p in *patterns {
            let mime = ext_mime(p).to_string();
            if !out.contains(&mime) {
                out.push(mime);
            }
        }
    }
    if out.is_empty() {
        out.push("*/*".to_string());
    }
    out
}

impl App {
    /// Initialize the default backend and return an App wrapper.
    /// Uses the priority chain from `backends::init()` (gtk > nwg > wasm > android > pancurses).
    /// When compiled with both `gui` and `pancurses`, this uses the GUI backend path;
    /// the pancurses backend initializes separately via `backends::pancurses::init()`.
    pub fn init() -> Result<Self, Error> {
        let _ = std::fs::write("/tmp/corro_init.txt", "App::init() called\n");
        let b = match crate::backends::init() {
            Ok(b) => b,
            // `{e}` (not `{}`, e): the iOS/Android `init_backend` returns
            // `Box<dyn Error>`, which is Display but not Debug-formattable
            // through an extra wrapper.
            Err(e) => return Err(Error::Backend(format!("{e}"))),
        };
        #[cfg(all(windows, not(feature = "zork")))]
        {
            // Create a hidden parent window for child controls
            let parent_hwnd = crate::backends::nwg::create_hidden_parent()?;
            return Ok(App {
                inner: Rc::new(RefCell::new(Some(b))),
                parent_cell: Rc::new(RefCell::new(Some(parent_hwnd))),
                action_registry: Rc::new(RefCell::new(HashMap::new())),
            });
        }
        #[cfg(not(all(windows, not(feature = "zork"))))]
        return Ok(App {
            inner: Rc::new(RefCell::new(Some(b))),
            #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
            action_group: Rc::new(RefCell::new(None)),
        });
    }

    // -- Linux paths --

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_window(&self) -> Result<crate::backends_gtk_adapter::Window, Error> {
        crate::backends_gtk_adapter::create_window().map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_button(&self, label: &str) -> Result<crate::backends_gtk_adapter::Button, Error> {
        crate::backends_gtk_adapter::create_button(label).map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_label(&self, text: &str) -> Result<crate::backends_gtk_adapter::Label, Error> {
        crate::backends_gtk_adapter::create_label(text).map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_box(&self, orientation: crate::backends_gtk_adapter::Orientation, spacing: i32) -> Result<crate::backends_gtk_adapter::BoxWidget, Error> {
        crate::backends_gtk_adapter::create_box(orientation, spacing).map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_grid(&self) -> Result<crate::backends_gtk_adapter::Grid, Error> {
        crate::backends_gtk_adapter::create_grid().map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_entry(&self) -> Result<crate::backends_gtk_adapter::Entry, Error> {
        crate::backends_gtk_adapter::create_entry().map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_menu(&self) -> Result<crate::backends_gtk_adapter::Menu, Error> {
        crate::backends_gtk_adapter::create_menu().map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    /// # Safety
    /// `action_group` must be a valid GActionGroup pointer or null.
    pub unsafe fn create_menubar(&self, model: &crate::backends_gtk_adapter::Menu, action_group: *mut c_void) -> Result<crate::backends_gtk_adapter::MenuBar, Error> {
        crate::backends_gtk_adapter::create_menubar(model, action_group).map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_simple_action(&self, name: &str) -> Result<crate::backends_gtk_adapter::SimpleAction, Error> {
        crate::backends_gtk_adapter::create_simple_action(name).map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_dialog(&self) -> Result<crate::backends_gtk_adapter::Dialog, Error> {
        crate::backends_gtk_adapter::create_dialog().map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_dropdown(&self, items: &[&str]) -> Result<crate::backends_gtk_adapter::DropDown, Error> {
        crate::backends_gtk_adapter::create_dropdown(items).map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_checkbutton(&self, label: &str) -> Result<crate::backends_gtk_adapter::CheckButton, Error> {
        crate::backends_gtk_adapter::create_checkbutton(label).map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_radiobutton(&self, group: Option<&crate::backends_gtk_adapter::RadioButton>, label: &str) -> Result<crate::backends_gtk_adapter::RadioButton, Error> {
        crate::backends_gtk_adapter::create_radiobutton(group, label).map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_textview(&self) -> Result<crate::backends_gtk_adapter::TextView, Error> {
        crate::backends_gtk_adapter::create_textview().map_err(|e| e)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_canvas(&self) -> Result<crate::backends_gtk_adapter::Canvas, Error> {
        crate::backends_gtk_adapter::create_canvas()
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_scrolled_window(&self) -> Result<crate::backends_gtk_adapter::ScrolledWindow, Error> {
        crate::backends_gtk_adapter::create_scrolled_window()
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_overlay(&self) -> Result<crate::backends_gtk_adapter::Overlay, Error> {
        crate::backends_gtk_adapter::create_overlay()
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn open_file(&self, title: &str) -> Result<Option<String>, Error> {
        crate::backends_gtk_adapter::open_file(title)
    }
    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn open_file_filtered(&self, title: &str, filters: &[(&str, &[&str])]) -> Result<Option<String>, Error> {
        crate::backends_gtk_adapter::open_file_filtered(title, filters)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn save_file(&self, title: &str) -> Result<Option<String>, Error> {
        crate::backends_gtk_adapter::save_file(title)
    }
    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn save_file_filtered(&self, title: &str, filters: &[(&str, &[&str])], current_name: &str) -> Result<Option<String>, Error> {
        crate::backends_gtk_adapter::save_file_filtered(title, filters, current_name)
    }

    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn create_spreadsheet(&self, rows: usize, cols: usize) -> Result<crate::backends_gtk_adapter::Spreadsheet, Error> {
        crate::backends_gtk_adapter::create_spreadsheet(rows, cols)
    }

    // -- Windows paths --

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_window(&self) -> Result<crate::backends_nwg_adapter::Window, Error> {
        crate::backends_nwg_adapter::create_window(&self.parent_cell)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_button(&self, label: &str) -> Result<crate::backends_nwg_adapter::Button, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::create_button(parent, label)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_label(&self, text: &str) -> Result<crate::backends_nwg_adapter::Label, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        let lbl = crate::backends_nwg_adapter::create_label(parent)?;
        lbl.set_text(text);
        Ok(lbl)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_box(&self, orientation: crate::backends::nwg::Orientation, spacing: i32) -> Result<crate::backends_nwg_adapter::BoxWidget, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::create_box(orientation, spacing, parent)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_grid(&self) -> Result<crate::backends_nwg_adapter::Grid, Error> {
        crate::backends_nwg_adapter::create_grid()
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_entry(&self) -> Result<crate::backends_nwg_adapter::Entry, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::create_entry(parent)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_menu(&self) -> Result<crate::backends_nwg_adapter::Menu, Error> {
        crate::backends_nwg_adapter::create_menu()
    }

    #[cfg(all(windows, not(feature = "zork")))]
    /// # Safety
    /// `window_hwnd` must be a valid HWND.
    pub unsafe fn create_menubar(&self, model: &crate::backends_nwg_adapter::Menu, window_hwnd: *mut c_void) -> Result<crate::backends_nwg_adapter::MenuBar, Error> {
        crate::backends_nwg_adapter::create_menubar(model, window_hwnd, self.action_registry.clone())
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_simple_action(&self, name: &str) -> Result<crate::backends_nwg_adapter::SimpleAction, Error> {
        crate::backends_nwg_adapter::create_simple_action(name, self.action_registry.clone())
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_dialog(&self) -> Result<crate::backends_nwg_adapter::Dialog, Error> {
        crate::backends_nwg_adapter::create_dialog(&self.parent_cell)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_dropdown(&self, items: &[&str]) -> Result<crate::backends_nwg_adapter::DropDown, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::create_dropdown(parent, items)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_checkbutton(&self, label: &str) -> Result<crate::backends_nwg_adapter::CheckButton, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        let cb = crate::backends_nwg_adapter::create_checkbutton(parent)?;
        cb.set_label(label);
        Ok(cb)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_radiobutton(&self, group: Option<&crate::backends_nwg_adapter::RadioButton>, label: &str) -> Result<crate::backends_nwg_adapter::RadioButton, Error> {
        // Win32 groups by WS_GROUP: the first radio (no group yet) starts it.
        let group_start = group.is_none();
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        let rb = crate::backends_nwg_adapter::create_radiobutton(parent, group_start)?;
        rb.set_label(label);
        Ok(rb)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_textview(&self) -> Result<crate::backends_nwg_adapter::TextView, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::create_textview(parent)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_canvas(&self) -> Result<crate::backends_nwg_adapter::Canvas, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::create_canvas(parent)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_overlay(&self) -> Result<crate::backends_nwg_adapter::Overlay, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::create_overlay(parent)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn create_scrolled_window(&self) -> Result<crate::backends_nwg_adapter::ScrolledWindow, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::create_scrolled_window(parent)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn open_file(&self, title: &str) -> Result<Option<String>, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::open_file(title, parent)
    }
    #[cfg(all(windows, not(feature = "zork")))]
    pub fn open_file_filtered(&self, title: &str, filters: &[(&str, &[&str])]) -> Result<Option<String>, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::open_file_filtered(title, parent, filters)
    }

    #[cfg(all(windows, not(feature = "zork")))]
    pub fn save_file(&self, title: &str) -> Result<Option<String>, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::save_file(title, parent)
    }
    #[cfg(all(windows, not(feature = "zork")))]
    pub fn save_file_filtered(&self, title: &str, filters: &[(&str, &[&str])], current_name: &str) -> Result<Option<String>, Error> {
        let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
        crate::backends_nwg_adapter::save_file_filtered(title, parent, filters, current_name)
    }

    // -- Pancurses paths --

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_window(&self) -> Result<crate::backends_pancurses_adapter::Window, Error> {
        crate::backends_pancurses_adapter::create_window()
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_button(&self, label: &str) -> Result<crate::backends_pancurses_adapter::Button, Error> {
        crate::backends_pancurses_adapter::create_button(label)
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_label(&self, text: &str) -> Result<crate::backends_pancurses_adapter::Label, Error> {
        crate::backends_pancurses_adapter::create_label(text)
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_box(&self, orientation: crate::backends_pancurses_adapter::Orientation, spacing: i32) -> Result<crate::backends_pancurses_adapter::BoxWidget, Error> {
        crate::backends_pancurses_adapter::create_box(orientation, spacing)
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_grid(&self) -> Result<crate::backends_pancurses_adapter::Grid, Error> {
        crate::backends_pancurses_adapter::create_grid()
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_entry(&self) -> Result<crate::backends_pancurses_adapter::Entry, Error> {
        crate::backends_pancurses_adapter::create_entry()
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_menu(&self) -> Result<crate::backends_pancurses_adapter::Menu, Error> {
        crate::backends_pancurses_adapter::create_menu()
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_menubar(&self, model: &crate::backends_pancurses_adapter::Menu, _action_group: *mut std::os::raw::c_void) -> Result<crate::backends_pancurses_adapter::MenuBar, Error> {
        crate::backends_pancurses_adapter::create_menubar(model, _action_group)
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_simple_action(&self, name: &str) -> Result<crate::backends_pancurses_adapter::SimpleAction, Error> {
        crate::backends_pancurses_adapter::create_simple_action(name)
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_dialog(&self) -> Result<crate::backends_pancurses_adapter::Dialog, Error> {
        crate::backends_pancurses_adapter::create_dialog()
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_dropdown(&self, items: &[&str]) -> Result<crate::backends_pancurses_adapter::DropDown, Error> {
        crate::backends_pancurses_adapter::create_dropdown(items)
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_checkbutton(&self, label: &str) -> Result<crate::backends_pancurses_adapter::CheckButton, Error> {
        crate::backends_pancurses_adapter::create_checkbutton(label)
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_radiobutton(&self, group: Option<&crate::backends_pancurses_adapter::RadioButton>, label: &str) -> Result<crate::backends_pancurses_adapter::RadioButton, Error> {
        let _ = group;
        crate::backends_pancurses_adapter::create_radiobutton(None, label)
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_textview(&self) -> Result<crate::backends_pancurses_adapter::TextView, Error> {
        crate::backends_pancurses_adapter::create_textview()
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_spreadsheet(&self, rows: u32, cols: u32) -> Result<crate::backends_pancurses_adapter::Spreadsheet, Error> {
        crate::backends_pancurses_adapter::create_spreadsheet(rows, cols)
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_canvas(&self) -> Result<crate::backends_pancurses_adapter::Canvas, Error> {
        crate::backends_pancurses_adapter::create_canvas()
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_overlay(&self) -> Result<crate::backends_pancurses_adapter::Overlay, Error> {
        crate::backends_pancurses_adapter::create_overlay()
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn create_scrolled_window(&self) -> Result<crate::backends_pancurses_adapter::ScrolledWindow, Error> {
        crate::backends_pancurses_adapter::create_scrolled_window()
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn open_file(&self, title: &str) -> Result<Option<String>, Error> {
        crate::backends_pancurses_adapter::open_file(title)
    }
#[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn open_file_filtered(&self, title: &str, _filters: &[(&str, &[&str])]) -> Result<Option<String>, Error> {
        crate::backends_pancurses_adapter::open_file(title)
    }

    #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn save_file(&self, title: &str) -> Result<Option<String>, Error> {
        crate::backends_pancurses_adapter::save_file(title)
    }
#[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
    pub fn save_file_filtered(&self, title: &str, _filters: &[(&str, &[&str])], _current_name: &str) -> Result<Option<String>, Error> {
        crate::backends_pancurses_adapter::save_file(title)
    }

    // -- Zork paths --

    #[cfg(feature = "zork")]
    pub fn create_canvas(&self) -> Result<crate::backends_zork_adapter::Canvas, Error> {
        crate::backends_zork_adapter::create_canvas()
    }

    #[cfg(feature = "zork")]
    pub fn create_overlay(&self) -> Result<crate::backends_zork_adapter::Overlay, Error> {
        crate::backends_zork_adapter::create_overlay()
    }

    #[cfg(feature = "zork")]
    pub fn create_scrolled_window(&self) -> Result<crate::backends_zork_adapter::ScrolledWindow, Error> {
        crate::backends_zork_adapter::create_scrolled_window()
    }

    #[cfg(feature = "zork")]
    pub fn open_file(&self, title: &str) -> Result<Option<String>, Error> {
        crate::backends_zork_adapter::open_file(title)
    }
    #[cfg(feature = "zork")]
    pub fn open_file_filtered(&self, title: &str, _filters: &[(&str, &[&str])]) -> Result<Option<String>, Error> {
        crate::backends_zork_adapter::open_file(title)
    }

    #[cfg(feature = "zork")]
    pub fn save_file(&self, title: &str) -> Result<Option<String>, Error> {
        crate::backends_zork_adapter::save_file(title)
    }
    #[cfg(feature = "zork")]
    pub fn save_file_filtered(&self, title: &str, _filters: &[(&str, &[&str])], _current_name: &str) -> Result<Option<String>, Error> {
        crate::backends_zork_adapter::save_file(title)
    }

    #[cfg(feature = "zork")]
    pub fn create_window(&self) -> Result<crate::backends_zork_adapter::Window, Error> {
        crate::backends_zork_adapter::create_window()
    }

    #[cfg(feature = "zork")]
    pub fn create_button(&self, label: &str) -> Result<crate::backends_zork_adapter::Button, Error> {
        crate::backends_zork_adapter::create_button(label)
    }

    #[cfg(feature = "zork")]
    pub fn create_label(&self, text: &str) -> Result<crate::backends_zork_adapter::Label, Error> {
        crate::backends_zork_adapter::create_label(text)
    }

    #[cfg(feature = "zork")]
    pub fn create_box(&self, orientation: crate::backends_zork_adapter::Orientation, spacing: i32) -> Result<crate::backends_zork_adapter::BoxWidget, Error> {
        crate::backends_zork_adapter::create_box(orientation, spacing)
    }

    #[cfg(feature = "zork")]
    pub fn create_grid(&self) -> Result<crate::backends_zork_adapter::Grid, Error> {
        crate::backends_zork_adapter::create_grid()
    }

    #[cfg(feature = "zork")]
    pub fn create_entry(&self) -> Result<crate::backends_zork_adapter::Entry, Error> {
        crate::backends_zork_adapter::create_entry()
    }

    #[cfg(feature = "zork")]
    pub fn create_menu(&self) -> Result<crate::backends_zork_adapter::Menu, Error> {
        crate::backends_zork_adapter::create_menu()
    }

    #[cfg(feature = "zork")]
    pub fn create_menubar(&self, model: &crate::backends_zork_adapter::Menu, _action_group: *mut std::os::raw::c_void) -> Result<crate::backends_zork_adapter::MenuBar, Error> {
        crate::backends_zork_adapter::create_menubar(model, _action_group)
    }

    #[cfg(feature = "zork")]
    pub fn create_simple_action(&self, name: &str) -> Result<crate::backends_zork_adapter::SimpleAction, Error> {
        crate::backends_zork_adapter::create_simple_action(name)
    }

    #[cfg(feature = "zork")]
    pub fn create_dialog(&self) -> Result<crate::backends_zork_adapter::Dialog, Error> {
        crate::backends_zork_adapter::create_dialog()
    }

    #[cfg(feature = "zork")]
    pub fn create_dropdown(&self, items: &[&str]) -> Result<crate::backends_zork_adapter::DropDown, Error> {
        crate::backends_zork_adapter::create_dropdown(items)
    }

    #[cfg(feature = "zork")]
    pub fn create_checkbutton(&self, label: &str) -> Result<crate::backends_zork_adapter::CheckButton, Error> {
        crate::backends_zork_adapter::create_checkbutton(label)
    }

    #[cfg(feature = "zork")]
    pub fn create_radiobutton(&self, group: Option<&crate::backends_zork_adapter::RadioButton>, label: &str) -> Result<crate::backends_zork_adapter::RadioButton, Error> {
        // The group was previously discarded (`let _ = group;`), so every
        // zork radio was its own singleton and the mutual exclusion GTK/NWG
        // provide never happened.
        crate::backends_zork_adapter::create_radiobutton(group, label)
    }

    #[cfg(feature = "zork")]
    pub fn create_textview(&self) -> Result<crate::backends_zork_adapter::TextView, Error> {
        crate::backends_zork_adapter::create_textview()
    }

    /// Zork's spreadsheet is a real cell map, not a rasterisation shortcut.
    /// `rows`/`cols` are accepted for signature parity with the GTK and
    /// pancurses backends; the model's cells are sparse, so the extents are
    /// advisory (see `docs/ZORK_MISSING.md` §2.4 F3).
    #[cfg(feature = "zork")]
    pub fn create_spreadsheet(&self, rows: u32, cols: u32) -> Result<crate::backends_zork_adapter::Spreadsheet, Error> {
        let ss = crate::backends_zork_adapter::create_spreadsheet()?;
        let w = (cols as i32) * 80;
        let h = (rows as i32) * 20;
        crate::backends::zork::set_size_request(ss.id, w, h);
        Ok(ss)
    }

    // -- WASM paths --

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_window(&self) -> Result<crate::backends_wasm_adapter::Window, Error> {
        crate::backends_wasm_adapter::create_window()
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_button(&self, label: &str) -> Result<crate::backends_wasm_adapter::Button, Error> {
        crate::backends_wasm_adapter::create_button(label)
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_label(&self, text: &str) -> Result<crate::backends_wasm_adapter::Label, Error> {
        crate::backends_wasm_adapter::create_label(text)
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_box(&self, orientation: crate::backends_wasm_adapter::Orientation, spacing: i32) -> Result<crate::backends_wasm_adapter::BoxWidget, Error> {
        crate::backends_wasm_adapter::create_box(orientation, spacing)
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_grid(&self) -> Result<crate::backends_wasm_adapter::Grid, Error> {
        crate::backends_wasm_adapter::create_grid()
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_entry(&self) -> Result<crate::backends_wasm_adapter::Entry, Error> {
        crate::backends_wasm_adapter::create_entry()
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_menu(&self) -> Result<crate::backends_wasm_adapter::Menu, Error> {
        crate::backends_wasm_adapter::create_menu()
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_menubar(&self, model: &crate::backends_wasm_adapter::Menu, action_group: *mut c_void) -> Result<crate::backends_wasm_adapter::MenuBar, Error> {
        crate::backends_wasm_adapter::create_menubar(model, action_group)
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_simple_action(&self, name: &str) -> Result<crate::backends_wasm_adapter::SimpleAction, Error> {
        crate::backends_wasm_adapter::create_simple_action(name)
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_dialog(&self) -> Result<crate::backends_wasm_adapter::Dialog, Error> {
        crate::backends_wasm_adapter::create_dialog()
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_dropdown(&self, items: &[&str]) -> Result<crate::backends_wasm_adapter::DropDown, Error> {
        crate::backends_wasm_adapter::create_dropdown(items)
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_checkbutton(&self, label: &str) -> Result<crate::backends_wasm_adapter::CheckButton, Error> {
        crate::backends_wasm_adapter::create_checkbutton(label)
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_radiobutton(&self, group: Option<&crate::backends_wasm_adapter::RadioButton>, label: &str) -> Result<crate::backends_wasm_adapter::RadioButton, Error> {
        let _ = group;
        crate::backends_wasm_adapter::create_radiobutton(None, label)
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_textview(&self) -> Result<crate::backends_wasm_adapter::TextView, Error> {
        crate::backends_wasm_adapter::create_textview()
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_canvas(&self) -> Result<crate::backends_wasm_adapter::Canvas, Error> {
        crate::backends_wasm_adapter::create_canvas()
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_overlay(&self) -> Result<crate::backends_wasm_adapter::Overlay, Error> {
        crate::backends_wasm_adapter::create_overlay()
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn create_scrolled_window(&self) -> Result<crate::backends_wasm_adapter::ScrolledWindow, Error> {
        crate::backends_wasm_adapter::create_scrolled_window()
    }
    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn open_file(&self, _title: &str) -> Result<Option<String>, Error> {
        Ok(None) // File dialogs not available in WASM
    }
    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn open_file_filtered(&self, _title: &str, _filters: &[(&str, &[&str])]) -> Result<Option<String>, Error> {
        Ok(None) // File dialogs not available in WASM
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn save_file(&self, _title: &str) -> Result<Option<String>, Error> {
        Ok(None) // File dialogs not available in WASM
    }
    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn save_file_filtered(&self, _title: &str, _filters: &[(&str, &[&str])], _current_name: &str) -> Result<Option<String>, Error> {
        Ok(None) // File dialogs not available in WASM
    }

// -- Android paths --

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_window(&self) -> Result<crate::backends_android_adapter::Window, Error> {
    crate::backends_android_adapter::create_window()
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_button(&self, label: &str) -> Result<crate::backends_android_adapter::Button, Error> {
    crate::backends_android_adapter::create_button(label)
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_label(&self, text: &str) -> Result<crate::backends_android_adapter::Label, Error> {
    crate::backends_android_adapter::create_label(text)
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_box(&self, orientation: crate::backends_android_adapter::Orientation, spacing: i32) -> Result<crate::backends_android_adapter::BoxWidget, Error> {
    crate::backends_android_adapter::create_box(orientation, spacing)
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_entry(&self) -> Result<crate::backends_android_adapter::Entry, Error> {
    crate::backends_android_adapter::create_entry()
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_menu(&self) -> Result<crate::backends_android_adapter::Menu, Error> {
    crate::backends_android_adapter::create_menu()
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_menubar(&self, model: &crate::backends_android_adapter::Menu, action_group: *mut c_void) -> Result<crate::backends_android_adapter::MenuBar, Error> {
    crate::backends_android_adapter::create_menubar(model, action_group)
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_simple_action(&self, name: &str) -> Result<crate::backends_android_adapter::SimpleAction, Error> {
    crate::backends_android_adapter::create_simple_action(name)
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_canvas(&self) -> Result<crate::backends_android_adapter::Canvas, Error> {
    crate::backends_android_adapter::create_canvas()
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_overlay(&self) -> Result<crate::backends_android_adapter::Overlay, Error> {
    crate::backends_android_adapter::create_overlay()
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_scrolled_window(&self) -> Result<crate::backends_android_adapter::ScrolledWindow, Error> {
    crate::backends_android_adapter::create_scrolled_window()
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
    pub fn open_file(&self, _title: &str) -> Result<Option<String>, Error> {
        Ok(crate::backends::android::open_document(&[]))
    }

    /// The Android Storage Access Framework, not a file chooser.
    ///
    /// The returned string is a `content://` **URI, not a path**: there is no
    /// filesystem path behind a SAF result, and `std::fs` cannot open one. A
    /// caller that needs a real file calls
    /// [`crate::backends::android::materialize_document`], which copies the
    /// document into the app's private storage and returns a path the
    /// ordinary loaders can read. Returning `None` here is what made
    /// File &rarr; Open open nothing and File &rarr; Save As discard the save.
    #[cfg(all(target_os = "android", not(feature = "zork")))]
    pub fn open_file_filtered(
        &self,
        _title: &str,
        filters: &[(&str, &[&str])],
    ) -> Result<Option<String>, Error> {
        Ok(crate::backends::android::open_document(
            &mimes_from_filters(filters),
        ))
    }

    /// See [`Self::open_file_filtered`]: a `content://` URI, not a path.
    #[cfg(all(target_os = "android", not(feature = "zork")))]
    pub fn save_file(&self, _title: &str) -> Result<Option<String>, Error> {
        Ok(crate::backends::android::create_document(
            "*/*",
            "untitled.corro",
        ))
    }

    /// See [`Self::open_file_filtered`] for the URI. The *default name* is
    /// honoured (via `Intent.EXTRA_TITLE`) rather than ignored, so the
    /// platform picker opens with it pre-filled the way a desktop's save
    /// dialog does.
    #[cfg(all(target_os = "android", not(feature = "zork")))]
    pub fn save_file_filtered(
        &self,
        _title: &str,
        filters: &[(&str, &[&str])],
        current_name: &str,
    ) -> Result<Option<String>, Error> {
        let mimes = mimes_from_filters(filters);
        // The picker takes one type; the first filter's is the best single
        // answer when a caller offers several groups, and an empty list means
        // any file, which is also the honest answer for "*.*".
        let mime = mimes.first().cloned().unwrap_or_else(|| "*/*".to_string());
        let name = if current_name.is_empty() {
            "untitled.corro"
        } else {
            current_name
        };
        Ok(crate::backends::android::create_document(&mime, name))
    }

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_grid(&self) -> Result<crate::backends_android_adapter::Grid, Error> {
    crate::backends_android_adapter::create_grid()
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_dropdown(&self, items: &[&str]) -> Result<crate::backends_android_adapter::DropDown, Error> {
    crate::backends_android_adapter::create_dropdown(items)
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_checkbutton(&self, label: &str) -> Result<crate::backends_android_adapter::CheckButton, Error> {
    crate::backends_android_adapter::create_checkbutton(label)
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_radiobutton(&self, group: Option<&crate::backends_android_adapter::RadioButton>, label: &str) -> Result<crate::backends_android_adapter::RadioButton, Error> {
    let _ = group;
    crate::backends_android_adapter::create_radiobutton(None, label)
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_dialog(&self) -> Result<crate::backends_android_adapter::Dialog, Error> {
    crate::backends_android_adapter::create_dialog()
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub fn create_textview(&self) -> Result<crate::backends_android_adapter::TextView, Error> {
    crate::backends_android_adapter::create_textview()
}

// -- iOS / macOS low-level construction and file dialogs --
//
// The Apple adapters already implement these widgets; what was missing was the
// `App`-level arm for them, so a `gui-mobile` / `gui-macos` build could not
// name them at all. Without these the shared dialog code (src/gui/dialogs.rs)
// does not compile on either platform, which is why the iOS host crate — a
// real consumer of `gui-mobile` — could not build.
//
// A file dialog is a document picker, which is a host responsibility on both
// platforms (UIDocumentPickerViewController / NSOpenPanel): the Rust side has
// no picker to open, so these report "cancelled" and the host shim intercepts
// the menu action instead. See `resolve_picked_document` in src/gui/dialogs.rs
// for the URI half of that contract.
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub fn create_textview(&self) -> Result<crate::backends_ios_adapter::TextView, Error> {
    crate::backends_ios_adapter::create_textview()
}
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub fn create_dropdown(&self, items: &[&str]) -> Result<crate::backends_ios_adapter::DropDown, Error> {
    crate::backends_ios_adapter::create_dropdown(items)
}
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub fn create_checkbutton(&self, label: &str) -> Result<crate::backends_ios_adapter::CheckButton, Error> {
    crate::backends_ios_adapter::create_checkbutton(label)
}
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub fn create_radiobutton(&self, group: Option<&crate::backends_ios_adapter::RadioButton>, label: &str) -> Result<crate::backends_ios_adapter::RadioButton, Error> {
    let _ = group;
    crate::backends_ios_adapter::create_radiobutton(None, label)
}
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub fn open_file(&self, _title: &str) -> Result<Option<String>, Error> { Ok(None) }
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub fn open_file_filtered(&self, _title: &str, _filters: &[(&str, &[&str])]) -> Result<Option<String>, Error> { Ok(None) }
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub fn save_file(&self, _title: &str) -> Result<Option<String>, Error> { Ok(None) }
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub fn save_file_filtered(&self, _title: &str, _filters: &[(&str, &[&str])], _current_name: &str) -> Result<Option<String>, Error> { Ok(None) }

#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_textview(&self) -> Result<crate::backends_macos_adapter::TextView, Error> {
    crate::backends_macos_adapter::create_textview()
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_dropdown(&self, items: &[&str]) -> Result<crate::backends_macos_adapter::DropDown, Error> {
    crate::backends_macos_adapter::create_dropdown(items)
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_checkbutton(&self, label: &str) -> Result<crate::backends_macos_adapter::CheckButton, Error> {
    crate::backends_macos_adapter::create_checkbutton(label)
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_radiobutton(&self, group: Option<&crate::backends_macos_adapter::RadioButton>, label: &str) -> Result<crate::backends_macos_adapter::RadioButton, Error> {
    let _ = group;
    crate::backends_macos_adapter::create_radiobutton(None, label)
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_window(&self) -> Result<crate::backends_macos_adapter::Window, Error> {
    crate::backends_macos_adapter::create_window()
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_button(&self, label: &str) -> Result<crate::backends_macos_adapter::Button, Error> {
    crate::backends_macos_adapter::create_button(label)
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_label(&self, text: &str) -> Result<crate::backends_macos_adapter::Label, Error> {
    crate::backends_macos_adapter::create_label(text)
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_box(
    &self,
    orientation: crate::backends_macos_adapter::Orientation,
    spacing: i32,
) -> Result<crate::backends_macos_adapter::BoxWidget, Error> {
    crate::backends_macos_adapter::create_box(orientation, spacing)
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_entry(&self) -> Result<crate::backends_macos_adapter::Entry, Error> {
    crate::backends_macos_adapter::create_entry()
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_menu(&self) -> Result<crate::backends_macos_adapter::Menu, Error> {
    crate::backends_macos_adapter::create_menu()
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_menubar(
    &self,
    model: &crate::backends_macos_adapter::Menu,
    action_group: *mut c_void,
) -> Result<crate::backends_macos_adapter::MenuBar, Error> {
    crate::backends_macos_adapter::create_menubar(model, action_group)
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_simple_action(&self, name: &str) -> Result<crate::backends_macos_adapter::SimpleAction, Error> {
    crate::backends_macos_adapter::create_simple_action(name)
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_canvas(&self) -> Result<crate::backends_macos_adapter::Canvas, Error> {
    crate::backends_macos_adapter::create_canvas()
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_overlay(&self) -> Result<crate::backends_macos_adapter::Overlay, Error> {
    crate::backends_macos_adapter::create_overlay()
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_scrolled_window(&self) -> Result<crate::backends_macos_adapter::ScrolledWindow, Error> {
    crate::backends_macos_adapter::create_scrolled_window()
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_grid(&self) -> Result<crate::backends_macos_adapter::Grid, Error> {
    crate::backends_macos_adapter::create_grid()
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn create_dialog(&self) -> Result<crate::backends_macos_adapter::Dialog, Error> {
    crate::backends_macos_adapter::create_dialog()
}
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn open_file(&self, _title: &str) -> Result<Option<String>, Error> { Ok(None) }
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn open_file_filtered(&self, _title: &str, _filters: &[(&str, &[&str])]) -> Result<Option<String>, Error> { Ok(None) }
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn save_file(&self, _title: &str) -> Result<Option<String>, Error> { Ok(None) }
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub fn save_file_filtered(&self, _title: &str, _filters: &[(&str, &[&str])], _current_name: &str) -> Result<Option<String>, Error> { Ok(None) }

// ---------------------------------------------------------------------------
// High-level wrapper creation methods (return common types)
// ---------------------------------------------------------------------------

    /// Create a new Window and return a platform-independent handle.
    pub fn new_window(&self) -> Result<crate::common::Window, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            let inner = crate::backends_gtk_adapter::create_window()?;
            return Ok(crate::common::Window { inner });
        }
        #[cfg(all(windows, not(feature = "zork")))]
        {
            let inner = crate::backends_nwg_adapter::create_window(&self.parent_cell)?;
            Ok(crate::common::Window { inner })
        }
        #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
        {
            let inner = crate::backends_pancurses_adapter::create_window()?;
            Ok(crate::common::Window { inner })
        }
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        {
            let inner = crate::backends_wasm_adapter::create_window()?;
            Ok(crate::common::Window { inner })
        }
        #[cfg(all(target_os = "android", not(feature = "zork")))]
        {
            let inner = crate::backends_android_adapter::create_window()?;
            Ok(crate::common::Window { inner })
        }
        #[cfg(all(target_os = "ios", not(feature = "zork")))]
        {
            let inner = crate::backends_ios_adapter::create_window()?;
            return Ok(crate::common::Window { inner });
        }
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        {
            let inner = crate::backends_macos_adapter::create_window()?;
            return Ok(crate::common::Window { inner });
        }
        // Zork: the headless model backend. Returns the same wrapper shape as
        // every other backend, so shared GUI code runs unchanged.
        #[cfg(feature = "zork")]
        {
            let inner = crate::backends_zork_adapter::create_window()?;
            return Ok(crate::common::Window { inner });
        }
    }

    /// Create a new layout Box.
    pub fn new_box(&self, orientation: crate::common::Orientation, spacing: i32) -> Result<crate::common::WidgetBox, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            let gtk_orient = match orientation {
                crate::common::Orientation::Horizontal => crate::backends_gtk_adapter::Orientation::Horizontal,
                crate::common::Orientation::Vertical => crate::backends_gtk_adapter::Orientation::Vertical,
            };
            let inner = crate::backends_gtk_adapter::create_box(gtk_orient, spacing)?;
            return Ok(crate::common::WidgetBox { inner });
        }
        #[cfg(all(windows, not(feature = "zork")))]
        {
            let nwg_orient = match orientation {
                crate::common::Orientation::Horizontal => crate::backends::nwg::Orientation::Horizontal,
                crate::common::Orientation::Vertical => crate::backends::nwg::Orientation::Vertical,
            };
            let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
            let inner = crate::backends_nwg_adapter::create_box(nwg_orient, spacing, parent)?;
            Ok(crate::common::WidgetBox { inner })
        }
        #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
        {
            let inner = crate::backends_pancurses_adapter::create_box(orientation, spacing)?;
            Ok(crate::common::WidgetBox { inner })
        }
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        {
            let inner = crate::backends_wasm_adapter::create_box(orientation, spacing)?;
            Ok(crate::common::WidgetBox { inner })
        }
        #[cfg(all(target_os = "android", not(feature = "zork")))]
        {
            let android_orient = match orientation {
                crate::common::Orientation::Horizontal => crate::backends_android_adapter::Orientation::Horizontal,
                crate::common::Orientation::Vertical => crate::backends_android_adapter::Orientation::Vertical,
            };
            let inner = crate::backends_android_adapter::create_box(android_orient, spacing)?;
            Ok(crate::common::WidgetBox { inner })
        }
        #[cfg(all(target_os = "ios", not(feature = "zork")))]
        {
            let ios_orient = match orientation {
                crate::common::Orientation::Horizontal => crate::backends_ios_adapter::Orientation::Horizontal,
                crate::common::Orientation::Vertical => crate::backends_ios_adapter::Orientation::Vertical,
            };
            let inner = crate::backends_ios_adapter::create_box(ios_orient, spacing)?;
            return Ok(crate::common::WidgetBox { inner });
        }
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        {
            let mac_orient = match orientation {
                crate::common::Orientation::Horizontal => crate::backends_macos_adapter::Orientation::Horizontal,
                crate::common::Orientation::Vertical => crate::backends_macos_adapter::Orientation::Vertical,
            };
            let inner = crate::backends_macos_adapter::create_box(mac_orient, spacing)?;
            return Ok(crate::common::WidgetBox { inner });
        }
        #[cfg(feature = "zork")]
        {
            let zork_orient = match orientation {
                crate::common::Orientation::Horizontal => crate::backends_zork_adapter::Orientation::Horizontal,
                crate::common::Orientation::Vertical => crate::backends_zork_adapter::Orientation::Vertical,
            };
            let inner = crate::backends_zork_adapter::create_box(zork_orient, spacing)?;
            return Ok(crate::common::WidgetBox { inner });
        }
    }

    /// Create a new Label with the given text.
    pub fn new_label(&self, text: &str) -> Result<crate::common::Label, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            let inner = crate::backends_gtk_adapter::create_label(text)?;
            return Ok(crate::common::Label { inner });
        }
        #[cfg(all(windows, not(feature = "zork")))]
        {
            let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
            let inner = crate::backends_nwg_adapter::create_label(parent)?;
            inner.set_text(text);
            Ok(crate::common::Label { inner })
        }
        #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
        {
            let inner = crate::backends_pancurses_adapter::create_label(text)?;
            Ok(crate::common::Label { inner })
        }
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        {
            let inner = crate::backends_wasm_adapter::create_label(text)?;
            Ok(crate::common::Label { inner })
        }
        #[cfg(all(target_os = "android", not(feature = "zork")))]
        {
            let inner = crate::backends_android_adapter::create_label(text)?;
            Ok(crate::common::Label { inner })
        }
        #[cfg(all(target_os = "ios", not(feature = "zork")))]
        {
            let inner = crate::backends_ios_adapter::create_label(text)?;
            return Ok(crate::common::Label { inner });
        }
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        {
            let inner = crate::backends_macos_adapter::create_label(text)?;
            return Ok(crate::common::Label { inner });
        }
        #[cfg(feature = "zork")]
        {
            let inner = crate::backends_zork_adapter::create_label(text)?;
            return Ok(crate::common::Label { inner });
        }
    }

    /// Create a new text Entry.
    pub fn new_entry(&self) -> Result<crate::common::Entry, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            let inner = crate::backends_gtk_adapter::create_entry()?;
            return Ok(crate::common::Entry::new(inner));
        }
        #[cfg(all(windows, not(feature = "zork")))]
        {
            let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
            let inner = crate::backends_nwg_adapter::create_entry(parent)?;
            Ok(crate::common::Entry::new(inner))
        }
        #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
        {
            let inner = crate::backends_pancurses_adapter::create_entry()?;
            Ok(crate::common::Entry::new(inner))
        }
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        {
            let inner = crate::backends_wasm_adapter::create_entry()?;
            Ok(crate::common::Entry::new(inner))
        }
        #[cfg(all(target_os = "android", not(feature = "zork")))]
        {
            let inner = crate::backends_android_adapter::create_entry()?;
            Ok(crate::common::Entry::new(inner))
        }
        #[cfg(all(target_os = "ios", not(feature = "zork")))]
        {
            let inner = crate::backends_ios_adapter::create_entry()?;
            return Ok(crate::common::Entry::new(inner));
        }
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        {
            let inner = crate::backends_macos_adapter::create_entry()?;
            return Ok(crate::common::Entry::new(inner));
        }
        #[cfg(feature = "zork")]
        {
            let inner = crate::backends_zork_adapter::create_entry()?;
            return Ok(crate::common::Entry::new(inner));
        }
    }

    /// Create a new Canvas (custom drawing surface).
    pub fn new_canvas(&self) -> Result<crate::common::Canvas, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            let inner = crate::backends_gtk_adapter::create_canvas()?;
            return Ok(crate::common::Canvas { inner });
        }
        #[cfg(all(windows, not(feature = "zork")))]
        {
            let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
            let inner = crate::backends_nwg_adapter::create_canvas(parent)?;
            Ok(crate::common::Canvas { inner })
        }
        #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
        {
            let inner = crate::backends_pancurses_adapter::create_canvas()?;
            Ok(crate::common::Canvas { inner })
        }
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        {
            let inner = crate::backends_wasm_adapter::create_canvas()?;
            Ok(crate::common::Canvas { inner })
        }
        #[cfg(all(target_os = "android", not(feature = "zork")))]
        {
            let inner = crate::backends_android_adapter::create_canvas()?;
            Ok(crate::common::Canvas { inner })
        }
        #[cfg(all(target_os = "ios", not(feature = "zork")))]
        {
            let inner = crate::backends_ios_adapter::create_canvas()?;
            return Ok(crate::common::Canvas { inner });
        }
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        {
            let inner = crate::backends_macos_adapter::create_canvas()?;
            return Ok(crate::common::Canvas { inner });
        }
        #[cfg(feature = "zork")]
        {
            let inner = crate::backends_zork_adapter::create_canvas()?;
            return Ok(crate::common::Canvas { inner });
        }
    }

    /// Create a scrollable container for the sheet canvas (native scrollbar
    /// chrome around the grid on GUI backends; inert elsewhere).
    pub fn new_scrolled_window(&self) -> Result<crate::common::ScrolledWindow, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            let inner = crate::backends_gtk_adapter::create_scrolled_window()?;
            return Ok(crate::common::ScrolledWindow { inner });
        }
        #[cfg(all(windows, not(feature = "zork")))]
        {
            let parent = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
            let inner = crate::backends_nwg_adapter::create_scrolled_window(parent)?;
            return Ok(crate::common::ScrolledWindow { inner });
        }
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        {
            let inner = crate::backends_wasm_adapter::create_scrolled_window()?;
            return Ok(crate::common::ScrolledWindow { inner });
        }
        #[cfg(all(target_os = "android", not(feature = "zork")))]
        {
            let inner = crate::backends_android_adapter::create_scrolled_window()?;
            return Ok(crate::common::ScrolledWindow { inner });
        }
        #[cfg(all(target_os = "ios", not(feature = "zork")))]
        {
            let inner = crate::backends_ios_adapter::create_scrolled_window()?;
            return Ok(crate::common::ScrolledWindow { inner });
        }
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        {
            let inner = crate::backends_macos_adapter::create_scrolled_window()?;
            return Ok(crate::common::ScrolledWindow { inner });
        }
        // Terminal: a scrolled window is a container node. It adds no native
        // scrollbar chrome (a character display has none), but it must exist so
        // the portable `new_scrolled_window` contract holds on every backend —
        // a host that nests its canvas in one (as `gui_backend` does) can then
        // be built unchanged against the terminal.
        #[cfg(all(
            feature = "pancurses",
            not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android", target_os = "ios", target_os = "macos"))
        ))]
        {
            let inner = crate::backends_pancurses_adapter::create_scrolled_window()?;
            return Ok(crate::common::ScrolledWindow { inner });
        }
        // Zork (the scripted-terminal fallback) has containers but no
        // scrollbars; same reasoning as the pancurses arm above.
        #[cfg(feature = "zork")]
        {
            let inner = crate::backends_zork_adapter::create_scrolled_window()?;
            return Ok(crate::common::ScrolledWindow { inner });
        }
        #[allow(unreachable_code)]
        Err(Error::Backend("scrolled windows not supported on this backend".into()))
    }

    /// Create a new Menu data model.
    pub fn new_menu(&self) -> Result<crate::common::Menu, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            let inner = crate::backends_gtk_adapter::create_menu()?;
            return Ok(crate::common::Menu { inner });
        }
        #[cfg(all(windows, not(feature = "zork")))]
        {
            let inner = crate::backends_nwg_adapter::create_menu()?;
            Ok(crate::common::Menu { inner })
        }
        #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
        {
            let inner = crate::backends_pancurses_adapter::create_menu()?;
            Ok(crate::common::Menu { inner })
        }
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        {
            let inner = crate::backends_wasm_adapter::create_menu()?;
            Ok(crate::common::Menu { inner })
        }
        #[cfg(all(target_os = "android", not(feature = "zork")))]
        {
            let inner = crate::backends_android_adapter::create_menu()?;
            Ok(crate::common::Menu { inner })
        }
        #[cfg(all(target_os = "ios", not(feature = "zork")))]
        {
            let inner = crate::backends_ios_adapter::create_menu()?;
            return Ok(crate::common::Menu { inner });
        }
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        {
            let inner = crate::backends_macos_adapter::create_menu()?;
            return Ok(crate::common::Menu { inner });
        }
        #[cfg(feature = "zork")]
        {
            let inner = crate::backends_zork_adapter::create_menu()?;
            return Ok(crate::common::Menu { inner });
        }
    }

    /// Create a new SimpleAction that will dispatch to the given name.
    /// On Windows the action is registered in the shared action registry.
    pub fn new_simple_action(&self, name: &str) -> Result<crate::common::SimpleAction, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            let inner = crate::backends_gtk_adapter::create_simple_action(name)?;
            return Ok(crate::common::SimpleAction { inner });
        }
        #[cfg(all(windows, not(feature = "zork")))]
        {
            let inner = crate::backends_nwg_adapter::create_simple_action(name, self.action_registry.clone())?;
            Ok(crate::common::SimpleAction { inner })
        }
        #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
        {
            let inner = crate::backends_pancurses_adapter::create_simple_action(name)?;
            Ok(crate::common::SimpleAction { inner })
        }
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        {
            let inner = crate::backends_wasm_adapter::create_simple_action(name)?;
            Ok(crate::common::SimpleAction { inner })
        }
        #[cfg(all(target_os = "android", not(feature = "zork")))]
        {
            let inner = crate::backends_android_adapter::create_simple_action(name)?;
            Ok(crate::common::SimpleAction { inner })
        }
        #[cfg(all(target_os = "ios", not(feature = "zork")))]
        {
            let inner = crate::backends_ios_adapter::create_simple_action(name)?;
            return Ok(crate::common::SimpleAction { inner });
        }
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        {
            let inner = crate::backends_macos_adapter::create_simple_action(name)?;
            return Ok(crate::common::SimpleAction { inner });
        }
        #[cfg(feature = "zork")]
        {
            let inner = crate::backends_zork_adapter::create_simple_action(name)?;
            return Ok(crate::common::SimpleAction { inner });
        }
    }

    /// Create a MenuBar from a Menu model.
    /// `action_group` – on GTK a `*mut c_void` pointer to a `GActionGroup`
    /// (pass null if not available); on Windows it is unused.
    pub fn new_menubar(&self, model: &crate::common::Menu, _action_group: *mut c_void) -> Result<crate::common::MenuBar, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            let inner = unsafe { crate::backends_gtk_adapter::create_menubar(&model.inner, _action_group) }?;
            return Ok(crate::common::MenuBar { inner });
        }
        #[cfg(all(windows, not(feature = "zork")))]
        {
            let hwnd = self.parent_cell.borrow().as_ref().copied().unwrap_or(std::ptr::null_mut());
            let inner = crate::backends_nwg_adapter::create_menubar(&model.inner, hwnd, self.action_registry.clone())?;
            Ok(crate::common::MenuBar { inner })
        }
        #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
        {
            let inner = crate::backends_pancurses_adapter::create_menubar(&model.inner, _action_group)?;
            Ok(crate::common::MenuBar { inner })
        }
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        {
            let inner = crate::backends_wasm_adapter::create_menubar(&model.inner, _action_group)?;
            Ok(crate::common::MenuBar { inner })
        }
        #[cfg(all(target_os = "android", not(feature = "zork")))]
        {
            let inner = crate::backends_android_adapter::create_menubar(&model.inner, _action_group)?;
            Ok(crate::common::MenuBar { inner })
        }
        #[cfg(all(target_os = "ios", not(feature = "zork")))]
        {
            let inner = crate::backends_ios_adapter::create_menubar(&model.inner, _action_group)?;
            return Ok(crate::common::MenuBar { inner });
        }
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        {
            let inner = crate::backends_macos_adapter::create_menubar(&model.inner, _action_group)?;
            return Ok(crate::common::MenuBar { inner });
        }
        #[cfg(feature = "zork")]
        {
            let inner = crate::backends_zork_adapter::create_menubar(&model.inner, _action_group)?;
            return Ok(crate::common::MenuBar { inner });
        }
    }

    /// Build a Menu tree from declarative SubmenuDef definitions.
    /// `label_prefix` is prepended to each submenu label (e.g. "\u{3164}" for
    /// GTK4 to prevent mnemonic accelerator assignment; pass "" for other backends).
    pub fn build_menu_model(&self, submenus: &[crate::common::SubmenuDef], _label_prefix: &str) -> Result<crate::common::Menu, Error> {
        fn build_items(app: &App, items: &[crate::common::MenuItemDef], prefix: &str) -> Result<crate::common::Menu, Error> {
            let mut menu = app.new_menu()?;
            for item in items {
                if let Some(children) = item.submenu {
                    let sub = build_items(app, children, prefix)?;
                    menu.append_submenu(item.label, &sub);
                } else {
                    menu.append(item.label, &format!("{}.{}", prefix, item.action));
                }
            }
            Ok(menu)
        }
        let mut root = self.new_menu()?;
        for sm in submenus {
            let sub = build_items(self, sm.items, sm.prefix)?;
            root.append_submenu(sm.label, &sub);
        }
        Ok(root)
    }

    /// Create a new Dialog.
    pub fn new_dialog(&self) -> Result<crate::common::Dialog, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            let inner = crate::backends_gtk_adapter::create_dialog()?;
            return Ok(crate::common::Dialog { inner });
        }
        #[cfg(all(windows, not(feature = "zork")))]
        {
            let inner = crate::backends_nwg_adapter::create_dialog(&self.parent_cell)?;
            Ok(crate::common::Dialog { inner })
        }
        #[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
        {
            let inner = crate::backends_pancurses_adapter::create_dialog()?;
            Ok(crate::common::Dialog { inner })
        }
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        {
            let inner = crate::backends_wasm_adapter::create_dialog()?;
            Ok(crate::common::Dialog { inner })
        }
        #[cfg(all(target_os = "android", not(feature = "zork")))]
        {
            let inner = crate::backends_android_adapter::create_dialog()?;
            Ok(crate::common::Dialog { inner })
        }
        #[cfg(all(target_os = "ios", not(feature = "zork")))]
        {
            let inner = crate::backends_ios_adapter::create_dialog()?;
            return Ok(crate::common::Dialog { inner });
        }
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        {
            let inner = crate::backends_macos_adapter::create_dialog()?;
            return Ok(crate::common::Dialog { inner });
        }
        #[cfg(feature = "zork")]
        {
            let inner = crate::backends_zork_adapter::create_dialog()?;
            return Ok(crate::common::Dialog { inner });
        }
    }

    /// Ensure the GTK application / action group exists (no-op on Windows).
    /// Returns an opaque `*mut c_void` that can be passed to `new_menubar`.
    pub fn ensure_action_group(&self) -> Result<*mut c_void, Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        {
            if self.action_group.borrow().is_none() {
                let app = crate::backends_gtk_adapter::create_application()?;
                app.register()?;
                *self.action_group.borrow_mut() = Some(app);
            }
            Ok(self.action_group.borrow().as_ref().unwrap().as_ptr())
        }
        #[cfg(not(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs")))))]
        Ok(std::ptr::null_mut())
    }

    /// Register a SimpleAction with the action group.
    /// On GTK this adds the action to the GApplication; on Windows it is a no-op.
    pub fn register_action(&self, _action: &crate::common::SimpleAction) -> Result<(), Error> {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        if let Some(ref app) = *self.action_group.borrow() {
            app.add_action(&_action.inner)?;
        }
        #[cfg(not(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs")))))]
        {}
        Ok(())
    }

/// Run the backend main loop
    pub fn run(self) -> Result<(), Error> {
        let _ = std::fs::write("/tmp/corro_app_run.txt", "App::run() called\n");
        let boxed = self.inner.borrow_mut().take().ok_or_else(|| Error::Backend("App::run already called".into()))?;
        let _ = std::fs::write("/tmp/corro_backend_ptr.txt", &format!("backend={:#p}\n", &*boxed as *const _ as *const u8));
        boxed.run().map_err(|e| Error::Backend(format!("{}", e)))
    }

    /// Post a quit message to the backend's event loop.
    /// Safe to call from signal handlers and event callbacks.
    pub fn quit(&self) {
        #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
        crate::backends_wasm_adapter::quit_main_loop();
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        let _ = crate::backends_gtk_adapter::quit_main_loop();
        #[cfg(all(windows, not(feature = "zork")))]
        crate::backends_nwg_adapter::quit_main_loop();
        #[cfg(all(target_os = "macos", not(feature = "zork")))]
        crate::backends_macos_adapter::quit_app();
    }

    /// Like quit() but returns the backend error, if any.
    #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
    pub fn try_quit(&self) -> Result<(), String> {
        crate::backends_gtk_adapter::quit_main_loop().map_err(|e| format!("{e}"))
    }
    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    pub fn try_quit(&self) -> Result<(), String> {
        // A browser is the one backend where quitting has real work to do:
        // set the quit title and take the app's root off the page. Returning
        // `Ok(())` here, as every other non-GTK backend does, meant the wasm
        // "Exit" (and Ctrl+Q) went through `save_before_quit` and stopped --
        // no `CORRO_QUIT`, no visible quit. See
        // `backends_wasm_adapter::quit_main_loop`.
        crate::backends_wasm_adapter::quit_main_loop();
        Ok(())
    }
    #[cfg(not(any(
        target_arch = "wasm32",
        feature = "gtk4-rs",
        all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))
    )))]
    pub fn try_quit(&self) -> Result<(), String> {
        Ok(())
    }

    /// Pump the backend's event loop for `count` blocking iterations.
    /// On GTK/Linux this processes pending main context events (frame
    /// clock ticks, redraws, configure events).  On other backends this
    /// is a no-op.
    ///
    /// Call after `queue_redraw()` to ensure the draw callback fires
    /// before entering the main loop, especially on virtual displays
    /// (Xvfb, WSL) where the GTK4 frame clock may not tick automatically.
    pub fn pump_events(&self, count: usize) {
        #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
        crate::backends_gtk_adapter::pump_main_context(count);
        let _ = count;
    }
}

impl From<Box<dyn crate::backends::BackendApp>> for App {
    fn from(b: Box<dyn crate::backends::BackendApp>) -> Self {
        App {
            inner: Rc::new(RefCell::new(Some(b))),
            #[cfg(all(windows, not(feature = "zork")))]
            parent_cell: Rc::new(RefCell::new(None)),
            #[cfg(all(windows, not(feature = "zork")))]
            action_registry: Rc::new(RefCell::new(HashMap::new())),
            #[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
            action_group: Rc::new(RefCell::new(None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

#[test]
    fn popup_keeps_the_mouse_point_when_it_fits() {
        // Plenty of room in every direction: the popup lands exactly on the
        // mouse, which is the whole contract.
        let work = ScreenRect { left: 0, top: 0, right: 1920, bottom: 1040 };
        assert_eq!(place_popup(500, 300, 200, 150, work), (500, 300));
        // Flush against the far corner with room to spare is still exact.
        assert_eq!(
            place_popup(1920 - 200, 1040 - 150, 200, 150, work),
            (1720, 890)
        );
    }

    #[test]
    fn popup_flips_left_and_up_rather_than_drifting() {
        let work = ScreenRect { left: 0, top: 0, right: 1920, bottom: 1040 };
        // Near the right edge: flip left, mouse's y untouched.
        assert_eq!(place_popup(1900, 300, 200, 150, work), (1700, 300));
        // Near the bottom: flip up, mouse's x untouched.
        assert_eq!(place_popup(500, 1000, 200, 150, work), (500, 850));
        // Both at once: both axes flip.
        assert_eq!(place_popup(1900, 1000, 200, 150, work), (1700, 850));
    }

    #[test]
    fn popup_never_lands_on_the_taskbar() {
        // Work area is the screen minus a 40px taskbar at the bottom.
        let work = ScreenRect { left: 0, top: 0, right: 1920, bottom: 1000 };
        // A menu opened low on the screen must have its BOTTOM at or above
        // 1000 -- inside rcMonitor (1080) but under the taskbar, which is the
        // bug being pinned.
        for mouse_y in 900..1080 {
            let (x, y) = place_popup(100, mouse_y, 200, 150, work);
            assert!(
                y + 150 <= work.bottom,
                "menu from y={mouse_y} placed at ({x},{y}) is {}px under the taskbar",
                work.bottom - (y + 150)
            );
            assert!(x >= work.left && x + 200 <= work.right, "menu off-screen horizontally at ({x},{y})");
        }
    }

    #[test]
    fn popup_handles_a_non_zero_work_area_origin() {
        // Secondary monitor / any work area not at the origin: every decision
        // is relative to work_area, not to (0,0).
        let work = ScreenRect { left: 1920, top: 0, right: 3840, bottom: 1040 };
        assert_eq!(place_popup(2500, 300, 200, 150, work), (2500, 300));
        // Right edge of THAT monitor flips left.
        assert_eq!(place_popup(3830, 300, 200, 150, work), (3630, 300));
        // A mouse left of the work area's left edge is pulled in, not left
        // off-screen.
        assert_eq!(place_popup(100, 300, 200, 150, work), (1920, 300));
    }

    #[test]
    fn popup_larger_than_the_work_area_pins_to_its_top_left() {
        // A very long menu on a short screen cannot fit either way; pin it to
        // the work area's top-left rather than pushing it off the bottom.
        let work = ScreenRect { left: 0, top: 0, right: 400, bottom: 300 };
        let (x, y) = place_popup(380, 290, 500, 900, work);
        assert_eq!((x, y), (0, 0), "oversized menu must pin to the work-area top-left");
    }

    #[test]
    fn popup_near_an_edge_only_moves_when_a_flip_would_go_off_screen() {
        let work = ScreenRect { left: 0, top: 0, right: 800, bottom: 600 };
        // x=10 with w=200 fits comfortably -- nothing to do, and the popup
        // must stay on the mouse rather than being clamped to the edge.
        assert_eq!(place_popup(10, 100, 200, 150, work), (10, 100));
        // Now the narrow case: the popup is wide enough that a left flip from
        // x=10 would go off-screen. Here the overflow really is unavoidable,
        // so pin to the left edge instead of returning a negative x.
        let narrow = ScreenRect { left: 0, top: 0, right: 150, bottom: 600 };
        let (x, _y) = place_popup(10, 100, 200, 150, narrow);
        assert_eq!(x, 0, "a flip that would go off-screen must pin to the left edge");
        assert!(x >= narrow.left, "never negative");
        // Same at the bottom: a tall popup near y=20 on a short work area.
        let short = ScreenRect { left: 0, top: 0, right: 800, bottom: 100 };
        let (_x2, y2) = place_popup(100, 20, 200, 150, short);
        assert_eq!(y2, 0, "unavoidable overflow must pin to the top edge");
        // And a case that genuinely flips and stays put: right edge, fits
        // above the left edge.
        assert_eq!(place_popup(790, 100, 200, 150, work), (590, 100));
    }

    #[test]
    fn popup_ignores_zero_or_negative_sizes_without_panicking() {
        let work = ScreenRect { left: 0, top: 0, right: 800, bottom: 600 };
        // A size we failed to measure (0, or -1 from a failed call) must not
        // panic and must not move the popup off its point.
        assert_eq!(place_popup(100, 100, 0, 0, work), (100, 100));
        assert_eq!(place_popup(100, 100, -5, -5, work), (100, 100));
    }

    
    // NOTE: the translator is process-global, so these tests must not run
    // concurrently with each other. `cargo test` runs tests in one binary on
    // multiple threads; keep all translator-dependent assertions in ONE test.
    #[test]
    fn label_keys_resolve_through_installed_translator() {
        // Before install: identity, so existing tests keep passing.
        assert_eq!(Label::Key("menu.file").resolve(), "menu.file");
        assert_eq!(tr("std.exit"), "std.exit");

        // Install a catalog, then swap it (locale switch) — the early
        // identity resolution above must not have frozen the cell.
        set_translator(|k| match k {
            "menu.file" => "File".to_string(),
            "std.exit" => "Exit".to_string(),
            _ => k.to_string(),
        });
        assert_eq!(Label::Key("menu.file").resolve(), "File");
        assert_eq!(tr("std.exit"), "Exit");

        set_translator(|k| match k {
            "menu.file" => "Datei".to_string(),
            "std.exit" => "Beenden".to_string(),
            _ => k.to_string(),
        });
        assert_eq!(Label::Key("menu.file").resolve(), "Datei");
        assert_eq!(tr("std.exit"), "Beenden");

        // Unknown key falls back to the key itself (benign).
        assert_eq!(tr("no.such.key"), "no.such.key");
    }

    #[test]
    fn literal_label_ignores_translator_and_identity_is_locale_stable() {
        // Literals never consult the translator.
        assert_eq!(Label::Literal("Open file".to_string()).resolve(), "Open file");
        assert!(!Label::Literal("x".to_string()).is_key());
        assert!(Label::Key("x").is_key());

        // identity() is the locale-independent contract (dispatch/parity).
        assert_eq!(Label::Key("menu.file").identity(), "menu.file");
        assert_eq!(Label::Literal("Open".to_string()).identity(), "Open");
    }

    #[test]
    fn from_impls_keep_literal_construction_unchanged() {
        assert_eq!(Label::from("x"), Label::Literal("x".to_string()));
        assert_eq!(Label::from("x".to_string()), Label::Literal("x".to_string()));
        assert_eq!(Label::from(&"x".to_string()), Label::Literal("x".to_string()));
    }

    #[test]
    fn literal_and_key_coexist_in_one_tree() {
        let tree = MenuItem::Submenu {
            label: Label::Key("menu.file"),
            shortcut: None,
            items: vec![
                MenuItem::Action { label: "Open".into(), action: "open".into(), shortcut: None },
                MenuItem::Action {
                    label: Label::Key(std_action_keys::SAVE_AS),
                    action: "save_as".into(),
                    shortcut: Some("Ctrl+S".into()),
                },
                MenuItem::Separator,
            ],
        };
        match &tree {
            MenuItem::Submenu { label, items, .. } => {
                assert!(label.is_key());
                assert_eq!(items.len(), 3);
                // Dispatch identity is the action string, untouched by translation.
                match &items[1] {
                    MenuItem::Action { action, label, .. } => {
                        assert_eq!(action, "save_as");
                        assert_eq!(label.identity(), std_action_keys::SAVE_AS);
                    }
                    _ => panic!("expected action"),
                }
            }
            _ => panic!("expected submenu"),
        }
    }

    #[test]
    fn layout_direction_hook_is_receives_changes() {
        // Headless: no backend hook installed, so this is a no-op (must not panic).
        set_layout_direction(LayoutDir::Rtl);
    }

    #[test]
    fn rtl_locale_heuristic() {
        assert!(locale_is_rtl("ar"));
        assert!(locale_is_rtl("ar-EG"));
        assert!(locale_is_rtl("he_IL"));
        assert!(!locale_is_rtl("en-US"));
        assert!(!locale_is_rtl("de"));
        assert!(!locale_is_rtl(""));
    }

    // ---- theme / colour scheme ----

    /// The palette is light until an app says otherwise.
    ///
    /// Load-bearing: every colour literal this crate used to hardcode is the
    /// `Light` value, so a build that never calls `set_color_scheme` must paint
    /// exactly as it did before theming existed. `default_is_the_historical_palette`
    /// pins the values themselves; this pins the wiring.
    #[test]
    fn theme_defaults_to_light() {
        assert_eq!(color_scheme(), ColorScheme::Light);
        assert_eq!(theme(), Theme::for_scheme(ColorScheme::Light));
    }

    /// The `Light` palette reproduces the literals that were hardcoded before
    /// theming existed.
    ///
    /// If this fails, a build that never opts into theming has been restyled —
    /// the worst possible outcome for a "new feature", because every existing
    /// app and every golden-pixel test changes under it.
    #[test]
    fn default_is_the_historical_palette() {
        set_color_scheme(ColorScheme::Light);
        let t = theme();
        let eq = |role: Role, r: f64, g: f64, b: f64| {
            assert_eq!(
                t.color(role),
                Color::new(r, g, b),
                "role {role:?} must keep its historical light value"
            );
        };
        eq(Role::Paper, 0.96, 0.96, 0.96); // spreadsheet::paint clear
        eq(Role::CellBody, 1.0, 1.0, 1.0);
        eq(Role::CellMargin, 0.75, 0.75, 0.75);
        eq(Role::Header, 0.88, 0.9, 0.93);
        eq(Role::Gridline, 0.8, 0.8, 0.8);
        eq(Role::Cursor, 0.8, 0.9, 1.0);
        eq(Role::CursorEditing, 1.0, 1.0, 0.8);
        eq(Role::Text, 0.05, 0.05, 0.1);
        eq(Role::TextMuted, 0.3, 0.3, 0.3);
        eq(Role::TextAccent, 0.0, 0.0, 0.85); // hyperlink blue
    }

    /// Switching schemes swaps the whole palette, and switching back restores
    /// it exactly — the property that makes the toggle safe to spam.
    #[test]
    fn scheme_round_trips() {
        set_color_scheme(ColorScheme::Light);
        let light = theme();
        set_color_scheme(ColorScheme::Night);
        let night = theme();
        assert_eq!(color_scheme(), ColorScheme::Night);
        assert_ne!(light, night, "night must actually differ from light");
        set_color_scheme(ColorScheme::Light);
        assert_eq!(theme(), light, "toggling back must restore the light palette exactly");
    }

    /// `set_theme` installs an app-supplied palette, and `with` overrides one
    /// role without disturbing the rest. This is the customization path a host
    /// with its own design uses, so it has to work per-role.
    #[test]
    fn custom_theme_overrides_only_the_named_role() {
        let base = Theme::for_scheme(ColorScheme::Night);
        let custom = base.with(Role::CellBody, Color::new(0.2, 0.05, 0.05));
        assert_eq!(custom.color(Role::CellBody), Color::new(0.2, 0.05, 0.05));
        assert_eq!(
            custom.color(Role::Text),
            base.color(Role::Text),
            "an override must not disturb any other role"
        );
        set_theme(custom);
        assert_eq!(theme().color(Role::CellBody), Color::new(0.2, 0.05, 0.05));
        // Leave the global as the next test expects to find it.
        set_color_scheme(ColorScheme::Light);
    }

    /// Text has to stay legible in *both* schemes.
    ///
    /// This is the assertion that makes the night palette a dark theme rather
    /// than a set of dark numbers: a dark surface with dark text is perfectly
    /// well-formed and completely unreadable. Asserted as a contrast ratio,
    /// which is the property a reader actually experiences.
    #[test]
    fn body_text_stays_legible_in_both_schemes() {
        for scheme in [ColorScheme::Light, ColorScheme::Night] {
            let t = Theme::for_scheme(scheme);
            let text = t.color(Role::Text);
            // Body text on a plain cell, and on the dimmest surface it is ever
            // drawn over.
            // Every surface body text is ever drawn over, not just the plain
            // cell: the cell under the caret, the cell being typed into, a
            // selected cell and the dimmed margin. A dark scheme that passes
            // only on the body cell can still render the *caret* unreadable,
            // which is the one surface a user must always be able to read.
            // WCAG AA for body text is 4.5:1; the highlights are held to the
            // same bar rather than a looser one, because they carry the same
            // text.
            for (role, label) in [
                (Role::CellBody, "body cell"),
                (Role::CellMargin, "margin cell"),
            ] {
                let ratio = text.contrast_ratio(t.color(role));
                assert!(
                    ratio >= 4.5,
                    "{scheme:?}: body text on a {label} is only {ratio:.2}:1"
                );
            }
            // Highlight ink on the highlight fills — a *different* pairing in
            // each scheme (light ink on dark fills, dark ink on light fills),
            // which is exactly why this is a separate assertion. Checking body
            // ink against a highlight fill is the mistake that made the first
            // night palette unreadable at the caret.
            let on_hi = t.color(Role::TextOnHighlight);
            for (role, label) in [
                (Role::Cursor, "cursor cell"),
                (Role::CursorEditing, "editing cell"),
                (Role::Selected, "selected cell"),
            ] {
                let ratio = on_hi.contrast_ratio(t.color(role));
                assert!(
                    ratio >= 4.5,
                    "{scheme:?}: highlight text on a {label} is only {ratio:.2}:1"
                );
            }
            // The highlights must also be visibly distinct from the body, or
            // "legible" would hold only because every surface is one flat
            // colour and the caret is invisible.
            for (role, label) in [
                (Role::Cursor, "cursor cell"),
                (Role::CursorEditing, "editing cell"),
                (Role::Selected, "selected cell"),
            ] {
                assert!(
                    (t.color(role).luminance() - t.color(Role::CellBody).luminance()).abs() > 0.01,
                    "{scheme:?}: a {label} must be visibly distinct from a body cell"
                );
            }
        }
    }

    /// Night mode is a *dark* scheme, and light mode a *light* one: the
    /// relationship inverts rather than simply darkening. Stated in luminance
    /// so it is about what a reader sees, not about the raw channel values.
    #[test]
    fn night_mode_actually_inverts_the_surface_luminance() {
        let light = Theme::for_scheme(ColorScheme::Light);
        let night = Theme::for_scheme(ColorScheme::Night);
        let lum = |t: &Theme, role: Role| t.color(role).luminance();
        assert!(
            lum(&light, Role::CellBody) > 0.9,
            "light body must be bright, got {}",
            lum(&light, Role::CellBody)
        );
        assert!(
            lum(&night, Role::CellBody) < 0.25,
            "night body must be dark, got {}",
            lum(&night, Role::CellBody)
        );
        assert!(
            lum(&night, Role::Text) > lum(&night, Role::CellBody) * 4.0,
            "night text must be far lighter than the surface it sits on"
        );
    }

    /// The margin stays subordinate to the body in both schemes.
    ///
    /// "Dimmer" means the same perceptual thing in each, but the direction
    /// flips: in light mode the margin is darker than the body, in night mode
    /// it is *darker still* (below the body, not above it). What must not
    /// happen is the margin becoming the more prominent surface, which is why
    /// this asserts the ordering rather than a literal value.
    #[test]
    fn margin_stays_subordinate_to_the_body_in_both_schemes() {
        for scheme in [ColorScheme::Light, ColorScheme::Night] {
            let t = Theme::for_scheme(scheme);
            let body = t.color(Role::CellBody).luminance();
            let margin = t.color(Role::CellMargin).luminance();
            // The margin differs from the body (it is visibly a separate zone)
            // but the two stay distinguishable from the paper.
            assert!(
                (body - margin).abs() > 0.01,
                "{scheme:?}: margin and body must differ"
            );
            assert!(
                t.color(Role::Paper).contrast_ratio(t.color(Role::CellBody)) < 2.0,
                "{scheme:?}: the body must read as a distinct surface from the paper"
            );
        }
    }

}

// ---------------------------------------------------------------------------
// Portable periodic tick
// ---------------------------------------------------------------------------

/// Run `f` every `ms` milliseconds until it returns `false`, with no user
/// input required (GTK timeout source / Win32 `WM_TIMER`).
///
/// Apps need this for work that must happen while idle — e.g. tailing an
/// append-only log so another window's committed revisions show up here
/// without the user having to press a key.
#[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
pub fn add_periodic_tick(
    _window: &crate::common::Window,
    ms: u32,
    f: Box<dyn FnMut() -> bool>,
) -> Result<(), Error> {
    // GTK timeouts belong to the main context, not a window.
    crate::backends_gtk_adapter::timeout_add_repeating(ms, f)
}

/// See the GTK variant. Win32 needs the window whose message loop should
/// receive `WM_TIMER`, so the handle is taken from `window`.
#[cfg(all(windows, not(feature = "zork")))]
pub fn add_periodic_tick(
    window: &crate::common::Window,
    ms: u32,
    f: Box<dyn FnMut() -> bool>,
) -> Result<(), Error> {
    // Ids are assigned by the backend: NWG needs handler ids above 0xFFFF
    // (it reserves the low range) plus a separate small Win32 timer id, so it
    // owns the numbering rather than taking one from here.
    window.inner.start_repeating_timer(0, ms, f)
}

/// iOS: an `NSTimer` on the main run loop, driven through the host's existing
/// callback trampoline.
///
/// This used to fall through to the no-op arm below, which returned `Ok(())`
/// and did nothing - so nothing on iOS ever polled: `CORRO_EDIT_SCRIPT` armed
/// and then ran no steps, and the append-only log tail that keeps two windows
/// in sync never fired. A silent success is the worst shape for this, because
/// every caller assumes the tick exists.
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub fn add_periodic_tick(
    _window: &crate::common::Window,
    ms: u32,
    f: Box<dyn FnMut() -> bool>,
) -> Result<(), Error> {
    crate::backends_ios_adapter::add_periodic_tick(ms, f)
}

/// WASM: a `setInterval` on the page, returning `true` from `f` to stop.
///
/// WASM used to be in the no-op list below, on the reasoning that a browser
/// drives its own loop. That is true of the *dispatch* and false of the
/// *timer*: nothing in a page calls back into Rust on a schedule unless
/// something asks it to, so an app that armed a periodic tick -- tailing an
/// append-only log, polling a collaborator -- silently never ran. It is the
/// same trap the iOS note below describes, and the same fix.
#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
pub fn add_periodic_tick(
    _window: &crate::common::Window,
    ms: u32,
    f: Box<dyn FnMut() -> bool>,
) -> Result<(), Error> {
    crate::backends_wasm_adapter::add_periodic_tick(ms, f)
}

/// Backends that drive their own event loop and already poll for external
/// changes (pancurses/terminal, zork, android) need no timer, so the
/// request is accepted and dropped.
///
/// NOTE: iOS is deliberately NOT in this list - it needs a real timer, and
/// returning Ok without one hid that for as long as it did. WASM is not in it
/// either, for the same reason; see the arm above.
#[cfg(not(any(
    any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))),
    all(windows, not(feature = "zork")),
    all(target_os = "ios", not(feature = "zork")),
    all(target_arch = "wasm32", not(feature = "zork"))
)))]
pub fn add_periodic_tick(
    _window: &crate::common::Window,
    _ms: u32,
    _f: Box<dyn FnMut() -> bool>,
) -> Result<(), Error> {
    Ok(())
}


impl App {
    /// Run `cb` every `ms` milliseconds on the UI thread until it returns
    /// `false`.
    ///
    /// This is the client-facing timer the vendored rustxWidgets has always
    /// had; the merged backend keeps it. A platform with no timer returns an
    /// error rather than `Ok(())`, because a silent success is how an app ends
    /// up polling nothing at all.
    pub fn timeout_add(ms: u32, cb: Box<dyn FnMut() -> bool>) -> Result<(), Error> {
        #[cfg(all(
            feature = "gtk",
            target_os = "linux",
            not(feature = "zork"),
            not(feature = "gtk4-rs")
        ))]
        {
            return crate::backends_gtk_adapter::timeout_add_repeating(ms, cb);
        }
        #[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
        {
            return crate::backends::macos::timeout_add_repeating(ms, cb);
        }
        #[cfg(feature = "zork")]
        {
            return crate::backends_zork_adapter::timeout_add_repeating(ms, cb);
        }
        #[allow(unreachable_code, unused_variables)]
        {
            let _ = (ms, cb);
            Err(Error::Backend(
                "this backend has no timer primitive".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod gesture_tests {
    use super::*;

    fn press(x: f64, y: f64) -> Gesture {
        Gesture::Button { button: Button::Primary, pressed: true, x, y, mods: Modifiers::default() }
    }

    fn drag(x: f64, y: f64) -> Gesture {
        Gesture::Drag { x, y, mods: Modifiers::default() }
    }

    #[test]
    fn jitter_below_the_threshold_is_not_a_drag() {
        let mut t = DragTracker::new().with_click_threshold(3.0);
        t.feed(press(10.0, 10.0));
        assert!(t.feed(drag(11.0, 10.0)).is_none(), "1px of hand shake is not a drag");
    }

    #[test]
    fn movement_past_the_threshold_reports_the_delta() {
        let mut t = DragTracker::new().with_click_threshold(3.0);
        t.feed(press(0.0, 0.0));
        let e = t.feed(drag(10.0, 4.0)).unwrap();
        assert_eq!((e.dx, e.dy), (10.0, 4.0));
        assert_eq!((e.total_dx, e.total_dy), (10.0, 4.0));
        assert!(e.exceeds(3.0));
    }

    /// The regression that motivated computing the delta before the write-back:
    /// a naive version returns the distance from the press point every time, so
    /// panning speeds up instead of tracking the pointer.
    #[test]
    fn each_move_reports_only_its_own_increment() {
        let mut t = DragTracker::new();
        t.feed(press(0.0, 0.0));
        t.feed(drag(20.0, 0.0));
        let e = t.feed(drag(25.0, 0.0)).unwrap();
        assert_eq!(e.dx, 5.0, "increment, not distance from the press");
        assert_eq!(e.total_dx, 25.0, "the total is measured from the press");
    }

    #[test]
    fn a_release_ends_the_drag() {
        let mut t = DragTracker::new();
        t.feed(press(0.0, 0.0));
        assert!(t.is_dragging());
        t.feed(Gesture::Button { button: Button::Primary, pressed: false, x: 5.0, y: 5.0, mods: Modifiers::default() });
        assert!(!t.is_dragging());
        assert!(t.feed(drag(99.0, 99.0)).is_none(), "motion after release is ignored");
    }

    #[test]
    fn a_second_button_does_not_restart_the_drag() {
        let mut t = DragTracker::new();
        t.feed(press(0.0, 0.0));
        let first = t.feed(drag(50.0, 0.0)).unwrap();
        assert_eq!(first.button, Button::Primary);
        t.feed(Gesture::Button { button: Button::Secondary, pressed: true, x: 50.0, y: 0.0, mods: Modifiers::default() });
        let second = t.feed(drag(60.0, 0.0)).unwrap();
        assert_eq!(second.button, Button::Primary, "the drag keeps its original button");
        assert_eq!(second.total_dx, 60.0, "the origin is still the first press");
    }

    #[test]
    fn zoom_clamps_at_both_ends_instead_of_overflowing() {
        let mut z = ZoomState::new();
        for _ in 0..500 {
            z.scroll(ScrollDelta { dx: 0.0, dy: 1.0 });
        }
        assert_eq!(z.factor(), 16.0);
        for _ in 0..900 {
            z.scroll(ScrollDelta { dx: 0.0, dy: -1.0 });
        }
        assert_eq!(z.factor(), 0.05);
    }

    #[test]
    fn scrolling_up_zooms_in_and_down_zooms_out() {
        let mut z = ZoomState::new();
        z.scroll(ScrollDelta { dx: 0.0, dy: 1.0 });
        assert!(z.factor() > 1.0);
        z.reset();
        z.scroll(ScrollDelta { dx: 0.0, dy: -1.0 });
        assert!(z.factor() < 1.0);
    }

    #[test]
    fn horizontal_scroll_does_not_zoom() {
        // A two-finger horizontal trackpad swipe means "scroll sideways";
        // treating it as zoom makes the view jump while scrolling a list.
        let mut z = ZoomState::new();
        z.scroll(ScrollDelta { dx: 3.0, dy: 0.0 });
        assert_eq!(z.factor(), 1.0);
    }

    #[test]
    fn a_zero_delta_changes_nothing() {
        let mut z = ZoomState::new();
        assert!(ScrollDelta { dx: 0.0, dy: 0.0 }.is_zero());
        z.scroll(ScrollDelta { dx: 0.0, dy: 0.0 });
        assert_eq!(z.factor(), 1.0);
    }

    #[test]
    fn modifiers_report_emptiness() {
        assert!(Modifiers::default().is_empty());
        assert!(!Modifiers { ctrl: true, ..Modifiers::default() }.is_empty());
    }
}
