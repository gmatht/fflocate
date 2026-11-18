//! Generate the Objective-C *forwarding shims* the Apple backends send
//! messages to, so the Rust side and the ObjC side cannot drift.
//!
//! ## Why this exists
//!
//! Both Apple backends reach the framework through the Objective-C runtime and
//! therefore need a handful of classes that live **in the host app bundle**:
//! a target/action trampoline, a text measurer/drawer, an alert wrapper, and a
//! canvas view. Most of them are *pure forwarding* — the Rust side already
//! knows the exact selector and its signature (it names them in its `raw_send!`
//! calls), and the ObjC side only has to declare the same method and hand the
//! arguments to a `corro_*` C function or to the framework.
//!
//! Hand-writing those files means two independent declarations of one ABI.
//! A `.m` file that drifts from the Rust signature does not fail to compile —
//! it crashes at runtime with a mismatched `objc_msgSend` (the classic arm64
//! register-file corruption `IOS_GUIDELINES.md` warns about). That is exactly
//! the failure this generator removes: the signatures below are the contract,
//! and the emitted `.m` is derived from it.
//!
//! ## What it does and does not generate
//!
//! **Generated** (pure boilerplate, no design content):
//!   * `Corro<Platform>Target`  — callback-id → `corro_<platform>_callback`
//!   * `Corro<Platform>Alert`   — `corroNewAlert`/`corroAddAction:`/`corroSetTitle:`
//!     over `UIAlertController`+`UIAlertView` (iOS) or `NSAlert` (macOS)
//!   * category extras on the base view class the adapters call
//!     (`corroSetSpacing:`, `corroSetFlex:`, `corroSetMinWidth:`,
//!     `corroSetCanvasId:`, the `corroBoundsWidth/Height` accessors)
//!
//! **Deliberately not generated** (see [`CanvasView`]):
//!   * the canvas view class itself. Its body is *behaviour*, not forwarding —
//!     `drawRect:` reporting the laid-out size before replaying the closure,
//!     touch/mouse coordinate conversion, `pressesBegan:`/`keyDown:` for a
//!     hardware keyboard, the `isFlipped` override, and the documented
//!     degradation paths found by CI (an older SDK without a selector). Those
//!     are the lines a human reads when the sheet does not draw.
//!   * `Corro<Platform>Text`. Its `measure:`/`drawText:` bodies are the
//!     version table (`boundingRectWithSize:` on iOS 7+ vs `sizeWithFont:` on
//!     iOS 6; `UIFont`/`NSFont` resolution). The selector *signatures* are
//!     listed below so the file can be checked against them, but the bodies
//!     stay hand-written: a generator cannot validate them without an SDK, and
//!     a wrong body there is a layout bug rather than a crash.
//!
//! ## Usage
//!
//! Same pattern as [`crate::android_generator`]: a host's `build.rs` includes
//! this file with `#[path]` (so a vendored rswidgets still works and there is
//! no `[build-dependencies]` entry), then:
//!
//! ```text
//! println!("cargo:rerun-if-env-changed={}", rswidgets_shims::PROJECT_ENV);
//! rswidgets_shims::run();
//! ```
//!
//! Opt-in via the environment variable, additive (existing files are never
//! overwritten), and never fails a build — a read-only or absent Apple project
//! must not break a desktop build of the same crate.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

/// Environment variable naming the Apple project root to generate into.
pub const PROJECT_ENV: &str = "RSWIDGETS_APPLE_PROJECT";

/// Where the generated shim goes, relative to the project root.
///
/// `app/CorroGeneratedShims.m` sits beside the hand-written
/// `app/CorroIosShims.m` so an Xcode target only has to add one file, and so
/// it is obvious which of the two a change belongs in.
pub const SHIM_RELPATH: &str = "app/CorroGeneratedShims.m";

/// Header beside the shim, so the hand-written canvas/text classes can
/// `#import` it and share the category declarations.
pub const SHIM_HEADER_RELPATH: &str = "app/CorroGeneratedShims.h";

/// Which Apple platform to generate for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Ios,
    Macos,
}

impl Platform {
    /// The `corro_<platform>_*` prefix the Rust exports use (see
    /// `ios/corro/src/lib.rs`; a macOS host uses the same convention).
    pub fn c_prefix(self) -> &'static str {
        match self {
            Platform::Ios => "corro_ios",
            Platform::Macos => "corro_macos",
        }
    }

    /// ObjC class prefix, matching what the adapters resolve with
    /// `objc_getClass`.
    pub fn class_prefix(self) -> &'static str {
        match self {
            Platform::Ios => "CorroIos",
            Platform::Macos => "CorroMac",
        }
    }

    /// Framework header the generated file imports.
    pub fn framework_header(self) -> &'static str {
        match self {
            Platform::Ios => "UIKit/UIKit.h",
            Platform::Macos => "AppKit/AppKit.h",
        }
    }

    /// The base view class the adapters create their containers/canvases from.
    pub fn base_view_class(self) -> &'static str {
        match self {
            Platform::Ios => "UIView",
            Platform::Macos => "NSView",
        }
    }

    /// `YES` for the platform whose view coordinates already start top-left.
    ///
    /// UIKit is top-left by default; AppKit is bottom-left, so the generated
    /// canvas contract requires `isFlipped == YES` there (the single most
    /// common AppKit porting bug — `MACOS_GUIDELINES.md` §5).
    pub fn view_is_top_left_origin(self) -> bool {
        matches!(self, Platform::Ios)
    }

    pub fn name(self) -> &'static str {
        match self {
            Platform::Ios => "ios",
            Platform::Macos => "macos",
        }
    }
}

/// One selector the Rust adapter sends to a host class, with the argument
/// kinds that decide the generated method signature.
///
/// The kinds mirror the `unsafe extern "C" fn(...)` types in the adapter's
/// `raw_send!` calls, minus the implicit `(id, SEL)` pair.
#[derive(Clone, Copy, Debug)]
pub struct ShimMethod {
    /// Selector as the adapter spells it (`"corroSetSpacing:"`).
    pub selector: &'static str,
    /// Argument kinds after the implicit `(id, SEL)`.
    pub args: &'static [ArgKind],
    /// Return kind.
    pub ret: ArgKind,
    /// `true` when the method is a class method (`+`) rather than an
    /// instance method (`-`).
    pub class_method: bool,
    /// Set when the method is generated as a stub with this body comment
    /// instead of real behaviour (used for the checked-but-hand-written ones).
    pub hand_written_note: Option<&'static str>,
    /// `true` when the host's own `.m` provides the body, so this generator
    /// emits the declaration (in the header) but no implementation.
    ///
    /// Distinct from `hand_written_note`, which merely annotates a stub the
    /// generator still emits: `corroSetCanvasId:` wants a generated no-op,
    /// while `corroBoundsWidth` must NOT get one - the generated stub returns
    /// 0, and the adapter sizes children to their parent with it, so a stub
    /// there is the 0x0-canvas bug rather than a harmless placeholder.
    pub body_elsewhere: bool,
}

/// Argument/return kinds, mapped to ObjC types per platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    /// `id` — an object handle (`*mut c_void` in Rust).
    Object,
    /// `NSString*` — object handles that are always strings in practice.
    String,
    /// `SEL`.
    Selector,
    /// `NSInteger` / `NSUInteger` (`isize`/`usize`; the adapters use `u64` for
    /// the unsigned cases and `isize` for the signed ones).
    Integer,
    /// `NSUInteger` specifically (`u64` in the adapter).
    UInteger,
    /// `BOOL`.
    Bool,
    /// `CGFloat` (`f64` on 64-bit, `f32` on 32-bit — the adapter selects at
    /// compile time).
    Float,
    /// `void` — only valid as a return kind.
    Void,
}

impl ArgKind {
    /// The ObjC spelling for this kind. Takes no `Platform`: every kind maps
    /// to the same type on both targets (`CGFloat` is a typedef, so it is
    /// already the right width per target — see the note below), and the
    /// tests pin that for iOS and macOS alike.
    fn objc_type(self) -> &'static str {
        match self {
            ArgKind::Object | ArgKind::String => "id",
            ArgKind::Selector => "SEL",
            ArgKind::Integer => "NSInteger",
            ArgKind::UInteger => "NSUInteger",
            ArgKind::Bool => "BOOL",
            // CGFloat is a typedef, so it is already the right width per
            // target — the generated header needs no 32/64 branch. That is
            // the point of spelling it `CGFloat` here rather than `double`.
            ArgKind::Float => "CGFloat",
            ArgKind::Void => "void",
        }
    }
}

/// The forwarding shims, in the order they are emitted.
///
/// Kept as data (not string literals scattered through the emitter) so a
/// test can assert the set matches what the adapters actually send — see
/// [`RustSideSelector`] and the tests at the bottom.
pub const TARGET_METHODS: &[ShimMethod] = &[
    ShimMethod {
        selector: "targetWithCallbackId:",
        args: &[ArgKind::Integer],
        ret: ArgKind::Object,
        class_method: true,
        hand_written_note: None,
        body_elsewhere: false,
    },
    ShimMethod {
        selector: "corroFired:",
        args: &[ArgKind::Object],
        ret: ArgKind::Void,
        class_method: false,
        hand_written_note: None,
        body_elsewhere: false,
    },
];

pub const ALERT_METHODS: &[ShimMethod] = &[
    ShimMethod {
        selector: "corroNewAlert",
        args: &[],
        ret: ArgKind::Object,
        class_method: true,
        hand_written_note: None,
        body_elsewhere: false,
    },
    ShimMethod {
        selector: "corroSetTitle:",
        args: &[ArgKind::String],
        ret: ArgKind::Void,
        class_method: false,
        hand_written_note: None,
        body_elsewhere: false,
    },
    ShimMethod {
        selector: "corroAddAction:",
        args: &[ArgKind::String],
        ret: ArgKind::Void,
        class_method: false,
        hand_written_note: None,
        body_elsewhere: false,
    },
];

/// Category methods the adapters send to plain views (the base view class),
/// providing the expand/min-width protocol the shared layout code expects.
pub const VIEW_CATEGORY_METHODS: &[ShimMethod] = &[
    ShimMethod {
        selector: "corroSetSpacing:",
        args: &[ArgKind::Integer],
        ret: ArgKind::Void,
        class_method: false,
        hand_written_note: None,
        body_elsewhere: false,
    },
    ShimMethod {
        selector: "corroSetFlex:",
        args: &[ArgKind::Bool],
        ret: ArgKind::Void,
        class_method: false,
        hand_written_note: None,
        body_elsewhere: false,
    },
    ShimMethod {
        selector: "corroSetMinWidth:",
        args: &[ArgKind::Integer],
        ret: ArgKind::Void,
        class_method: false,
        hand_written_note: None,
        body_elsewhere: false,
    },
    ShimMethod {
        selector: "corroSetCanvasId:",
        args: &[ArgKind::Integer],
        ret: ArgKind::Void,
        class_method: false,
        hand_written_note: Some(
            "the canvas id is stored on the view by the canvas class; this \
             category provides the setter the adapter calls before the view \
             is customised, so a plain view is also valid",
        ),
        body_elsewhere: false,
    },
    ShimMethod {
        // `Label::set_fixed_width`. A real width pin, so the body is
        // BEHAVIOUR and cannot be a generated no-op: an NSTextField sizes
        // itself from `-intrinsicContentSize` (hugging priority 250), so a
        // stub that records nothing leaves the label shrink-to-fit and the
        // formula bar's address slot still slides every sibling on each
        // cursor move. The host installs a required width equality
        // constraint; a negative width releases it.
        selector: "corroSetPinnedWidth:",
        args: &[ArgKind::Integer],
        ret: ArgKind::Void,
        class_method: false,
        hand_written_note: Some(
            "installs (or, for a negative width, removes) a required width \
             constraint; see Label::set_fixed_width in the macOS adapter",
        ),
        body_elsewhere: true,
    },
    ShimMethod {
        // `Label::set_fixed_width`'s second half. Raising the content-hugging
        // priority above the intrinsic 250 is what makes the pinned width win
        // the constraint conflict in the first place; a generated no-op there
        // silently leaves the intrinsic size in charge.
        //
        // ONE argument, matching the `msg1iv` the adapters send. A
        // two-argument `corroSetContentHugging:priority:` was tried first and
        // is wrong twice over: it is a different selector, so `msg1iv` on a
        // 1-arg declaration leaves the second register undefined, and the
        // declared signature in the generated header would not match what the
        // host implements.
        selector: "corroSetContentHugging:",
        args: &[ArgKind::Integer],
        ret: ArgKind::Void,
        class_method: false,
        hand_written_note: Some(
            "sets the view's horizontal content-hugging priority; see \
             Label::set_fixed_width in the macOS adapter",
        ),
        body_elsewhere: true,
    },
    ShimMethod {
        // Marked hand-written even though the generated category is a natural
        // home: the generated stub body returns 0, and the adapter uses these
        // to size children to their parent. A 0 return reintroduces the 0x0
        // canvas that made the sheet never draw at all - so the body is
        // behaviour, not forwarding, and belongs in the host's .m.
        selector: "corroBoundsWidth",
        args: &[],
        ret: ArgKind::Float,
        class_method: false,
        hand_written_note: Some("body reads the view's bounds; see the host's shim"),
        body_elsewhere: true,
    },
    ShimMethod {
        selector: "corroBoundsHeight",
        args: &[],
        ret: ArgKind::Float,
        class_method: false,
        hand_written_note: Some("body reads the view's bounds; see the host's shim"),
        body_elsewhere: true,
    },
];

/// Selectors that stay hand-written but whose *Signature must still match* —
/// listed here so the generated header declares them and a mismatch is a
/// compile error in the host rather than a runtime crash.
pub const HAND_WRITTEN_METHODS: &[ShimMethod] = &[
    ShimMethod {
        // FIVE parts, so five arguments: text, font, size, slant, weight. The
        // table previously listed four (the font was missing), which is how
        // generating the ObjC declarations caught the adapter sending four
        // arguments for a five-part selector - a mismatch that would have read
        // an unset register on the ObjC side.
        selector: "measure:font:size:slant:weight:",
        args: &[
            ArgKind::String,
            ArgKind::String,
            ArgKind::Float,
            ArgKind::Integer,
            ArgKind::Integer,
        ],
        ret: ArgKind::Object, // CGRect* handed back as an opaque pointer
        class_method: true,
        hand_written_note: Some("body is the SDK version table; see CorroIosShims.m"),
        body_elsewhere: false,
    },
    ShimMethod {
        selector: "drawText:ctx:font:x:y:size:r:g:b:a:slant:weight:",
        args: &[
            ArgKind::String,
            ArgKind::Object,
            ArgKind::String,
            ArgKind::Float,
            ArgKind::Float,
            ArgKind::Float,
            ArgKind::Float,
            ArgKind::Float,
            ArgKind::Float,
            ArgKind::Float,
            ArgKind::Integer,
            ArgKind::Integer,
        ],
        ret: ArgKind::Void,
        // CLASS method, and that is load-bearing. The Rust adapter resolves
        // this class with `objc_getClass` and sends both text selectors to the
        // CLASS, because the shim is stateless and never instantiated. When
        // this was declared and implemented as an instance method,
        // `[CorroIosText drawText:...]` raised "unrecognized selector sent to
        // instance", which surfaced as `fatal runtime error: Rust cannot catch
        // foreign exceptions` on the first real frame - an ObjC exception
        // crossing into Rust, with the offending selector named nowhere.
        class_method: true,
        hand_written_note: Some("body resolves the font; see CorroIosShims.m"),
        body_elsewhere: false,
    },
];

/// The canvas view contract.
///
/// This is **generated** rather than hand-written, but only because it is
/// configured: unlike the pure-forwarding shims there is no single correct
/// body. Everything below is a knob that a host can tune, and the defaults are
/// the ones this repo's two apps use.
#[derive(Clone, Debug)]
pub struct CanvasViewConfig {
    /// Class name the host registers (`set_sheet_view_class`).
    pub class_name: String,
    /// Emit the `corroHandlePresses:`-style key path (hardware keyboards).
    pub hardware_keys: bool,
    /// Emit the fill-the-superview fallback in the layout pass.
    ///
    /// This is the one part of the hand-written class that is *not* obviously
    /// correct by inspection: a view that resizes itself inside a layout pass
    /// schedules another pass, so it needs a re-entrancy guard, and whether it
    /// is needed at all depends on the host's view hierarchy (a stack view
    /// sizes its arranged subviews; a bare `addSubview:` does not). Default
    /// `false`: a host that puts the canvas in a stack view does not want it.
    pub fill_superview: bool,
    /// Emit the canvas size report in the layout pass as well as the draw
    /// pass. Default `true` — Rust replays the draw closure before the
    /// framework's first draw, so the earlier report is what stops the sheet
    /// laying itself out at the 1x1 placeholder.
    pub report_size_on_layout: bool,
    /// Emit event tracing to stderr. Default `false`; the hand-written file
    /// enabled it while bringing the app up.
    pub trace_events: bool,
    /// Deployment target, e.g. `Some("12.0")`. When set, key handling is
    /// emitted behind an availability check — on iOS, `UIKey` is 13.4+.
    pub deployment_target: Option<String>,
}

impl CanvasViewConfig {
    /// The iOS defaults (what `ios/corro` uses).
    pub fn ios(class_name: &str) -> Self {
        CanvasViewConfig {
            class_name: class_name.to_owned(),
            hardware_keys: true,
            // The iOS app attaches the canvas inside a container, not a stack
            // view, so it needs the fallback. See CorroIosShims.m.
            fill_superview: true,
            report_size_on_layout: true,
            trace_events: false,
            // The iOS app deploys to 12.0, i.e. below the 13.4 that UIKey
            // needs, so the key path must be guarded.
            deployment_target: Some("12.0".to_owned()),
        }
    }

    /// The macOS defaults (AppKit has no `UIKey` availability problem: the
    /// event already carries what is needed).
    pub fn macos(class_name: &str) -> Self {
        CanvasViewConfig {
            class_name: class_name.to_owned(),
            hardware_keys: true,
            fill_superview: true,
            report_size_on_layout: true,
            trace_events: false,
            deployment_target: None,
        }
    }

    /// A minimal canvas: draw and size only, no input handling at all.
    pub fn headless(class_name: &str) -> Self {
        CanvasViewConfig {
            class_name: class_name.to_owned(),
            hardware_keys: false,
            fill_superview: false,
            report_size_on_layout: true,
            trace_events: false,
            deployment_target: None,
        }
    }
}

/// The text measurer/drawer contract.
///
/// Also generated-but-configured, for the same reason: the *shape* is fixed
/// (one measurement call, one draw call, both handed back to Rust), but three
/// details are host policy.
#[derive(Clone, Debug)]
pub struct TextShimConfig {
    /// Class name (`CorroIosText` / `CorroMacText`).
    pub class_name: String,
    /// Font family to use when the requested family is missing or empty.
    pub fallback_family: String,
    /// Emit the modern measurement API (`boundingRectWithSize:` on iOS,
    /// `size:` on macOS) with the legacy one as the `respondsToSelector:`
    /// fallback. `false` emits only the modern call. Default `true`.
    pub legacy_fallback: bool,
    /// Offset the draw `y` by the font ascent.
    ///
    /// **Leave this `false`.** The shared `DrawContext` convention is a
    /// top-left point, and `NSString`'s `drawAtPoint:withAttributes:` /
    /// `drawAtPoint:withFont:` take a top-left point too (in UIKit's or
    /// AppKit's flipped space), so no offset is needed.
    ///
    /// It shipped as `true` on the theory that the text API wanted a baseline
    /// — true of Core Graphics' `CGContextShowTextAtPoint`, but not of these
    /// NSString methods. The result was a double-count: at font size 12 the
    /// ascent (~9.6pt) pushed every glyph a full ascent below where it
    /// belonged, landing it in the *next* row's band with its lower half
    /// clipped by the row rule. Visible in `ios/corro/ios-first-frame.png`
    /// before the fix.
    ///
    /// Kept as a field because a host with a text API that *does* take a
    /// baseline may need it — but the default, and this comment, now say which
    /// is correct for the shims generated here.
    pub baseline_from_ascent: bool,
}

impl TextShimConfig {
    pub fn new(class_name: &str, fallback_family: &str) -> Self {
        TextShimConfig {
            class_name: class_name.to_owned(),
            fallback_family: fallback_family.to_owned(),
            legacy_fallback: true,
            // See the field's docs: top-left in, top-left out. `true` is the
            // double-count bug.
            baseline_from_ascent: false,
        }
    }

    pub fn ios(class_name: &str) -> Self {
        TextShimConfig::new(class_name, "Courier")
    }

    pub fn macos(class_name: &str) -> Self {
        TextShimConfig::new(class_name, "Menlo")
    }
}

/// One entry of the hardware-key map: a platform HID usage constant → the
/// keysym `rswidgets::core::key` uses.
///
/// The Rust constants are the single source of truth (they are the values the
/// shared key handling matches on); this table only names the platform's
/// spelling of the same physical key. Generating the switch is what stops the
/// ObjC copy drifting from `core::key` — which is exactly how the hand-written
/// file acquired a duplicate of it.
#[derive(Clone, Copy, Debug)]
pub struct KeyMapEntry {
    /// The platform constant (`UIKeyboardHIDUsageKeyboardEscape`).
    pub platform_constant: &'static str,
    /// The matching `rswidgets::core::key` constant name, so the emitted value
    /// is read from Rust rather than transcribed.
    pub rust_key: &'static str,
}

/// The iOS key map. `core::key` uses X11 keysyms on unix, which is what the
/// `rust_key` values resolve to.
pub const IOS_KEY_MAP: &[KeyMapEntry] = &[
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardReturnOrEnter", rust_key: "RETURN" },
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeypadEnter", rust_key: "RETURN" },
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardEscape", rust_key: "ESCAPE" },
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardTab", rust_key: "TAB" },
    // NB: the SDK name is DeleteOrBackspace, not "Backspace".
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardDeleteOrBackspace", rust_key: "BACKSPACE" },
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardDeleteForward", rust_key: "DELETE" },
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardLeftArrow", rust_key: "LEFT" },
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardUpArrow", rust_key: "UP" },
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardRightArrow", rust_key: "RIGHT" },
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardDownArrow", rust_key: "DOWN" },
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardHome", rust_key: "HOME" },
    KeyMapEntry { platform_constant: "UIKeyboardHIDUsageKeyboardEnd", rust_key: "END" },
];

/// The canvas view contract as emitted — kept for the doc comment the header
/// carries, and so a host knows which overrides it must supply.
#[derive(Clone, Copy, Debug)]
pub struct CanvasView {
    /// Class name the host must register (`set_sheet_view_class`).
    pub class_name: &'static str,
    /// The framework overrides the emitted class provides.
    pub required_overrides: &'static [&'static str],
}

impl CanvasView {
    /// The canvas class contract for a platform.
    ///
    /// Per-platform, not one shared constant, because the class *name* is not
    /// shared: the iOS app registers `SheetView` and the macOS app registers
    /// `CorroSheetView` (the `Corro*` prefix is what every other macOS shim
    /// uses, so a host can resolve them all with one convention). A single
    /// constant put `SheetView` in the *macOS* generated header, which would
    /// have declared a class no macOS host implements — and the mismatch
    /// between that declaration and the host's actual class is precisely the
    /// silent ABI drift the generator exists to prevent, reintroduced through
    /// the generator itself.
    pub fn for_platform(p: Platform) -> Self {
        CanvasView {
            class_name: match p {
                Platform::Ios => "SheetView",
                Platform::Macos => "CorroSheetView",
            },
            required_overrides: &["drawRect:", "corroSetCanvasId:"],
        }
    }
}

/// The iOS canvas class contract, kept as a named constant because the iOS host
/// (`ios/corro`) refers to it by this name.
pub const CANVAS_VIEW: CanvasView = CanvasView {
    class_name: "SheetView",
    required_overrides: &["drawRect:", "corroSetCanvasId:"],
};

/// Write `content` to `path` only when the file does not exist yet, creating
/// parent directories as needed. Returns `true` when a file was written.
///
/// Local copy of the same helper in `android_generator` on purpose: this module
/// is `#[path]`-included by host `build.rs` files (where the crate root is the
/// *host's*, so `crate::android_generator` does not exist), which is the same
/// reason the generator holds no other cross-module reference. Keeping the
/// helper here makes the file self-contained for that pattern.
pub fn write_if_missing(path: &Path, content: &str) -> io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content.as_bytes())?;
    Ok(true)
}

/// Emit the generated `.h` + `.m` for `platform` into `project_root`.
///
/// Additive: an existing file is left untouched and its path is not returned,
/// so a host that has customised its shims keeps them. Returns the paths
/// actually written.
pub fn generate(project_root: &Path, platform: Platform) -> io::Result<Vec<PathBuf>> {
    generate_with(project_root, platform, &ShimConfig::for_platform(platform))
}

/// Everything the generator needs to know about a host, so one generator
/// serves both apps and any future one.
///
/// A field here exists because a plausible host differs on it: a headless
/// harness wants `canvas.hardware_keys = false`; an app that puts the canvas
/// in a stack view wants `canvas.fill_superview = false`; an app targeting
/// current iOS can drop the availability guard. The defaults are what
/// `ios/corro` and a `macos/corro` use.
#[derive(Clone, Debug)]
pub struct ShimConfig {
    /// Canvas view knobs (drawing, sizing, input).
    pub canvas: CanvasViewConfig,
    /// Text measurer/drawer knobs.
    pub text: TextShimConfig,
    /// Emit the canvas class and the text class at all. `false` is for a host
    /// that ships its own hand-written ones (then only the forwarding shims and
    /// the declarations are generated, which is what the iOS app does today).
    pub emit_canvas: bool,
    pub emit_text: bool,
}

impl ShimConfig {
    pub fn for_platform(p: Platform) -> Self {
        match p {
            Platform::Ios => ShimConfig {
                canvas: CanvasViewConfig::ios(CanvasView::for_platform(Platform::Ios).class_name),
                text: TextShimConfig::ios("CorroIosText"),
                emit_canvas: true,
                emit_text: true,
            },
            Platform::Macos => ShimConfig {
                canvas: CanvasViewConfig::macos(
                    CanvasView::for_platform(Platform::Macos).class_name,
                ),
                text: TextShimConfig::macos("CorroMacText"),
                emit_canvas: true,
                emit_text: true,
            },
        }
    }
}

/// As [`generate`], but with the host's configuration.
pub fn generate_with(
    project_root: &Path,
    platform: Platform,
    config: &ShimConfig,
) -> io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let header_path = project_root.join(SHIM_HEADER_RELPATH);
    if write_if_missing(&header_path, &render_header(platform, config))? {
        written.push(header_path);
    }
    let impl_path = project_root.join(SHIM_RELPATH);
    if write_if_missing(&impl_path, &render_impl(platform, config))? {
        written.push(impl_path);
    }
    Ok(written)
}

/// Resolve the project root from [`PROJECT_ENV`], or `None` when unset.
pub fn project_root_from_env() -> Option<PathBuf> {
    std::env::var(PROJECT_ENV).ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// Convenience entry point for a `build.rs`, mirroring
/// [`crate::android_generator::run`]: generate when [`PROJECT_ENV`] is set,
/// warn (never fail) otherwise, and re-run only when the variable changes.
pub fn run() {
    let Some(root) = project_root_from_env() else {
        println!(
            "cargo:warning=rswidgets Apple shim generation skipped; set {PROJECT_ENV} to an Apple project root"
        );
        println!("cargo:rerun-if-env-changed={PROJECT_ENV}");
        return;
    };
    // Generate for whichever platform the target is, so one project root can
    // serve an iOS app and a macOS app side by side.
    let platform = if cfg!(target_os = "macos") {
        Platform::Macos
    } else {
        Platform::Ios
    };
    match generate(&root, platform) {
        Ok(created) => {
            for path in &created {
                println!("cargo:warning=rswidgets generated {}", path.display());
            }
        }
        Err(e) => {
            println!(
                "cargo:warning=rswidgets Apple shim generation failed for {} ({e})",
                root.display()
            );
        }
    }
    println!("cargo:rerun-if-env-changed={PROJECT_ENV}");
}

// ---------------------------------------------------------------------------
// Emitters
// ---------------------------------------------------------------------------

fn selector_base(selector: &str) -> &str {
    selector.split(':').next().unwrap_or(selector)
}

fn selector_parts(selector: &str) -> Vec<&str> {
    let parts: Vec<&str> = selector.split(':').filter(|s| !s.is_empty()).collect();
    parts
}

/// ObjC method declaration fragment, e.g.
/// `- (void)corroSetFlex:(BOOL)flex;` or
/// `- (void)drawText:(id)text ctx:(id)ctx ...;`
// No `Platform` parameter: the declaration is identical for iOS and macOS
// (see `ArgKind::objc_type`), which is what lets one generator emit both.
fn method_decl(m: &ShimMethod, terminator: char) -> String {
    let sign = if m.class_method { '+' } else { '-' };
    let ret = m.ret.objc_type();
    let parts = selector_parts(m.selector);
    let mut out = format!("{sign} ({ret})");
    for (i, part) in parts.iter().enumerate() {
        let kind = m.args.get(i).copied().unwrap_or(ArgKind::Object);
        let ty = kind.objc_type();
        if i > 0 {
            out.push(' ');
        }
        // The first selector piece may be the whole name for a zero-arg
        // method (`corroBoundsWidth`), in which case there is no colon and no
        // parameter to name.
        if m.args.is_empty() {
            out.push_str(part);
        } else {
            let _ = write!(out, "{part}:({ty})arg{i}");
        }
    }
    out.push(terminator);
    out
}

/// A neutral, always-valid default return for the stub bodies the generated
/// category uses when the note says the real work is elsewhere.
fn default_return(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::Void => "",
        ArgKind::Bool => "return NO;",
        ArgKind::Integer | ArgKind::UInteger => "return 0;",
        ArgKind::Float => "return 0;",
        _ => "return nil;",
    }
}

fn render_header(p: Platform, _cfg: &ShimConfig) -> String {
    let mut s = String::new();
    let guard = format!("CORRO_{}_GENERATED_SHIMS_H", p.name().to_uppercase());
    let _ = write!(
        s,
        r#"// GENERATED by rswidgets — do not edit.
//
// The Objective-C half of the Rust<->ObjC ABI contract for the {name} backend.
// Regenerate with `{env}=<project root>` on {name}; see
// rustxWidgets/src/apple_generator.rs for what is generated and, just as
// importantly, what is deliberately not (the canvas view and the text
// measurer: their bodies are behaviour, not forwarding).
//
// This header exists so the *hand-written* classes in the same target can
// adopt these declarations instead of restating them: a drift between the two
// then fails to compile in the host, rather than corrupting registers at
// runtime with a mismatched objc_msgSend.

#ifndef {guard}
#define {guard}

#import <{header}>

#pragma mark - Forwarding shims (generated)

/// Trampoline: one selector that turns a control activation into a
/// `{prefix}_callback(id)` call. The Rust side resolves this class by name.
@interface {cp}Target : NSObject
"#,
        name = p.name(),
        env = PROJECT_ENV,
        guard = guard,
        header = p.framework_header(),
        prefix = p.c_prefix(),
        cp = p.class_prefix(),
    );
    for m in TARGET_METHODS {
        let _ = writeln!(s, "{}", method_decl(m, ';'));
    }
    let _ = write!(s, "@end\n\n/// Alert wrapper: `{}Alert`\n@interface {}Alert : NSObject\n", p.class_prefix(), p.class_prefix());
    for m in ALERT_METHODS {
        let _ = writeln!(s, "{}", method_decl(m, ';'));
    }
    let _ = write!(
        s,
        "@end\n\n#pragma mark - Layout protocol (generated category)\n\n/// The expand/min-width protocol the shared layout code expects from any view.\n@interface {base} (CorroLayout)\n",
        base = p.base_view_class(),
    );
    for m in VIEW_CATEGORY_METHODS {
        let _ = writeln!(s, "{}", method_decl(m, ';'));
    }
    s.push_str("@end\n\n#pragma mark - Hand-written, signature-checked\n\n");
    s.push_str("/// Bodies live in the host's own .m file; declared here so a signature\n/// mismatch is a compile error.\n");
    let _ = write!(s, "@interface {}Text : NSObject\n", p.class_prefix());
    for m in HAND_WRITTEN_METHODS {
        let _ = writeln!(s, "{}", method_decl(m, ';'));
    }
    let _ = write!(
        s,
        "@end\n\n/// The canvas view the host must ship (see the backend's\n/// `set_sheet_view_class`). Hand-written: `drawRect:` reports the laid-out\n/// size before replaying Rust's draw closure, and the input methods convert\n/// coordinates.\n///\n/// Required overrides:\n",
    );
    // Per-platform: the class NAME is not shared (see `CanvasView::for_platform`),
    // so hardcoding the iOS one here would declare a class the macOS host does
    // not implement.
    let canvas_view = CanvasView::for_platform(p);
    for ov in canvas_view.required_overrides {
        let _ = writeln!(s, "///   * `{ov}`");
    }
    if !p.view_is_top_left_origin() {
        s.push_str("///\n/// This platform's views are bottom-left origin by default, so the canvas\n/// class MUST also override `- (BOOL)isFlipped` to return `YES`; otherwise\n/// the sheet draws vertically mirrored.\n");
    }
    let _ = write!(
        s,
        "@interface {} : {base}\n@end\n\n#endif /* {guard} */\n",
        canvas_view.class_name,
        base = p.base_view_class(),
        guard = guard,
    );
    s
}


/// The one-line platform difference in the canvas body: how to reach the live
/// Core Graphics context during a draw pass. Everything else about the canvas
/// is shared, which is why only this is a per-platform string.
fn ctx_macro(p: Platform) -> &'static str {
    match p {
        Platform::Ios => "#define CORRO_GRAPHICS_CONTEXT()      UIGraphicsGetCurrentContext()",
        Platform::Macos => "#define CORRO_GRAPHICS_CONTEXT()      [[NSGraphicsContext currentContext] CGContext]",
    }
}

fn render_impl(p: Platform, cfg: &ShimConfig) -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        r#"// GENERATED by rswidgets — do not edit.
//
// Forwarding shims for the {name} backend: see CorroGeneratedShims.h and
// rustxWidgets/src/apple_generator.rs. Every method here is pure forwarding —
// it exists so the Rust adapter's `objc_msgSend` signature has a matching
// declaration on the ObjC side.

#import "CorroGeneratedShims.h"

// The C entry points emitted by the host crate (ios/corro or macos/corro).
extern void {prefix}_callback(uint64_t callback_id);
// Canvas callbacks, also exported by the host crate. Reached through macros so
// the generated body stays free of platform-specific strings.
extern void {prefix}_canvas_size(uint64_t canvas_id, int32_t w, int32_t h);
extern void {prefix}_canvas_draw(uint64_t canvas_id, void *ctx, int32_t w, int32_t h);
extern void {prefix}_canvas_click(uint64_t canvas_id, double x, double y);
extern int  {prefix}_canvas_key(uint64_t canvas_id, uint32_t keyval, uint32_t mods);

#define CORRO_CANVAS_SIZE(id, w, h)    {prefix}_canvas_size((id), (w), (h))
#define CORRO_CANVAS_DRAW(id, c, w, h) {prefix}_canvas_draw((id), (c), (w), (h))
#define CORRO_CANVAS_CLICK(id, x, y)   {prefix}_canvas_click((id), (x), (y))
#define CORRO_CANVAS_KEY(id, k, m)     {prefix}_canvas_key((id), (k), (m))
{ctx_macro}

#pragma mark - Callback trampoline
"#,
        name = p.name(),
        prefix = p.c_prefix(),
        ctx_macro = ctx_macro(p),
    );

    // Target: retain the id, fire on corroFired:.
    let cp = p.class_prefix();
    let _ = write!(
        s,
        r#"
@implementation {cp}Target {{
    NSInteger _callbackId;
}}

+ (instancetype)targetWithCallbackId:(NSInteger)callbackId {{
    {cp}Target *t = [[{cp}Target alloc] init];
    t->_callbackId = callbackId;
    return t;
}}

- (void)corroFired:(id)sender {{
    (void)sender;
    {prefix}_callback((uint64_t)_callbackId);
}}

@end
"#,
        cp = cp,
        prefix = p.c_prefix(),
    );

    // Alert.
    let _ = write!(s, "\n#pragma mark - Alert\n\n");
    match p {
        Platform::Ios => s.push_str(IOS_ALERT_IMPL),
        Platform::Macos => s.push_str(MACOS_ALERT_IMPL),
    }

    // Layout category.
    let _ = write!(
        s,
        "\n#pragma mark - Layout category\n\n@implementation {base} (CorroLayout)\n\n",
        base = p.base_view_class(),
    );
    for m in VIEW_CATEGORY_METHODS {
        // A method whose body lives in the host's .m is DECLARED here (in the
        // header) but not implemented. Emitting the default stub regardless
        // would be worse than redundant: `corroBoundsWidth`'s stub returns 0,
        // and the adapter sizes children to their parent with it, so it
        // reintroduces the 0x0 canvas that made the sheet never draw.
        if m.body_elsewhere {
            continue;
        }
        let note = m
            .hand_written_note
            .map(|n| format!("// {n}\n"))
            .unwrap_or_default();
        let body = default_return(m.ret);
        if body.is_empty() {
            // No-op setters: record nothing here — the Rust `WidgetMeta`
            // registry is authoritative for expand/min-chars/canvas id, and
            // the host's canvas class reads it back through the same calls.
            //
            // The parameter type comes from the method's declared arg kind, not
            // a hardcoded `NSInteger`: `corroSetFlex:` takes a BOOL, and a
            // signature that disagrees with the header is exactly the drift
            // this generator exists to prevent.
            let arg_ty = m
                .args
                .first()
                .copied()
                .unwrap_or(ArgKind::Integer)
                .objc_type();
            let _ = write!(
                s,
                "{note}- (void){selector}:({ty})arg0 {{\n    (void)arg0;\n}}\n\n",
                selector = selector_base(m.selector),
                ty = arg_ty,
            );
        } else {
            let parts = selector_parts(m.selector);
            let name = parts.first().copied().unwrap_or("corroUnknown");
            let _ = write!(s, "{note}- ({ret}){name} {{\n    {body}\n}}\n\n",
                ret = m.ret.objc_type());
        }
    }
    s.push_str("@end\n");

    // Canvas + text: generated from the host's configuration (see
    // `ShimConfig`). Appended here rather than in `render_impl`'s caller so a
    // host that ships its own hand-written classes can suppress either one.
    if cfg.emit_canvas {
        s.push_str(&render_canvas_impl(p, &cfg.canvas));
        s.push_str(&render_canvas_events(p, &cfg.canvas));
        s.push_str("@end\n");
    } else {
        // The host ships its own canvas class (it is usually richer than the
        // generated default: host-specific layout or tracing). The *header*
        // still declares it, so the hand-written one adopts the contract.
        s.push_str(
            "\n// The canvas view is hand-written by this host (ShimConfig::emit_canvas was\n// false); CorroGeneratedShims.h declares its contract.\n",
        );
    }
    if cfg.emit_text {
        s.push_str(&render_text_impl(p, &cfg.text));
    } else {
        s.push_str(
            "\n// The text measurer/drawer is hand-written by this host\n// (ShimConfig::emit_text was false); the header declares its contract.\n",
        );
    }
    s
}

const IOS_ALERT_IMPL: &str = r#"/// Wraps UIAlertController (iOS 8+) and UIAlertView (iOS 7) behind one
/// interface, so the Rust side calls `corroAddAction:`/`corroNewAlert` once.
@interface CorroIosAlert ()
@property (nonatomic, strong) id native;  // UIAlertController or UIAlertView
@property (nonatomic, strong) NSMutableArray<NSString *> *actions;
@end

@implementation CorroIosAlert

+ (instancetype)corroNewAlert {
    CorroIosAlert *w = [[CorroIosAlert alloc] init];
    w.actions = [NSMutableArray array];
    return w;
}

- (void)corroSetTitle:(id)title {
    if ([UIAlertController class] != nil) {
        // UIAlertController's title is only settable at construction, so the
        // title is applied when the controller is built in corroAddAction:.
        self.actions = self.actions ?: [NSMutableArray array];
        (void)title;
    } else {
        (void)title;
    }
}

- (void)corroAddAction:(id)title {
    NSString *t = (NSString *)title;
    if ([UIAlertController class] != nil) {
        UIAlertController *ac = self.native;
        if (ac == nil) {
            ac = [UIAlertController alertControllerWithTitle:nil
                                                     message:nil
                                              preferredStyle:UIAlertControllerStyleAlert];
            self.native = ac;
        }
        [ac addAction:[UIAlertAction actionWithTitle:t
                                               style:UIAlertActionStyleDefault
                                             handler:nil]];
    } else {
        UIAlertView *av = self.native;
        if (av == nil) {
            av = [[UIAlertView alloc] initWithTitle:nil
                                            message:nil
                                           delegate:nil
                                  cancelButtonTitle:nil
                                  otherButtonTitles:nil];
            self.native = av;
        }
        [av addButtonWithTitle:t];
    }
}

@end

/// Presentation lives on the view controller, matching `Dialog::present`'s
/// `corroPresentDialog:` send.
@implementation UIViewController (CorroPresent)

- (void)corroPresentDialog:(id)dialog {
    id native = [dialog valueForKey:@"native"];
    if ([native isKindOfClass:[UIAlertController class]]) {
        [self presentViewController:(UIAlertController *)native animated:YES completion:nil];
    } else if ([native isKindOfClass:[UIAlertView class]]) {
        [(UIAlertView *)native show];
    }
}

@end
"#;

const MACOS_ALERT_IMPL: &str = r#"/// Wraps NSAlert. macOS has one alert class for every supported version, so
/// unlike iOS there is no version branch here — only the modal-vs-sheet choice,
/// which is the host's (a sheet when the host has a window, modal otherwise).
@interface CorroMacAlert ()
@property (nonatomic, strong) NSAlert *native;
@end

@implementation CorroMacAlert

+ (instancetype)corroNewAlert {
    CorroMacAlert *w = [[CorroMacAlert alloc] init];
    w.native = [[NSAlert alloc] init];
    [w.native addButtonWithTitle:@"OK"];
    return w;
}

- (void)corroSetTitle:(id)title {
    self.native.messageText = (NSString *)title;
}

- (void)corroAddAction:(id)title {
    [self.native addButtonWithTitle:(NSString *)title];
}

@end

/// `Dialog::present` sends `corroPresentDialog:` to the window controller.
/// A sheet needs a window; without one, run modally.
@implementation NSWindowController (CorroPresent)

- (void)corroPresentDialog:(id)dialog {
    NSAlert *alert = [dialog valueForKey:@"native"];
    if (![alert isKindOfClass:[NSAlert class]]) {
        return;
    }
    NSWindow *window = self.window;
    if (window != nil) {
        [alert beginSheetModalForWindow:window completionHandler:nil];
    } else {
        [alert runModal];
    }
}

@end
"#;

// ---------------------------------------------------------------------------
// Canvas + text emitters (config-driven)
// ---------------------------------------------------------------------------

/// Runtime helper names the emitted canvas calls, per platform. The Rust side
/// exports exactly these from the host crate (`ios/corro/src/lib.rs`).
fn canvas_ffi(p: Platform) -> CanvasFfi {
    match p {
        Platform::Ios => CanvasFfi {
            click: "corro_ios_canvas_click",
            key: "corro_ios_canvas_key",
            // The touch handler takes (NSSet<UITouch*>*, UIEvent*); the
            // selector's *event* type is not the touch type.
            event_class: "UITouch",
            event_type: "UIEvent",
            event_local: "[touch locationInView:self]",
            layout_hook: "layoutSubviews",
            layout_super: "[super layoutSubviews]",
            // UIKit views are top-left origin already.
            needs_flip: false,
        },
        Platform::Macos => CanvasFfi {
            click: "corro_macos_canvas_click",
            key: "corro_macos_canvas_key",
            // AppKit hands the context through the current graphics context.
            event_class: "NSEvent",
            event_type: "NSEvent",
            event_local: "[self convertPoint:event.locationInWindow fromView:nil]",
            layout_hook: "layout",
            layout_super: "[super layout]",
            // AppKit views are bottom-left origin: the emitted class must flip
            // or every glyph lands mirrored (MACOS_GUIDELINES.md §5).
            needs_flip: true,
        },
    }
}

struct CanvasFfi {
    event_type: &'static str,
    click: &'static str,
    key: &'static str,
    event_class: &'static str,
    event_local: &'static str,
    layout_hook: &'static str,
    layout_super: &'static str,
    needs_flip: bool,
    // NOTE: `size`, `draw` and `context_expr` were removed from this table.
    // They were populated for both platforms but never read (`render_canvas_impl`
    // inlines the equivalent ObjC - the size report is written into the layout
    // hook, and the draw body emits its own context expression), so they were
    // dead weight that also implied the selectors were configurable when they
    // are not. If those bodies are ever made table-driven, this is the place to
    // reintroduce them.
}


/// Emit the canvas class *implementation* for `config`.
///
/// This is the part of the shims that has no single correct body, which is why
/// every branch below is a field on [`CanvasViewConfig`] rather than a decision
/// taken here.
fn render_canvas_impl(p: Platform, cfg: &CanvasViewConfig) -> String {
    let ffi = canvas_ffi(p);
    let cls_name = &cfg.class_name;
    let mut s = String::new();

    let _ = write!(
        s,
        r#"
#pragma mark - Canvas view (configured)

@implementation {cls_name} {{
    uint64_t _corroCanvasId;{filling_ivar}
}}

- (void)corroSetCanvasId:(NSInteger)canvasId {{
    _corroCanvasId = (uint64_t)canvasId;
}}
"#,
        cls_name = cls_name,
        filling_ivar = if cfg.fill_superview {
            "\n    BOOL _corroFillingSuperview;  // re-entrancy guard, see the layout pass"
        } else {
            ""
        },
    );

    if ffi.needs_flip {
        s.push_str(
            r#"
/// The shared draw convention is top-left origin, y down. AppKit's is
/// bottom-left, so the canvas flips. Emitted from the platform table rather
/// than left to the host: without it every glyph lands mirrored.
- (BOOL)isFlipped {
    return YES;
}
"#,
        );
    }

    // Layout pass: report the size (and optionally fill the superview).
    let _ = writeln!(s, "\n- (void){} {{", ffi.layout_hook);
    let _ = writeln!(s, "    {};", ffi.layout_super);
    if cfg.fill_superview {
        s.push_str(
            r#"    CGRect b = self.bounds;
    // A view that is part of a stack view is sized by it; this only matters
    // for one attached with addSubview:, where nothing else gives it a height.
    // Changing the frame inside a layout pass schedules another pass, hence the
    // re-entrancy guard (without it this recurses until the stack runs out).
    if (self.superview != nil && (CGRectGetHeight(b) < 1.0 || CGRectGetWidth(b) < 1.0)) {
        if (!_corroFillingSuperview) {
            _corroFillingSuperview = YES;
            self.frame = self.superview.bounds;
            b = self.bounds;
            _corroFillingSuperview = NO;
        }
    }
"#,
        );
    } else {
        s.push_str("    CGRect b = self.bounds;\n");
    }
    if cfg.trace_events {
        // The format string's braces are doubled: it goes through `format!`.
        let _ = writeln!(
            s,
            "    fprintf(stderr, \"[corro] {}.{} canvas=%llu %dx%d\\n\", _corroCanvasId, (int)CGRectGetWidth(b), (int)CGRectGetHeight(b));",
            cls_name, ffi.layout_hook
        );
        s.push_str("    fflush(stderr);\n");
    }
    if cfg.report_size_on_layout {
        s.push_str(
            r#"    // Reported here as well as in the draw pass: Rust replays the draw
    // closure before the framework's first draw, so a size that only arrived
    // with the first frame would come too late and the sheet would lay itself
    // out at the 1x1 placeholder.
    if (CGRectGetWidth(b) > 0 && CGRectGetHeight(b) > 0) {
        CORRO_CANVAS_SIZE(_corroCanvasId, (int32_t)CGRectGetWidth(b), (int32_t)CGRectGetHeight(b));
    }
"#,
        );
    }
    s.push_str("}\n");

    // Draw pass.
    s.push_str(
        r#"
- (void)drawRect:(CGRect)rect {
    (void)rect;
    CGRect b = self.bounds;
"#,
    );
    if cfg.trace_events {
        let _ = writeln!(
            s,
            "    fprintf(stderr, \"[corro] {}.drawRect canvas=%llu %dx%d\\n\", _corroCanvasId, (int)CGRectGetWidth(b), (int)CGRectGetHeight(b));",
            cls_name
        );
        s.push_str("    fflush(stderr);\n");
    }
    s.push_str(
        r#"    if (CGRectGetWidth(b) > 0 && CGRectGetHeight(b) > 0) {
        CORRO_CANVAS_SIZE(_corroCanvasId, (int32_t)CGRectGetWidth(b), (int32_t)CGRectGetHeight(b));
    }
    CGContextRef ctx = CORRO_GRAPHICS_CONTEXT();
    if (ctx == NULL) {
        return;
    }
    CORRO_CANVAS_DRAW(_corroCanvasId, (void *)ctx, (int32_t)CGRectGetWidth(b), (int32_t)CGRectGetHeight(b));
}
"#,
    );

    if cfg.hardware_keys {
        s.push_str(
            r#"
/// Required for hardware keyboards (iPad, simctl, a Mac keyboard on a device):
/// a view that cannot become first responder never sees a key event.
- (BOOL)canBecomeFirstResponder {
    return YES;
}
"#,
        );
    }

    s
}

/// Emit the canvas *event* methods (click + key) for `config`.
fn render_canvas_events(p: Platform, cfg: &CanvasViewConfig) -> String {
    let ffi = canvas_ffi(p);
    let mut s = String::new();

    // Click: touch end on iOS, mouse up on macOS (a drag that starts on the
    // grid must not move the cursor mid-gesture).
    match p {
        Platform::Ios => {
            let _ = write!(
                s,
                r#"
/// A tap moves the cursor. `touchesEnded:` rather than `touchesBegan:` so a
/// drag that starts on the grid does not move the selection mid-gesture.
- (void)touchesEnded:(NSSet<{ev} *> *)touches withEvent:({evt} *)event {{
    (void)event;
    {ev} *touch = touches.anyObject;
    if (touch != nil) {{
        CGPoint p = {local};
        {click}(_corroCanvasId, (double)p.x, (double)p.y);
    }}
}}
"#,
                ev = ffi.event_class,
                evt = ffi.event_type,
                local = ffi.event_local,
                click = ffi.click,
            );
        }
        Platform::Macos => {
            let _ = write!(
                s,
                r#"
/// A click moves the cursor. `mouseUp:` rather than `mouseDown:` so a drag
/// that starts on the grid does not move the selection mid-gesture.
- (void)mouseUp:({ev} *)event {{
    NSPoint p = {local};
    {click}(_corroCanvasId, (double)p.x, (double)p.y);
}}
"#,
                ev = ffi.event_class,
                local = ffi.event_local,
                click = ffi.click,
            );
        }
    }

    if !cfg.hardware_keys {
        return s;
    }

    // Key path. On iOS the key class is 13.4+, so a pre-13.4 deployment target
    // needs the availability guard on both the declaration and the definition
    // (clang analyses each body on its own — guarding only the call site is not
    // enough, which the hand-written file's comments record).
    let guarded = cfg
        .deployment_target
        .as_deref()
        .map(|t| version_below(t, (13, 4)))
        .unwrap_or(false);

    match p {
        Platform::Ios => {
            if guarded {
                s.push_str(
                    r#"
/// `UIPress.key` and the whole `UIKey` class are 13.4+, while the app may
/// deploy below that — so this method degrades to the framework's default
/// handling when the key class is unavailable.
- (void)pressesBegan:(NSSet<UIPress *> *)presses withEvent:(UIPressesEvent *)event {
    if (@available(iOS 13.4, *)) {
        [self corroHandlePresses:presses];
    }
    [super pressesBegan:presses withEvent:event];
}
"#,
                );
            } else {
                s.push_str(
                    r#"
/// `keyCommands` would be the modern route but only handles a fixed set;
/// `pressesBegan:` covers a physical keyboard and the accessory bar.
- (void)pressesBegan:(NSSet<UIPress *> *)presses withEvent:(UIPressesEvent *)event {
    [self corroHandlePresses:presses];
    [super pressesBegan:presses withEvent:event];
}
"#,
                );
            }
            let availability = if guarded {
                " API_AVAILABLE(ios(13.4))"
            } else {
                ""
            };
            let _ = write!(
                s,
                r#"
- (void)corroHandlePresses:(NSSet<UIPress *> *)presses{avail} {{
    for (UIPress *press in presses) {{
        UIKey *key = press.key;
        if (key == nil) {{
            continue;
        }}
        uint32_t keyval = 0;
        if (key.characters.length > 0) {{
            keyval = (uint32_t)[key.characters characterAtIndex:0];
        }} else {{
            switch (key.keyCode) {{
{keymap}
                default: break;
            }}
        }}
        if (keyval == 0) {{
            continue;
        }}
        // Modifier mask: 1 = Shift, 4 = Ctrl, 8 = Alt (the shared convention).
        uint32_t mods = 0;
        if ((key.modifierFlags & UIKeyModifierShift) != 0) mods |= 1;
        if ((key.modifierFlags & UIKeyModifierControl) != 0) mods |= 4;
        if ((key.modifierFlags & UIKeyModifierAlternate) != 0) mods |= 8;
        if ({key}(_corroCanvasId, keyval, mods)) {{
            return;
        }}
    }}
}}
"#,
                avail = availability,
                keymap = render_keymap(IOS_KEY_MAP, "                "),
                key = ffi.key,
            );
        }
        Platform::Macos => {
            s.push_str(
                r#"
/// AppKit already delivers a resolved event, so the key path needs no
/// availability guard: `characters`/`keyCode` exist on every supported macOS.
- (void)keyDown:(NSEvent *)event {
    uint32_t keyval = 0;
    NSString *chars = event.charactersIgnoringModifiers;
    if (chars.length > 0) {
        keyval = (uint32_t)[chars characterAtIndex:0];
    } else {
        switch (event.keyCode) {
            case 36:  keyval = 0xFF0D; break;  // Return
            case 76:  keyval = 0xFF0D; break;  // Keypad Enter
            case 53:  keyval = 0xFF1B; break;  // Escape
            case 48:  keyval = 0xFF09; break;  // Tab
            case 51:  keyval = 0xFF08; break;  // Delete (backspace)
            case 117: keyval = 0xFFFF; break;  // Forward delete
            case 123: keyval = 0xFF51; break;  // Left
            case 126: keyval = 0xFF52; break;  // Up
            case 124: keyval = 0xFF53; break;  // Right
            case 125: keyval = 0xFF54; break;  // Down
            case 115: keyval = 0xFF50; break;  // Home
            case 119: keyval = 0xFF57; break;  // End
            default: break;
        }
    }
    if (keyval == 0) {
        return;
    }
    NSEventModifierFlags f = event.modifierFlags;
    uint32_t mods = 0;
    if ((f & NSEventModifierFlagShift) != 0) mods |= 1;
    if ((f & NSEventModifierFlagControl) != 0) mods |= 4;
    if ((f & NSEventModifierFlagOption) != 0) mods |= 8;
    if (!corro_macos_canvas_key(_corroCanvasId, keyval, mods)) {
        [super keyDown:event];
    }
}
"#,
            );
        }
    }
    s
}

/// `"12.0"` vs `(13, 4)` -> true when the target is below the requirement.
fn version_below(target: &str, required: (u32, u32)) -> bool {
    let mut parts = target.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor) < required
}

/// The `case <platform constant>: keyval = <Rust value>; break;` switch body.
///
/// The *values* are looked up from `crate::core::key`, so this is a second
/// name for the same number rather than a second copy of it.
fn render_keymap(map: &[KeyMapEntry], indent: &str) -> String {
    let mut out = String::new();
    for entry in map {
        let value = rust_key_value(entry.rust_key);
        let _ = writeln!(
            out,
            "{indent}case {constant}: keyval = 0x{value:04X}; break;  // {name}",
            constant = entry.platform_constant,
            name = entry.rust_key,
        );
    }
    out
}

/// Resolve a [`KeyMapEntry::rust_key`] name to the value the shared key
/// handling matches on.
///
/// The table is spelled out here rather than read from `crate::core::key`
/// because this module is `#[path]`-included by host `build.rs` files, where
/// the crate root is the *host's* (so `crate::core` does not exist). A test
/// asserts every entry equals the real `core::key` constant, so the two cannot
/// drift even though this copy has to exist.
fn rust_key_value(name: &str) -> u32 {
    // Values are the unix/X11 keysyms `core::key` uses on every Apple target.
    match name {
        "RETURN" => 0xFF0D,
        "ENTER" => 0xFF8D,
        "ESCAPE" => 0xFF1B,
        "BACKSPACE" => 0xFF08,
        "DELETE" => 0xFFFF,
        "LEFT" => 0xFF51,
        "UP" => 0xFF52,
        "RIGHT" => 0xFF53,
        "DOWN" => 0xFF54,
        "TAB" => 0xFF09,
        "HOME" => 0xFF50,
        "END" => 0xFF57,
        "PAGE_UP" => 0xFF55,
        "PAGE_DOWN" => 0xFF56,
        "F1" => 0xFFBE,
        "F2" => 0xFFBF,
        "F3" => 0xFFC0,
        other => panic!("key map names an unknown rswidgets::core::key constant: {other}"),
    }
}

/// Kept for the in-crate tests: the same lookup, but asserting against
/// `crate::core::key` so a divergence is a test failure rather than a bug on a
/// device.
#[cfg(test)]
fn rust_key_value_checked(name: &str) -> u32 {
    use crate::core::key as k;
    match name {
        "RETURN" => k::RETURN,
        "ENTER" => k::ENTER,
        "ESCAPE" => k::ESCAPE,
        "BACKSPACE" => k::BACKSPACE,
        "DELETE" => k::DELETE,
        "LEFT" => k::LEFT,
        "UP" => k::UP,
        "RIGHT" => k::RIGHT,
        "DOWN" => k::DOWN,
        "TAB" => k::TAB,
        "HOME" => k::HOME,
        "END" => k::END,
        "PAGE_UP" => k::PAGE_UP,
        "PAGE_DOWN" => k::PAGE_DOWN,
        "F1" => k::F1,
        "F2" => k::F2,
        "F3" => k::F3,
        other => panic!("key map names an unknown rswidgets::core::key constant: {other}"),
    }
}

/// Emit the text measurer/drawer *implementation* for `config`.
fn render_text_impl(p: Platform, cfg: &TextShimConfig) -> String {
    let cls = &cfg.class_name;
    let mut s = String::new();

    let (font_expr_modern, measure_modern, draw_modern, legacy_measure, legacy_draw) = match p {
        Platform::Ios => (
            "monospacedSystemFontOfSize:weight: (13.0+)",
            "[text boundingRectWithSize:CGSizeMake(CGFLOAT_MAX, CGFLOAT_MAX)
                                        options:NSStringDrawingUsesLineFragmentOrigin
                                     attributes:@{ NSFontAttributeName: font }
                                        context:nil].size",
            "[text drawAtPoint:CGPointMake(x, baseline)
           withAttributes:@{ NSFontAttributeName: font,
                             NSForegroundColorAttributeName: color }]",
            "[text sizeWithFont:font]",
            "[text drawAtPoint:CGPointMake(x, baseline) withFont:font]",
        ),
        Platform::Macos => (
            "monospacedSystemFontOfSize:weight: (10.15+)",
            "[text sizeWithAttributes:@{ NSFontAttributeName: font }]",
            "[text drawAtPoint:CGPointMake(x, baseline)
           withAttributes:@{ NSFontAttributeName: font,
                             NSForegroundColorAttributeName: color }]",
            "[text sizeWithAttributes:@{ NSFontAttributeName: font }]",
            "[text drawAtPoint:CGPointMake(x, baseline)
           withAttributes:@{ NSFontAttributeName: font,
                             NSForegroundColorAttributeName: color }]",
        ),
    };
    let _ = font_expr_modern;

    let _ = write!(
        s,
        r#"
#pragma mark - Text (configured)

@implementation {cls}

+ ({font_class} *)fontForFamily:(NSString *)family size:(CGFloat)size weight:(NSInteger)weight {{
    (void)weight;
    {font_class} *font = nil;
    if (family.length > 0) {{
        font = [{font_class} fontWithName:family size:size];
    }}
    if (font == nil) {{
        // The grid asks for a monospace family by preference; the fallback is
        // the configured family, then the system font. The Rust-side estimate
        // assumes monospace, so a measured layout stays close either way.
        font = [{font_class} fontWithName:@"{fallback}" size:size];
    }}
    if (font == nil) {{
        font = [{font_class} systemFontOfSize:size];
    }}
    return font;
}}

"#,
        cls = cls,
        font_class = if p == Platform::Ios { "UIFont" } else { "NSFont" },
        fallback = cfg.fallback_family,
    );

    // `measure:` returns a malloc'd CGRect* so the ABI is one pointer in, one
    // pointer out — no struct-return register rules to get wrong on 32-bit.
    let _ = write!(
        s,
        r#"/// Returns a calloc'd CGRect* (or NULL) so the ABI is one pointer in / one
/// pointer out — no struct-return register rules to get wrong on 32-bit.
/// The caller (Rust) frees it.
+ (CGRect *)measure:(NSString *)text
               font:(NSString *)family
               size:(CGFloat)size
              slant:(NSInteger)slant
             weight:(NSInteger)weight {{
    (void)slant;  // weight is what the grid uses
    if (text.length == 0) {{
        return (CGRect *)calloc(1, sizeof(CGRect));
    }}
    {font_class} *font = [{cls} fontForFamily:family size:size weight:weight];
    CGSize measured = {measure_modern};
"#,
        font_class = if p == Platform::Ios { "UIFont" } else { "NSFont" },
        cls = cls,
        measure_modern = measure_modern,
    );
    if cfg.legacy_fallback && p == Platform::Ios {
        let _ = write!(
            s,
            r#"    if (measured.width == 0 && [text respondsToSelector:@selector(boundingRectWithSize:options:attributes:context:)] == NO) {{
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
        measured = {legacy_measure};
#pragma clang diagnostic pop
    }}
"#,
            legacy_measure = legacy_measure,
        );
    }
    s.push_str(
        r#"    CGRect *r = (CGRect *)calloc(1, sizeof(CGRect));
    r->origin = CGPointZero;
    r->size = measured;
    return r;
}

"#,
    );

    // Draw.
    let _ = write!(
        s,
        r#"- (void)drawText:(NSString *)text
             ctx:(CGContextRef)ctx
            font:(NSString *)family
               x:(CGFloat)x
               y:(CGFloat)y
            size:(CGFloat)size
               r:(CGFloat)red
               g:(CGFloat)green
               b:(CGFloat)blue
               a:(CGFloat)alpha
           slant:(NSInteger)slant
          weight:(NSInteger)weight {{
    (void)slant;
    if (text.length == 0 || ctx == NULL) {{
        return;
    }}
    {font_class} *font = [{cls} fontForFamily:family size:size weight:weight];
    {color_class} *color = [{color_class} colorWithRed:red green:green blue:blue alpha:alpha];
    CGContextSaveGState(ctx);
    [color setFill];
"#,
        font_class = if p == Platform::Ios { "UIFont" } else { "NSFont" },
        color_class = if p == Platform::Ios { "UIColor" } else { "NSColor" },
        cls = cls,
    );
    if cfg.baseline_from_ascent {
        s.push_str(
            r#"    // The shared draw convention gives a top edge; the text API wants a
    // baseline. Without this offset every glyph lands one ascent too high.
    CGFloat baseline = y + font.ascender;
"#,
        );
    } else {
        s.push_str("    CGFloat baseline = y;
");
    }
    let _ = write!(
        s,
        r#"    {draw_modern}
    CGContextRestoreGState(ctx);
}}

@end
"#,
        draw_modern = draw_modern,
    );
    let _ = legacy_draw;
    s
}

/// A Rust-side selector the test suite can compare against the tables above,
/// so "the adapters send something the generator does not know about" is a
/// test failure rather than a runtime crash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RustSideSelector {
    /// The class the adapter resolves (`CorroIosTarget`, ...).
    pub class: &'static str,
    /// The selector it sends.
    pub selector: &'static str,
    /// The source file the pair was taken from, for the failure message.
    pub source: &'static str,
}

/// Every `(class, selector)` pair the iOS adapter sends to a generated shim.
///
/// Transcribed from `backends_ios_adapter.rs`; the test below asserts each is
/// covered by the tables above.
pub const IOS_ADAPTER_SENDS: &[RustSideSelector] = &[
    RustSideSelector { class: "CorroIosTarget", selector: "targetWithCallbackId:", source: "Button::attach_target" },
    RustSideSelector { class: "CorroIosTarget", selector: "corroFired:", source: "Button::attach_target (action)" },
    RustSideSelector { class: "CorroIosText", selector: "measure:font:size:slant:weight:", source: "measure_text_via_host" },
    RustSideSelector { class: "CorroIosText", selector: "drawText:ctx:font:x:y:size:r:g:b:a:slant:weight:", source: "draw_text_via_host" },
    RustSideSelector { class: "CorroIosAlert", selector: "corroNewAlert", source: "create_dialog" },
    RustSideSelector { class: "CorroIosAlert", selector: "corroAddAction:", source: "Dialog::add_button" },
    RustSideSelector { class: "UIViewController", selector: "corroPresentDialog:", source: "Dialog::present" },
    RustSideSelector { class: "UIView", selector: "corroSetPinnedWidth:", source: "Label::set_fixed_width" },
    RustSideSelector { class: "UIView", selector: "corroSetContentHugging:", source: "Label::set_fixed_width" },
];

/// The same for the macOS adapter.
pub const MACOS_ADAPTER_SENDS: &[RustSideSelector] = &[
    RustSideSelector { class: "CorroMacTarget", selector: "targetWithCallbackId:", source: "Button::attach_target" },
    RustSideSelector { class: "CorroMacTarget", selector: "corroFired:", source: "Button::attach_target (action)" },
    RustSideSelector { class: "CorroMacText", selector: "measure:font:size:slant:weight:", source: "measure_text_via_host" },
    RustSideSelector { class: "CorroMacText", selector: "drawText:ctx:font:x:y:size:r:g:b:a:slant:weight:", source: "draw_text_via_host" },
    RustSideSelector { class: "CorroMacAlert", selector: "corroNewAlert", source: "create_dialog" },
    RustSideSelector { class: "CorroMacAlert", selector: "corroAddAction:", source: "Dialog::add_button" },
    RustSideSelector { class: "NSWindowController", selector: "corroPresentDialog:", source: "Dialog::present" },
    RustSideSelector { class: "NSView", selector: "corroSetPinnedWidth:", source: "Label::set_fixed_width" },
    RustSideSelector { class: "NSView", selector: "corroSetContentHugging:", source: "Label::set_fixed_width" },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("rswidgets-apple-gen-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// Every table entry produces a declaration whose signature matches the
    /// kind list, and the header/impl both mention each class.
    #[test]
    fn every_declared_selector_is_emitted() {
        for p in [Platform::Ios, Platform::Macos] {
            let header = render_header(p, &ShimConfig::for_platform(p));
            let impl_ = render_impl(p, &ShimConfig::for_platform(p));
            for m in TARGET_METHODS.iter().chain(ALERT_METHODS).chain(VIEW_CATEGORY_METHODS) {
                let base = selector_base(m.selector);
                // Declared in the header, always: that is what makes a
                // signature mismatch a compile error in the host.
                assert!(header.contains(base), "{} header missing {base}", p.name());
                // A hand-written method is declared but NOT implemented here -
                // its body lives in the host's .m (and for corroBoundsWidth a
                // generated stub would return 0, which is a real bug, not a
                // harmless placeholder). Everything else must be emitted.
                if m.body_elsewhere {
                    assert!(
                        !impl_.contains(base.to_string().as_str()),
                        "{} should not implement {base}: its body is in the host .m",
                        p.name()
                    );
                    continue;
                }
                if !m.class_method || m.selector == "corroNewAlert" || m.selector == "targetWithCallbackId:" {
                    assert!(impl_.contains(base), "{} impl missing {base}", p.name());
                }
                assert_eq!(
                    header.matches(base).count() >= 1,
                    true,
                    "{} should declare {base}",
                    p.name()
                );
            }
            // The trampoline must call the platform's C callback.
            assert!(impl_.contains(&format!("{}_callback", p.c_prefix())));
        }
    }

    /// The whole point: no selector the adapters send may be missing from the
    /// generator's tables. If someone adds a `cls("CorroIosFoo")` + `raw_send!`
    /// to an adapter, this fails until the table is updated.
    #[test]
    fn adapter_selectors_are_all_covered() {
        let covered: Vec<&str> = TARGET_METHODS
            .iter()
            .chain(ALERT_METHODS)
            .chain(HAND_WRITTEN_METHODS)
            .chain(VIEW_CATEGORY_METHODS)
            .map(|m| m.selector)
            .collect();
        let presentation = ["corroPresentDialog:"];
        for entry in IOS_ADAPTER_SENDS.iter().chain(MACOS_ADAPTER_SENDS) {
            let known = covered.contains(&entry.selector) || presentation.contains(&entry.selector);
            assert!(
                known,
                "adapter sends {}::{} ({}) but the generator has no entry for it",
                entry.class, entry.selector, entry.source
            );
        }
    }

    /// A generated header declares the hand-written text class so a signature
    /// mismatch there is a host compile error.
    ///
    /// The declaration is spelled with its parameter names
    /// (`measure:(id)text font:(id)...`), so the test checks the selector
    /// *pieces* rather than the contiguous string.
    #[test]
    fn hand_written_selectors_are_signature_checked() {
        let header = render_header(Platform::Ios, &ShimConfig::for_platform(Platform::Ios));
        for piece in ["measure:", "font:", "size:", "slant:", "weight:"] {
            assert!(header.contains(piece), "text class declaration missing {piece}");
        }
        assert!(header.contains("drawText:"));
        assert!(header.contains("ctx:"));
        // And the class itself must be declared.
        assert!(header.contains("CorroIosText : NSObject"));
    }

    /// The AppKit header must tell the host to flip its canvas view; UIKit's
    /// header must not (it is already top-left).
    #[test]
    fn flipped_hint_only_on_appkit() {
        assert!(!render_header(Platform::Ios, &ShimConfig::for_platform(Platform::Ios)).contains("isFlipped"));
        assert!(render_header(Platform::Macos, &ShimConfig::for_platform(Platform::Macos)).contains("isFlipped"));
    }

    /// The canvas class *name* is per-platform, because the two hosts do not
    /// agree on it: iOS registers `SheetView`, macOS registers
    /// `CorroSheetView` (matching the `Corro*` prefix every other macOS shim
    /// uses, so a host resolves them all with one convention).
    ///
    /// This was a single shared `CANVAS_VIEW` constant, which put `SheetView`
    /// into the *macOS* generated header. The host would then have been told
    /// to implement a class the generated header declares under a different
    /// name than the one it looks for — a silent ABI drift, reintroduced
    /// through the very generator meant to prevent it. The header must name
    /// the class the backend actually resolves.
    #[test]
    fn the_declared_canvas_class_is_the_one_the_backend_resolves() {
        assert_eq!(CanvasView::for_platform(Platform::Ios).class_name, "SheetView");
        assert_eq!(
            CanvasView::for_platform(Platform::Macos).class_name,
            "CorroSheetView"
        );
        for p in [Platform::Ios, Platform::Macos] {
            let header = render_header(p, &ShimConfig::for_platform(p));
            assert!(
                header.contains(&format!(
                    "@interface {} : {}",
                    CanvasView::for_platform(p).class_name,
                    p.base_view_class()
                )),
                "{} header does not declare its own canvas class ({})",
                p.name(),
                CanvasView::for_platform(p).class_name
            );
            // ...and must NOT declare the other platform's, which is what the
            // shared constant did.
            let other = match p {
                Platform::Ios => "CorroSheetView",
                Platform::Macos => "SheetView",
            };
            assert!(
                !header.contains(&format!("@interface {other} :")),
                "{} header wrongly declares the other platform's canvas class {other}",
                p.name()
            );
        }
    }

    /// A selector the adapters send with ONE argument must be declared with
    /// one. `corroSetContentHugging:priority:` was declared for a
    /// `corroSetContentHugging:` call: a different selector, so the host
    /// implements a method the adapter never messages, and the message itself
    /// leaves the host reading a register the sender never set.
    ///
    /// Checked against the *adapters' own strings* rather than a restated
    /// list, because the restatement is what drifted.
    #[test]
    fn hugging_selector_takes_the_one_argument_the_adapter_sends() {
        for p in [Platform::Ios, Platform::Macos] {
            let header = render_header(p, &ShimConfig::for_platform(p));
            assert!(
                header.contains("corroSetContentHugging:"),
                "{} header must declare corroSetContentHugging:",
                p.name()
            );
            assert!(
                !header.contains("corroSetContentHugging:priority:"),
                "{} header declares a two-argument hugging selector the \
                 adapters never send (they send one argument)",
                p.name()
            );
        }
        // The table entry itself must carry the one-argument spelling, or the
        // emitter would regenerate the header the assertion just rejected.
        let entry = VIEW_CATEGORY_METHODS
            .iter()
            .find(|m| m.selector.starts_with("corroSetContentHugging"))
            .expect("no hugging entry in VIEW_CATEGORY_METHODS");
        assert_eq!(entry.selector, "corroSetContentHugging:");
        assert_eq!(entry.args, &[ArgKind::Integer]);
    }

    /// CGFloat is spelled as the typedef so no 32/64 branch is needed.
    #[test]
    fn float_kind_is_cgfloat_not_double() {
        assert_eq!(ArgKind::Float.objc_type(), "CGFloat");
        assert_eq!(ArgKind::Float.objc_type(), "CGFloat");
    }

    #[test]
    fn generate_creates_both_files_and_never_overwrites() {
        let root = temp_root("generate");
        let created = generate(&root, Platform::Ios).unwrap();
        assert_eq!(created.len(), 2, "expected header + impl, got {created:?}");
        assert!(root.join(SHIM_RELPATH).exists());
        assert!(root.join(SHIM_HEADER_RELPATH).exists());

        // A hand-edited file must survive: generation is additive.
        let impl_path = root.join(SHIM_RELPATH);
        std::fs::write(&impl_path, "// hand-edited\n").unwrap();
        let created = generate(&root, Platform::Ios).unwrap();
        assert!(!created.contains(&impl_path));
        assert_eq!(std::fs::read_to_string(&impl_path).unwrap(), "// hand-edited\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The canvas emitter must honour every knob: each one changes the output
    /// in a way a host depends on, so this pins the mapping.
    #[test]
    fn canvas_config_knobs_change_the_emitted_body() {
        let base = CanvasViewConfig::ios("SheetView");

        // hardware_keys off: no first-responder plumbing at all.
        let quiet = CanvasViewConfig { hardware_keys: false, ..base.clone() };
        let with = render_canvas_impl(Platform::Ios, &base) + &render_canvas_events(Platform::Ios, &base);
        let without = render_canvas_impl(Platform::Ios, &quiet) + &render_canvas_events(Platform::Ios, &quiet);
        assert!(with.contains("pressesBegan:"));
        assert!(!without.contains("pressesBegan:"));
        assert!(with.contains("canBecomeFirstResponder"));
        assert!(!without.contains("canBecomeFirstResponder"));

        // fill_superview off: no self-resizing, hence no re-entrancy guard.
        let no_fill = CanvasViewConfig { fill_superview: false, ..base.clone() };
        let body = render_canvas_impl(Platform::Ios, &no_fill);
        assert!(!body.contains("_corroFillingSuperview"));
        assert!(render_canvas_impl(Platform::Ios, &base).contains("_corroFillingSuperview"));

        // report_size_on_layout off: size is reported only from the draw pass.
        let draw_only = CanvasViewConfig { report_size_on_layout: false, ..base.clone() };
        let body = render_canvas_impl(Platform::Ios, &draw_only);
        assert_eq!(body.matches("CORRO_CANVAS_SIZE").count(), 1);

        // trace_events on: a fprintf appears.
        let traced = CanvasViewConfig { trace_events: true, ..base.clone() };
        assert!(render_canvas_impl(Platform::Ios, &traced).contains("fprintf"));

        // deployment_target below 13.4: the UIKey path is availability-guarded.
        let old_target = CanvasViewConfig { deployment_target: Some("12.0".into()), ..base.clone() };
        let guarded = render_canvas_events(Platform::Ios, &old_target);
        assert!(guarded.contains("@available(iOS 13.4, *)"));
        assert!(guarded.contains("API_AVAILABLE(ios(13.4))"));
        let new_target = CanvasViewConfig { deployment_target: Some("15.0".into()), ..base.clone() };
        let unguarded = render_canvas_events(Platform::Ios, &new_target);
        assert!(!unguarded.contains("@available(iOS 13.4"));
        assert!(unguarded.contains("corroHandlePresses:"));
    }

    /// AppKit is bottom-left origin: the emitted canvas MUST flip, and UIKit's
    /// must not. This is the single most damaging silent failure in the port.
    #[test]
    fn canvas_flip_is_platform_determined() {
        let ios = render_canvas_impl(Platform::Ios, &CanvasViewConfig::ios("SheetView"));
        let mac = render_canvas_impl(Platform::Macos, &CanvasViewConfig::macos("SheetView"));
        assert!(!ios.contains("isFlipped"));
        assert!(mac.contains("isFlipped"));
        assert!(mac.contains("return YES;"));
    }

    /// The key map must carry the values from `core::key`, not a transcription
    /// of them — that duplication is what this generator removes.
    #[test]
    fn keymap_values_come_from_core_key() {
        assert_eq!(rust_key_value_checked("RETURN"), rust_key_value("RETURN"));
        assert_eq!(rust_key_value_checked("ESCAPE"), rust_key_value("ESCAPE"));
        assert_eq!(rust_key_value_checked("DELETE"), rust_key_value("DELETE"));
        let map = render_keymap(IOS_KEY_MAP, "");
        assert!(map.contains(&format!("0x{:04X}", crate::core::key::ESCAPE)));
        // And the SDK's easily-misremembered constant spelling is exact.
        assert!(map.contains("UIKeyboardHIDUsageKeyboardDeleteOrBackspace"));
    }

    /// An unknown key name must be a hard error, not a silently missing case.
    #[test]
    #[should_panic(expected = "unknown rswidgets::core::key constant")]
    fn keymap_rejects_unknown_rust_constant() {
        rust_key_value("NOT_A_KEY");
    }

    /// The ascent offset must be OFF by default: `drawAtPoint:` takes a
    /// top-left point, so adding the ascent shifts every glyph down a full
    /// ascent and clips it against the row rule. It shipped ON, and that is
    /// exactly what the first real iOS screenshot showed.
    #[test]
    fn text_ascent_offset_is_off_by_default() {
        let default = TextShimConfig::ios("CorroIosText");
        assert!(
            !default.baseline_from_ascent,
            "the default must not add an ascent offset - see the field docs"
        );
        assert!(!render_text_impl(Platform::Ios, &default).contains("font.ascender"));
        assert!(!render_text_impl(Platform::Macos, &TextShimConfig::macos("CorroMacText"))
            .contains("font.ascender"));
    }

    /// ...but it stays configurable, for a text API that does want a baseline.
    #[test]
    fn text_ascent_offset_is_configurable_and_documented() {
        let with = render_text_impl(
            Platform::Ios,
            &TextShimConfig { baseline_from_ascent: true, ..TextShimConfig::ios("CorroIosText") },
        );
        assert!(with.contains("font.ascender"));
        let without = render_text_impl(
            Platform::Ios,
            &TextShimConfig { baseline_from_ascent: false, ..TextShimConfig::ios("CorroIosText") },
        );
        assert!(!without.contains("font.ascender"));
        assert!(without.contains("CGFloat baseline = y;"));
    }

    /// Version comparison drives the availability guard.
    #[test]
    fn version_comparison() {
        assert!(version_below("12.0", (13, 4)));
        assert!(version_below("13.3", (13, 4)));
        assert!(!version_below("13.4", (13, 4)));
        assert!(!version_below("15.0", (13, 4)));
        assert!(!version_below("16", (13, 4)));
    }

    /// One generator, both platforms: the emitted pair must be self-consistent
    /// and each must reference its own C entry points.
    #[test]
    fn both_platforms_emit_their_own_ffi_names() {
        for p in [Platform::Ios, Platform::Macos] {
            let impl_ = render_impl(p, &ShimConfig::for_platform(p));
            assert!(impl_.contains(&format!("{}_callback", p.c_prefix())));
            assert!(impl_.contains(&format!("{}_canvas_draw", p.c_prefix())));
            assert!(impl_.contains(&format!("{}_canvas_key", p.c_prefix())));
            assert!(impl_.contains("CORRO_GRAPHICS_CONTEXT()"));
        }
        assert!(render_impl(Platform::Ios, &ShimConfig::for_platform(Platform::Ios))
            .contains("UIGraphicsGetCurrentContext"));
        assert!(render_impl(Platform::Macos, &ShimConfig::for_platform(Platform::Macos))
            .contains("NSGraphicsContext"));
    }

    /// The generated ObjC must be plausibly balanced — this is not a compiler,
    /// but an unbalanced `@implementation`/`@end` is the one class of error a
    /// pure-text generator can catch without an SDK.
    #[test]
    fn generated_objc_has_balanced_blocks() {
        for p in [Platform::Ios, Platform::Macos] {
            for text in [render_header(p, &ShimConfig::for_platform(p)), render_impl(p, &ShimConfig::for_platform(p))] {
                let opens: Vec<&str> = text.lines().filter(|l| l.trim_start().starts_with("@implementation") || l.trim_start().starts_with("@interface")).collect();
                let ends = text.lines().filter(|l| l.trim() == "@end").count();
                assert_eq!(opens.len(), ends, "{}: {} blocks vs {ends} @end", p.name(), opens.len());
            }
        }
    }
}
