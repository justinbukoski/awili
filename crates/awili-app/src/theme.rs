//! Design tokens for the Awili composer shell, as GPUI colors and a few metrics.
//!
//! Every color the views paint comes from here — there are NO inline color
//! literals in the view code (the "SCRIM lesson" from Hub: once a raw `rgb(..)`
//! leaks into a view it metastasises and theming becomes a grep-and-pray). The
//! verification step greps the views for `rgb(`/`rgba(`/`hsla(`/`Rgba {` and must
//! find nothing.
//!
//! A calm dark instrument panel: flat charcoal canvas, warm-bone text, one sage
//! accent for the caret and the user's own messages.

#![allow(dead_code)]

#[cfg(not(test))]
use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{px, Pixels, Rgba};

// ---- interface zoom (A4) ---------------------------------------------------
//
// The whole UI is laid out in absolute `px`, so the one lever that scales
// everything uniformly is the pixel constructor itself. [`z`] multiplies its
// argument by a process-global scale; every dimension in the view layer is
// built through it, so bumping the scale grows the sidebar, chip row, composer,
// rails, Settings and all pane content together — not just the font. At the
// default scale (1.0) `z(v)` is exactly `px(v)`, so nothing changes until the
// user zooms.

/// The live UI scale, as raw `f32` bits (default 1.0). Set once on launch from
/// the persisted value, then by the View ▸ Zoom actions.
///
/// Under `cfg(test)` the scale is THREAD-LOCAL, not process-global: the Rust
/// test harness runs tests concurrently on separate threads, so a test that
/// exercises a non-1.0 zoom (the palette double-scaling regression test) must
/// not be transiently visible to unrelated tests that read `z` mid-flight
/// (Codex round-2 finding 4). Production keeps the plain atomic — the app has
/// one window and one zoom, and the field is read on every paint.
#[cfg(not(test))]
static UI_SCALE_BITS: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0f32
#[cfg(test)]
thread_local! {
    static UI_SCALE_BITS: std::cell::Cell<u32> = const { std::cell::Cell::new(0x3f80_0000) }; // 1.0f32
}

/// The current interface zoom factor (1.0 = actual size).
pub fn ui_scale() -> f32 {
    #[cfg(not(test))]
    {
        f32::from_bits(UI_SCALE_BITS.load(Ordering::Relaxed))
    }
    #[cfg(test)]
    {
        UI_SCALE_BITS.with(|c| f32::from_bits(c.get()))
    }
}

/// Set the live interface zoom factor. Callers persist and trigger a re-render
/// separately; this only moves the constant the view layer reads.
pub fn set_ui_scale(scale: f32) {
    #[cfg(not(test))]
    UI_SCALE_BITS.store(scale.to_bits(), Ordering::Relaxed);
    #[cfg(test)]
    UI_SCALE_BITS.with(|c| c.set(scale.to_bits()));
}

/// A scaled pixel value: `px(v * ui_scale())`. The view layer builds every
/// dimension through this so interface zoom scales the whole layout, not just
/// text. Equivalent to `px(v)` at the default scale.
pub fn z(v: f32) -> Pixels {
    px(v * ui_scale())
}

/// `const`-evaluable opaque hex → `Rgba` (gpui's own `rgb()` isn't `const fn`).
const fn rgb(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// `const`-evaluable white at a given alpha — for hairlines / low-alpha fills.
const fn white_a(a: f32) -> Rgba {
    Rgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a,
    }
}

/// `const`-evaluable black at a given alpha — overlay fills that read correctly
/// on any bubble color (charcoal assistant, blue user).
const fn black_a(a: f32) -> Rgba {
    Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a,
    }
}

/// Preserve a design token's hue while using it as a translucent overlay.
const fn color_a(color: Rgba, a: f32) -> Rgba {
    Rgba {
        r: color.r,
        g: color.g,
        b: color.b,
        a,
    }
}

// ---- transcript measure -----------------------------------------------------
//
// The centered reading column and the content width inside it. These are a
// TOKEN, not a magic number, because the content width must also clamp the
// bubbles: a bubble with no definite max width reports its MAX-CONTENT size
// during intrinsic sizing, and a paragraph's max-content height is one line —
// which ancestors then commit as the row height (2026-08-24 overlap bug).

pub const TRANSCRIPT_MEASURE: f32 = 760.0;
pub const TRANSCRIPT_CONTENT: f32 = TRANSCRIPT_MEASURE - 28.0 * 2.0;

/// Prose reading column width (O1): ~72-75 characters of 15 px body text.
/// Only prose top-level blocks (paragraph, heading, list, quote) are clamped
/// to this; code blocks and rules keep the full bubble width, because source
/// lines are read across, not down.
pub const READING_MEASURE: f32 = READING_MAX;

// ---- spacing scale (critique: rhythm) ---------------------------------------
//
// One ladder for gaps and padding, adopted by new components and by touched
// code as it changes (no big-bang rewrite): 4 · 6 · 8 · 12 · 16 · 24. Use
// through [`z`] like every dimension: `z(SPACE_M)`.

pub const SPACE_XS: f32 = 4.0;
pub const SPACE_S: f32 = 6.0;
pub const SPACE_M: f32 = 8.0;
pub const SPACE_L: f32 = 12.0;
pub const SPACE_XL: f32 = 16.0;
pub const SPACE_XXL: f32 = 24.0;

/// Scroll edge comparisons allow half a physical pixel for rounding.
pub const SCROLL_EDGE_TOLERANCE_PHYSICAL_PX: f32 = 0.5;

// ---- Awili brand palette ----------------------------------------------------
//
// The canonical five, verbatim from `assets/brand/README.md` (brand pack v1.0).
// The tokens below are the app's own names for these; when a token IS a brand
// color it references the constant here rather than repeating the hex, so the
// brand stays greppable and there is exactly one place to change it.
//
// Brand rule, quoted from the pack: the blue and the sage "represent the two
// collaborating voices" and must not be recolored independently of each other.

/// Awili blue — the primary brand color.
pub const BRAND_BLUE: Rgba = rgb(0x2563eb);
/// Observatory sage — the second collaborating voice.
pub const BRAND_SAGE: Rgba = rgb(0x6dba8e);
/// Deep navy — the dark brand surface (logo lockups, marketing chrome).
pub const BRAND_NAVY: Rgba = rgb(0x071a2e);
/// Observatory bone — brand type color.
pub const BRAND_BONE: Rgba = rgb(0xe6e6e6);
/// Observatory charcoal — brand ground.
pub const BRAND_CHARCOAL: Rgba = rgb(0x141414);

// ---- surfaces (flat charcoal) ---------------------------------------------

/// Window / page / message-list canvas.
pub const BG: Rgba = rgb(0x1a1a1a);
/// Composer chrome bar at the bottom of the window — Observatory charcoal.
pub const CHROME: Rgba = BRAND_CHARCOAL;
/// Inputs / the composer field fill.
pub const RAISED: Rgba = rgb(0x222222);
/// Assistant message bubble.
pub const BUBBLE_ASSISTANT: Rgba = rgb(0x222222);

// ---- hairlines ------------------------------------------------------------

/// 1px hairline rules.
pub const BORDER: Rgba = LINE;
/// Input / composer hairline (a touch stronger).
pub const BORDER_INPUT: Rgba = CONTROL_LINE;

// ---- text -----------------------------------------------------------------

/// Body type — Observatory bone, calm vs pure-white glare.
pub const TEXT: Rgba = BRAND_BONE;
/// Secondary text / role labels.
pub const MUTED: Rgba = TEXT_DIM;
/// Faint meta — placeholder, hints.
pub const FAINT: Rgba = TEXT_DIM;
/// Text on the accent fill (the user's own bubble). Stays dark ink: against
/// Awili blue it measures ~4.97:1, where white would only reach ~3.68:1.
pub const ON_ACCENT: Rgba = rgb(0xffffff);

// ---- chips (provider/model + rules) ---------------------------------------

/// Chip pill fill (the provider/model + rules chips in the composer row).
pub const CHIP_BG: Rgba = rgb(0x2c2c2c);
/// Chip label text.
pub const CHIP_TEXT: Rgba = TEXT_DIM;
/// Chip hairline.
pub const CHIP_BORDER: Rgba = CONTROL_LINE;
/// Chip text when it signals a degraded state (memory offline).
pub const CHIP_WARN: Rgba = BRAND_SAGE;

// ---- needs-you panel (slice 4) ---------------------------------------------

/// `critical` severity tag — the one red in the panel; muted, not alarm-glare.
pub const SEV_CRITICAL: Rgba = rgb(0xd07a6f);
/// `attention` severity tag — shares the degraded-state amber family.
pub const SEV_ATTENTION: Rgba = BRAND_SAGE;

// ---- RC2-CNCL8: the Council page (Observatory, mockup tokens) ----------------

/// A member answering right now (its dot): Observatory sage.
pub const COUNCIL_LIVE: Rgba = BRAND_SAGE;
/// A member that didn't answer — worth a look, not an alarm: muted violet.
pub const COUNCIL_LOOK: Rgba = rgb(0xa48dd0);
/// The left nav rail's 1 px outline: the one sanctioned purple edge (owner
/// 2026-11-01), always the identity token.
pub const RAIL_EDGE: Rgba = COUNCIL_LOOK;
/// RC2-CNCL10: the one red: the owner must act (sign in, add a key).
pub const COUNCIL_ACT: Rgba = rgb(0xd4574a);

// ---- conversations sidebar (slice 7) ---------------------------------------

/// Accent at a given alpha — `const`-evaluable for tinted fills. Components
/// must track [`ACCENT`] (= [`BRAND_BLUE`]); the test below pins that.
const fn accent_a(a: f32) -> Rgba {
    Rgba {
        r: 0x25 as f32 / 255.0,
        g: 0x63 as f32 / 255.0,
        b: 0xeb as f32 / 255.0,
        a,
    }
}

/// Sidebar conversation row on hover.
pub const ROW_HOVER: Rgba = white_a(0.05);
/// The active conversation row — a quiet accent tint, not a full fill.
pub const ROW_ACTIVE: Rgba = accent_a(0.14);

/// Per-message search-highlight fill (slice 16 §3): the accent at 30% behind
/// every occurrence of the selected search term in the active conversation.
pub const SEARCH_HIGHLIGHT: Rgba = accent_a(0.30);

/// Text-selection fill behind selected composer characters (A1). The accent at
/// a quiet alpha so selected text stays readable over the input fill.
pub const SELECTION: Rgba = accent_a(0.28);
/// Transcript selection inside the blue user bubble. Reusing the blue accent
/// there is nearly invisible, so use a strong light-gray wash instead. The
/// selectable-message renderer paints this *behind* the dark bubble ink.
pub const SELECTION_ON_ACCENT: Rgba = color_a(rgb(0x0b1d4d), 0.60);

// ---- settings panel (slice 7) -----------------------------------------------

/// The dimming layer behind the settings modal.
pub const SCRIM: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.55,
};

// ---- signal accent --------------------------------------------------------

/// Awili blue — the caret and the user's own messages.
///
/// Was `rgb(0x5db8d4)`, the blue-shifted teal from Owner's "more blue, less
/// green" pass (#4). Repointed at the brand pack's Awili blue in the v1.0
/// rebrand; revert this one line to restore the old teal.
pub const ACCENT: Rgba = BRAND_BLUE;
/// Brighter blue — the caret bar. Derived one step lighter than [`ACCENT`]
/// (the brand pack defines no bright variant of its own).
pub const ACCENT_BRIGHT: Rgba = rgb(0x60a5fa);
/// Observatory sage — the second collaborating voice.
pub const ACCENT_SECONDARY: Rgba = BRAND_SAGE;
/// Quiet field and rim behind the live thinking pulse.
pub const THINKING_HALO: Rgba = color_a(BRAND_SAGE, 0.14);
pub const THINKING_RING: Rgba = color_a(BRAND_SAGE, 0.48);
/// Conversation scrollbar: Codex-style overlay chrome. The visible thumb is
/// deliberately quiet; the forgiving interaction target remains much wider.
pub const SCROLLBAR_TRACK: Rgba = white_a(0.0);
pub const SCROLLBAR_THUMB: Rgba = white_a(0.34);

// ---- markdown (message bubbles, slice 6) -----------------------------------

/// Code-block fill — a black overlay so it reads on both the
/// charcoal assistant bubble and the blue user bubble.
pub const CODE_BG: Rgba = black_a(0.27);
/// inline code: a lighter fill than blocks so dense prose does not read as a
/// row of pills (O1). Blocks keep the heavier [`CODE_BG`].
pub const INLINE_CODE_BG: Rgba = black_a(0.14);
/// Code-block hairline.
pub const CODE_BORDER: Rgba = LINE;
/// Links in assistant bubbles (bright blue + underline).
pub const LINK: Rgba = ACCENT_BRIGHT;
/// Links in the user's own accent-filled bubble — the bubble ink color; the
/// underline carries the affordance there.
pub const LINK_ON_ACCENT: Rgba = rgb(0xffffff);
/// Blockquote left bar (the sage — a quiet "someone else said this").
pub const QUOTE_BAR: Rgba = BRAND_SAGE;

/// Code text size / line height (blocks and the language label row).
pub const CODE_TEXT_SIZE: f32 = 13.0;
pub const CODE_LINE_HEIGHT: f32 = 20.0;

/// Monospace family for code. A missing family falls back to gpui's default
/// font stack, so an unusual Linux box degrades instead of panicking.
#[cfg(target_os = "macos")]
pub const MONO_FONT: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
pub const MONO_FONT: &str = "DejaVu Sans Mono";

/// Heading text size by markdown level (h4+ render at body size, bold).
pub const fn heading_size(level: u8) -> f32 {
    match level {
        1 => type_::TITLE.size,
        2 => type_::HEADING.size,
        3 => type_::READ.size,
        _ => type_::READ.size,
    }
}

// ---- typography / metrics -------------------------------------------------

/// Body / composer text size.
pub const TEXT_SIZE: f32 = 15.0;
/// Message + composer line height.
pub const LINE_HEIGHT: f32 = 25.0;
/// Role-label (sender) caption size.
pub const LABEL_SIZE: f32 = 11.0;

/// Universal corner radius.
pub const RADIUS: f32 = 4.0;

/// Caret bar width / height.
pub const CARET_W: f32 = 2.0;
pub const CARET_H: f32 = 18.0;

/// The composer soft-wraps and auto-grows up to this many visual rows, then
/// holds that height and scrolls vertically (A2). Sized so a paragraph is
/// comfortably visible without the field swallowing the whole window.
pub const COMPOSER_MAX_ROWS: usize = 8;

/// Composer placeholder shown when the draft is empty.
pub const COMPOSER_PLACEHOLDER: &str = "Message Awili — Enter to send, Shift+Enter for a newline";

// ---- v2 Studio tokens (slice 1: additive; consumers migrate in later slices) ----
//
// Colour meanings (owner rule 2026-10-09): CONTRAST (brand blue) = primary
// action, selection, focus, highlight. ATTENTION (brand sage) = needs
// attention, not urgent. ACT_NOW (red) = act now, and nothing else.
//
// Migration table — old name → the v2 token its consumers move to (slice 4+).
// The old names below stay exactly as they are until their consumers move:
//   BG → CANVAS · CHROME → GROUND · RAISED / BUBBLE_ASSISTANT → PANEL_SOLID
//   BORDER → LINE · BORDER_INPUT / CHIP_BORDER → CONTROL_LINE
//   ROW_HOVER → FILL_HOVER · MUTED / CHIP_TEXT → TEXT_DIM · FAINT → TEXT_DIM
//   ACCENT → CONTRAST · ON_ACCENT → ON_CONTRAST · ROW_ACTIVE → CONTRAST_SOFT
//   ACCENT_SECONDARY / COUNCIL_LIVE → per slice-4 audit (ATTENTION or TEXT_DIM)
//   CHIP_WARN / SEV_ATTENTION / COUNCIL_LOOK → ATTENTION (per slice-4 audit)
//   SEV_CRITICAL / COUNCIL_ACT → ACT_NOW (per slice-4 audit)
//   THINKING_HALO → CONTRAST_SOFT · THINKING_RING → CONTRAST (slice 3)
//   SPACE_* → SP_* · RADIUS → RADIUS_M · TEXT_SIZE → type_::BODY.size
//   LABEL_SIZE → type_::CAPTION.size

/// V2 ground — the darkest surface, the composer chrome bar (replaces CHROME).
pub const GROUND: Rgba = rgb(0x141414);
/// V2 window / page / message-list canvas (replaces BG).
pub const CANVAS: Rgba = rgb(0x1a1a1a);
/// V2 translucent panel fill — the panel color at 78% over the canvas.
pub const PANEL: Rgba = rgb(0x202020);
/// V2 solid panel / bubble surface (replaces RAISED / BUBBLE_ASSISTANT).
pub const PANEL_SOLID: Rgba = rgb(0x202020);
/// V2 raised surface — one step above the panel for stacked layers.
pub const RAISE: Rgba = rgb(0x262626);
/// V2 hairline rules (replaces BORDER).
pub const LINE: Rgba = rgb(0x2e2e2e);
/// V2 input / control hairline (replaces BORDER_INPUT / CHIP_BORDER).
///
/// v3 (KIT.md): lilac at 0.72 clears the
/// 3:1 floor on `PANEL_SOLID`, `CANVAS` and `RAISE` (K1 contrast tests).
pub const CONTROL_LINE: Rgba = rgb(0x555555);
/// V2 ghost fill for quiet interactive regions.
pub const FILL_GHOST: Rgba = white_a(0.06);
/// V2 row hover fill (replaces ROW_HOVER).
pub const FILL_HOVER: Rgba = white_a(0.10);
/// V2 highlight wash behind selected / focused rows.
pub const HIGHLIGHT: Rgba = white_a(0.04);
/// V2 explicit no-fill token, so "no fill" is a token, not an inline literal.
/// Exact value: (0, 0, 0, 0) — value-identical to the `gpui::rgba(0x00000000)`
/// literal it replaced (pinned by `transparent_is_exactly_rgba_0x00000000`).
pub const TRANSPARENT: Rgba = black_a(0.0);
/// V2 long reading text (reply body). Not a MUTED replacement: MUTED → TEXT_DIM.
pub const TEXT_2: Rgba = rgb(0xb8b8b8);
/// V2 faint meta — placeholders, hints (replaces MUTED / CHIP_TEXT / FAINT).
///
/// v3 (KIT.md): `#93939b` (neutral gray) clears the
/// 4.5:1 WCAG AA floor on `RAISE`, `PANEL_SOLID`, `CANVAS` and the
/// `PANEL`-over-`CANVAS` composite (K1 contrast tests).
pub const TEXT_DIM: Rgba = rgb(0xa0a0a0);
/// V2 primary-action color — brand blue: action, selection, focus, highlight (replaces ACCENT).
pub const CONTRAST: Rgba = BRAND_BLUE;
/// V2 text on the CONTRAST fill (replaces ON_ACCENT).
pub const ON_CONTRAST: Rgba = ON_ACCENT;
/// V2 quiet accent tint behind active / thinking rows (replaces ROW_ACTIVE).
pub const CONTRAST_SOFT: Rgba = color_a(BRAND_BLUE, 0.14);
/// V2 hover fill for the `CONTRAST` face — the mockup's
/// `.btn.primary:hover` / `.ibtn.primary:hover` `#5592f7` (K2c).
pub const CONTRAST_HOVER: Rgba = rgb(0x1d4ed8);
/// V2 hover fill for the `CONTRAST_SOFT` lead face: the soft blue a step
/// stronger (K2c; the mockup gives `.btn.lead` no hover, so the step is
/// the kit's — `CONTRAST_TEXT` and `TEXT` stay ≥ 4.5:1 on it, tested).
pub const CONTRAST_SOFT_HOVER: Rgba = color_a(BRAND_BLUE, 0.22);
/// V2 attention color — brand sage: needs attention, not urgent.
pub const ATTENTION: Rgba = BRAND_SAGE;
/// V2 quiet attention tint.
pub const ATTENTION_SOFT: Rgba = TRANSPARENT;
/// V2 act-now red — the act-now signal, and nothing else.
pub const ACT_NOW: Rgba = rgb(0xd4574a);
/// V2 act-now label text — a readable red-family ink on dark surfaces.
pub const ACT_NOW_TEXT: Rgba = rgb(0xf0b2aa);
/// V2 quiet act-now tint behind act-now rows.
pub const ACT_NOW_SOFT: Rgba = TRANSPARENT;

/// V2 blue *text* — links, "More · Code", the task-bar lead label (KIT.md v3).
/// `CONTRAST` stays for fills, rings and dots; this is the readable-ink
/// variant, pinned to `ACCENT_BRIGHT` `#60a5fa`. Must be ≥ 4.5:1 on
/// `PANEL_SOLID`, the `PANEL`-over-`CANVAS` composite and `RAISE` (K1 tests).
pub const CONTRAST_TEXT: Rgba = ACCENT_BRIGHT;
/// The Attention tone's outline: brand sage at 0.35 (KIT.md v3).
pub const ATTENTION_EDGE: Rgba = BRAND_SAGE;
/// The ActNow tone's outline: the act-now red at 0.45 (KIT.md v3).
pub const ACT_NOW_EDGE: Rgba = ACT_NOW;

/// The float shadow, as two layers: (y offset, blur, black alpha).
pub const FLOAT_SHADOW: [(f32, f32, f32); 2] = [(12.0, 40.0, 0.45), (2.0, 8.0, 0.30)];

pub mod type_ {
    /// A text style: size and line height in logical px (scale with `z`).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct TypeStyle {
        pub size: f32,
        pub line: f32,
    }
    /// Caption — small meta and role labels (replaces LABEL_SIZE).
    pub const CAPTION: TypeStyle = TypeStyle {
        size: 12.0,
        line: 16.0,
    };
    /// Label — field and section labels.
    pub const LABEL: TypeStyle = TypeStyle {
        size: 13.0,
        line: 18.0,
    };
    /// UI — controls and dense interface text.
    pub const UI: TypeStyle = TypeStyle {
        size: 14.0,
        line: 20.0,
    };
    /// Body — the default reading size (replaces TEXT_SIZE).
    pub const BODY: TypeStyle = TypeStyle {
        size: 15.0,
        line: 22.0,
    };
    /// Read — long-form prose.
    pub const READ: TypeStyle = TypeStyle {
        size: 17.0,
        line: 27.0,
    };
    /// Heading — section headings.
    pub const HEADING: TypeStyle = TypeStyle {
        size: 19.0,
        line: 25.0,
    };
    /// Title — page and modal titles.
    pub const TITLE: TypeStyle = TypeStyle {
        size: 24.0,
        line: 30.0,
    };
}

/// 4 px — the smallest v2 spacing step.
pub const SP_4: f32 = 4.0;
/// 8 px spacing step.
pub const SP_8: f32 = 8.0;
/// 12 px spacing step.
pub const SP_12: f32 = 12.0;
/// 16 px spacing step.
pub const SP_16: f32 = 16.0;
/// 20 px spacing step.
pub const SP_20: f32 = 20.0;
/// 24 px spacing step.
pub const SP_24: f32 = 24.0;
/// 32 px spacing step.
pub const SP_32: f32 = 32.0;
/// 40 px spacing step.
pub const SP_40: f32 = 40.0;
/// 56 px spacing step.
pub const SP_56: f32 = 56.0;

/// Opacity for a visually dimmed SettingRow line; its control remains live.
pub const SETTING_ROW_VISUAL_DIM_OPACITY: f32 = 0.55;

/// The physical debug marker used to expose the Settings-owned keyboard focus.
pub const SETTINGS_FOCUS_MARKER_PX: f32 = 1.0;
/// Width of the Settings zoom percentage slot, keeping its controls aligned.
pub const SETTINGS_ZOOM_VALUE_W: f32 = 64.0;
/// Minimum readable width for the Settings change-status message.
pub const SETTINGS_STATUS_MIN_W: f32 = 88.0;
/// Maximum width of a read-only Settings value beside its Change action.
pub const SETTINGS_VALUE_MAX_W: f32 = 240.0;
/// Width of the inline Settings editor and its readable text buffer.
pub const SETTINGS_EDITOR_W: f32 = 320.0;

/// Tight corners — small chips and tags.
pub const RADIUS_TIGHT: f32 = 4.0;
/// Small corners — compact controls.
pub const RADIUS_S: f32 = 4.0;
/// Medium corners — the default v2 corner (replaces RADIUS).
pub const RADIUS_M: f32 = 4.0;
/// Large corners — cards and sheets.
pub const RADIUS_L: f32 = 4.0;
/// Extra-large corners — modals and floaters.
pub const RADIUS_XL: f32 = 4.0;
/// Fully rounded — pills and dots.
pub const RADIUS_PILL: f32 = 999.0;
/// The focus ring's width and its gap from the part (logical px before
/// zoom; the spec's 2 px / 2 px). `ui::focus_ring` draws it.
pub const RING: f32 = 2.0;
/// Frozen ring border and gap overhang the actual hit target.
pub fn focus_ring_overhang() -> Pixels {
    z(RING + RING)
}
/// A real hairline: the 1 px `HIGHLIGHT` top edge of a floating face. Like
/// every `border_1()` in the app it does not scale with interface zoom.
pub const HAIRLINE: f32 = 1.0;
/// Retained legacy Council member marker size.
pub const LEGACY_MEMBER_DOT: f32 = 7.0;
/// Retained provider section rule outset.
pub const PROVIDER_RULE_OUTSET: f32 = -2.0;

// Part dimensions (whole-kit review: every design value lives here).
/// The thinking dot's breathing frames, in logical px before zoom:
/// `THINK_MIN` → `THINK_MAX` → `THINK_MIN` over 12 frames (symmetric, every
/// frame within `6..=12`).
pub const THINK_FRAMES: [f32; 12] = [
    6.0, 7.0, 8.5, 10.0, 11.0, 12.0, 12.0, 11.0, 10.0, 8.5, 7.0, 6.0,
];
/// A level's Page Up / Page Down step, in notches (×10, KIT.md § Level).
pub const LEVEL_PAGE_STEP: i64 = 10;
/// A file chip's longest shown name, in characters, before middle-truncation.
pub const CHIP_FILE_NAME_MAX: usize = 30;
/// The characters a middle-truncated file name keeps from its end (the extension side).
pub const CHIP_FILE_NAME_TAIL: usize = 12;
/// The notice stack shows at most this many notices (KIT.md § Notice).
pub const NOTICE_MAX_VISIBLE: usize = 3;
/// The tab strip's overflow edge fade, in steps.
pub const TABS_FADE_STEPS: usize = 8;
/// The gallery screens' card grid columns (the mockups' auto-fit resolves to three in the fixed frame).
pub const GALLERY_GRID_COLS: u16 = 3;
/// The scrollbar's hide fade, in steps over `motion::BASE_MS`.
pub const SCROLLBAR_FADE_STEPS: u32 = 5;
/// A level's keyboard steps across its range when no `.notches(n)` is set.
pub const LEVEL_DEFAULT_NOTCHES: u32 = 16;
/// A chart legend swatch's corner radius.
pub const CHART_SWATCH_RADIUS: f32 = 2.0;
/// The act-now pulse halo's resting width, a box shadow outside the dot.
pub const DOT_HALO: f32 = 2.0;
/// The empty state's glyph tile (the mockup's `.empty .glyph` 72 px).
pub const EMPTY_GLYPH: f32 = 72.0;
/// The empty state's glyph tile corner radius (the mockup's 22 px).
pub const EMPTY_GLYPH_RADIUS: f32 = 22.0;
/// The icon inside the empty state's glyph tile.
pub const EMPTY_GLYPH_ICON: f32 = 34.0;
/// The empty state's widest (the mockup's `.empty { max-width: 520px }`).
pub const EMPTY_MAX: f32 = 520.0;
/// A badge dot's surface ring (an outer box shadow), on icon buttons and the rail.
pub const BADGE_RING: f32 = 2.0;
/// The level's painted thumb diameter (inside its 44 px hit box).
pub const LEVEL_THUMB: f32 = 20.0;
/// The level's track height.
pub const LEVEL_TRACK_H: f32 = 4.0;
/// The level's track edge (a hairline).
pub const LEVEL_TRACK_EDGE: f32 = 1.0;
/// The level fill's narrowest visible width.
pub const LEVEL_MIN_FILL: f32 = 1.0;
/// The command palette's width (the mockup's 560 px).
pub const PALETTE_W: f32 = 560.0;
/// The scrollbar thumb's shortest.
pub const SCROLLBAR_MIN_THUMB: f32 = 36.0;
/// The scrollbar track's inset from the strip's ends.
pub const SCROLLBAR_TRACK_INSET: f32 = 8.0;
/// A skeleton block's height.
pub const SKELETON_BLOCK_H: f32 = 24.0;
/// A skeleton line's height.
pub const SKELETON_LINE_H: f32 = 12.0;
/// The tab strip's overflow edge fade width.
pub const TABS_FADE_W: f32 = 6.0;
/// Long tab labels truncate with their complete label in the tab tooltip.
pub const TAB_MAX_W: f32 = 240.0;
/// The stepper's value column width (tabular, fits "1.25×").
pub const STEPPER_VALUE_W: f32 = 64.0;

// Part dimensions (whole-kit review: every design value lives here).

/// Minimum hit-area side, in logical px, BEFORE zoom.
pub const TARGET: f32 = 44.0;
/// The legacy Work/Code command-palette card's existing desktop height cap.
pub const LEGACY_PALETTE_CARD_MAX_H: f32 = 320.0;
/// Minimum vertical space left around the centered legacy palette card.
pub const LEGACY_PALETTE_WINDOW_MARGIN: f32 = EDGE * 2.0;
/// Vertical spacing reserved around the legacy palette's row viewport.
pub const LEGACY_PALETTE_BODY_CHROME_H: f32 = SP_8 + SP_12;
/// Floating-face top and bottom borders around the legacy palette card.
pub const LEGACY_PALETTE_FACE_BORDER_H: f32 = 2.0;
/// LocalDiscovery's bounded results viewport shows five minimum-height target rows.
pub const LOCAL_DISCOVERY_RESULTS_H: f32 = TARGET * 5.0;
/// The hit-area side to lay out with, already scaled for zoom and clamped:
/// `px(max(44, 44 × ui_scale))`. Use the returned value directly — do NOT wrap
/// it in `z()` and do NOT use `z(TARGET)` (that falls to 22 px at zoom 0.5).
pub fn target() -> Pixels {
    px(TARGET.max(TARGET * ui_scale()))
}

pub mod motion {
    /// Fast — micro-feedback (hover, press).
    pub const FAST_MS: u64 = 150;
    /// Base — most standard transitions.
    pub const BASE_MS: u64 = 200;
    /// Slow — larger surfaces and sheets.
    pub const SLOW_MS: u64 = 250;
    /// One breath of the thinking dot (ambient loop).
    pub const THINK_MS: u64 = 1440;
    /// The act-now dot's pulse (ambient loop).
    pub const PULSE_MS: u64 = 2400;
    /// The act-now pulse runs as `PULSE_FRAMES` steps of `PULSE_MS / PULSE_FRAMES`
    /// (100 ms) on the app's animation tick, like the thinking dot.
    pub const PULSE_FRAMES: u8 = 24;
    /// The share of the pulse cycle over which the halo grows and fades (the
    /// mockup's ` pulse` 70 % stop); it rests for the remainder.
    pub const PULSE_RISE: f32 = 0.7;
    /// The halo's opacity at the start of each pulse (the mockup's 0.5).
    pub const PULSE_START_ALPHA: f32 = 0.5;
    /// The thinking dot advances one breathing frame every this many ms.
    pub const THINK_STEP_MS: u64 = 120;
    /// The scrollbar hides this long after the last scroll (KIT.md § ScrollBar).
    pub const SCROLLBAR_HIDE_MS: u64 = 1000;
    /// A setting row's "Saved" flash lasts this long.
    pub const SAVED_FLASH_MS: u64 = 1500;
    /// Holding a stepper button repeats after this delay ...
    pub const HOLD_DELAY_MS: u64 = 400;
    /// ... and then every this many ms until the release.
    pub const HOLD_EVERY_MS: u64 = 80;
    /// A secret field's armed "Click again to clear" lapses after this long.
    pub const CLEAR_ARMED_MS: u64 = 4000;
    /// A non-act-now notice's default lifetime, in seconds (KIT.md § Notice).
    pub const NOTICE_TIMED_SECS: u64 = 12;
    /// Ease-out cubic-bezier control points; no bounce.
    pub const EASE: [f32; 4] = [0.2, 0.8, 0.2, 1.0];
}

// ---- v2 kit sizes (slice K, batch K1) --------------------------------------
//
// The kit's painted sizes, in logical px BEFORE zoom. Use them through `z()`
// (hit areas through `target()`), never as raw `px()` literals — a kit file
// adds zero design-lint findings (docs/v2/KIT.md § Conventions).

/// Icons — the kit's default icon size.
pub const ICON: f32 = 18.0;
/// Icons, large — primary icon buttons.
pub const ICON_L: f32 = 20.0;
/// Icons, small — chip leading icons.
pub const ICON_S: f32 = 14.0;
/// Status dot diameter.
pub const DOT: f32 = 8.0;
/// Status dot, large.
pub const DOT_L: f32 = 10.0;
/// Badge dot diameter (top-right state / count dots).
pub const BADGE: f32 = 9.0;
/// Toggle switch track width (painted).
pub const SWITCH_W: f32 = 44.0;
/// Toggle switch track height (painted).
pub const SWITCH_H: f32 = 26.0;
/// The toggle knob's inset from the track edge (KIT.md § Toggle,
/// batch K4).
pub const SWITCH_KNOB_PAD: f32 = 2.0;
/// The toggle knob diameter (`SWITCH_H` minus the two insets).
pub const SWITCH_KNOB: f32 = SWITCH_H - 2.0 * SWITCH_KNOB_PAD;
/// The toggle knob's left offset when on: the same inset from the
/// right edge as from the left when off (`SWITCH_W − SWITCH_KNOB −
/// SWITCH_KNOB_PAD` = 20; K4b — it was 22, flush with the edge).
pub const SWITCH_KNOB_ON_X: f32 = SWITCH_W - SWITCH_KNOB - SWITCH_KNOB_PAD;
/// Menu width.
pub const MENU_W: f32 = 280.0;
/// Panel width (default).
pub const PANEL_W: f32 = 360.0;
/// Composer frame maximum width.
pub const COMPOSER_MAX: f32 = 720.0;
/// Existing Chat Needs You dock width, shared with composer allocation.
pub const NEEDS_YOU_W: f32 = 320.0;

/// Resolve the outer composer width before intrinsic height measurement.
pub fn composer_width(window_width: Pixels, dock_width: Pixels) -> Pixels {
    (window_width - z(STAGE_LEFT) - dock_width)
        .max(Pixels::ZERO)
        .min(z(COMPOSER_MAX))
}
/// Readable composer content width reserved before admitting a Job dock.
pub const JOB_EDITOR_MIN_W: f32 = 240.0;
/// Stacked editor and action rows retain their full minimum allocations.
pub fn composer_stacked_row_min() -> Pixels {
    target() * 2. + z(SP_4)
}
/// Exact composer chrome plus action targets and a readable editor reserve.
pub fn composer_actions_need_row(width: Pixels, count: usize) -> bool {
    width
        < z(JOB_EDITOR_MIN_W)
            + (target() + px(HAIRLINE * 2.)) * count as f32
            + z(SP_16 * 2. + SP_12 * 3. + SP_8 + SP_4 * count as f32)
            + px(HAIRLINE * 2.)
}
/// Editor content, five hit targets with room for their control hairlines,
/// composer outer/frame/input padding, five row gaps and the frame border.
pub fn job_composer_min_width() -> Pixels {
    z(JOB_EDITOR_MIN_W)
        + (target() + px(HAIRLINE * 2.)) * 5.
        + z(SP_16 * 2. + SP_12 * 3. + SP_8 + SP_4 * 5.)
        + px(HAIRLINE * 2.)
}
/// Long-reading column maximum width.
pub const READING_MAX: f32 = 680.0;
/// Bounded inline attachment preview; source pixels remain available to vision.
pub const IMAGE_PREVIEW_HEIGHT: f32 = 280.0;
/// Page content maximum width.
pub const PAGE_MAX: f32 = 960.0;
/// The Council column's widest (the mockup's `.wide { max-width: 1120px }`).
pub const WIDE_MAX: f32 = 1120.0;
/// The margin floating surfaces keep from the window edge.
pub const EDGE: f32 = 20.0;
/// The composer status strip (docs/v2/strip, DeepSeek's design): one 22 px
/// row of 11 px type under the composer box.
pub const STRIP_H: f32 = 22.0;
/// The strip's type size (one step under `type_::CAPTION`).
pub const STRIP_TEXT: f32 = 11.0;
/// The strip's text style (11 px on the 22 px row).
pub const STRIP_TYPE: type_::TypeStyle = type_::TypeStyle {
    size: STRIP_TEXT,
    line: STRIP_H,
};
/// The strip's state dot.
pub const STRIP_DOT: f32 = 5.0;
/// The gap between the state dot and its words.
pub const STRIP_DOT_GAP: f32 = 8.0;
/// The detail popover's width.
pub const STRIP_POP_W: f32 = 316.0;
/// The gap between the strip and the popover opening above it.
pub const STRIP_POP_GAP: f32 = 10.0;
/// The popover's side padding.
pub const STRIP_POP_PAD_X: f32 = 14.0;
/// The popover's top padding.
pub const STRIP_POP_PAD_T: f32 = 13.0;
/// The popover's bottom padding.
pub const STRIP_POP_PAD_B: f32 = 11.0;
/// One key-value row in the popover.
pub const STRIP_ROW_H: f32 = 18.0;
/// The popover's section title size.
pub const STRIP_SEC_TEXT: f32 = 10.0;
/// The popover's footnote size.
pub const STRIP_FOOT_TEXT: f32 = 9.5;
/// The popover's title size.
pub const STRIP_TITLE_TEXT: f32 = 12.5;
/// The context meter's height in the popover.
pub const STRIP_METER_H: f32 = 3.0;
/// The popover's rule margin above.
pub const STRIP_RULE_TOP: f32 = 11.0;
/// The popover's rule margin below.
pub const STRIP_RULE_BOTTOM: f32 = 9.0;
/// How long the strip waits with no first token before it says "Loading".
pub const STRIP_LOADING_AFTER_MS: u64 = 1500;
/// The streaming dot's breath, one full cycle (the mockup's 1.7 s).
pub const STRIP_BREATH_MS: u64 = 1700;
/// The popover's fade in (the mockup's 130 ms).
pub const STRIP_FADE_MS: u64 = 130;
/// One repaint of the popover's fade (about 60 fps).
pub const STRIP_FADE_FRAME_MS: u64 = 16;
/// The popover's hairline rules.
pub const STRIP_RULE_H: f32 = 1.0;
/// Nav-rail button side.
pub const RAIL_BUTTON: f32 = 48.0;
/// Nav-rail item width (the 64 × 58 item, KIT.md § NavRail, batch K4).
pub const RAIL_ITEM_W: f32 = 64.0;
/// Nav-rail item height (icon over its `type_::CAPTION` label).
pub const RAIL_ITEM_H: f32 = 58.0;
/// The nav rail's face width (the item column + both paddings).
pub const RAIL_W: f32 = RAIL_ITEM_W + 2.0 * SP_8;
/// Scrollbar pointer target (the declared 20 px exception).
pub const SCROLLBAR_HIT: f32 = 20.0;
/// Scrollbar painted thumb width. Named `*_W` because the name
/// `SCROLLBAR_THUMB` is taken by the pre-v2 colour token above.
pub const SCROLLBAR_THUMB_W: f32 = 6.0;
/// ThinkingDot halo circle diameter.
pub const THINK_HALO: f32 = 20.0;
/// ThinkingDot minimum dot size.
pub const THINK_MIN: f32 = 6.0;
/// ThinkingDot maximum dot size.
pub const THINK_MAX: f32 = 12.0;
/// The act-now pulse halo's widest spread, in logical px before zoom.
pub const PULSE_SPREAD: f32 = 8.0;
/// A `ui::Level` slider's minimum width (K4b: a slider must never
/// collapse to nothing in a squeezed row).
pub const LEVEL_MIN_W: f32 = 120.0;
/// The activity island pill's height (the mockup's `.island { min-height:
/// 52px }`).
pub const ISLAND_H: f32 = 52.0;
/// A numbered-list marker's circle (the mockup's `.reply li::before`, 26 px).
pub const MARKER: f32 = 26.0;
/// An empty state's top margin as a share of the window height (the
/// mockup's `.empty { margin: 12vh auto 0 }`).
pub const EMPTY_TOP: f32 = 0.12;
/// The design gallery's window size (KIT.md § The gallery: it renders at
/// 1280 wide, and its tests also at 860).
pub const GALLERY_W: f32 = 1280.0;
/// The design gallery's window height.
pub const GALLERY_H: f32 = 900.0;
/// A gallery sample's narrow column: long-text samples sit in it so their
/// truncation (or wrapping) is visible.
pub const GALLERY_NARROW: f32 = 160.0;
/// A composed mockup screen's frame in the gallery (the five Studio
/// screens, side by side with the HTML mockups).
pub const SCREEN_W: f32 = 1200.0;
/// A composed mockup screen's frame height.
pub const SCREEN_H: f32 = 760.0;
/// The gallery page's narrowest: a screen frame plus the page's `SP_40`
/// padding each side, so a narrower window scrolls sideways to the screens
/// instead of clipping them.
pub const GALLERY_PAGE_MIN_W: f32 = SCREEN_W + SP_40 * 2.0;
/// The stage's padding in a screen — the mockup's `.stage { padding: 96px
/// 420px 210px 132px }` (the dock and the panel use the same left / right).
pub const STAGE_TOP: f32 = 96.0;
/// The stage's left: past the rail (`EDGE + RAIL_W + SP_32` = 132).
pub const STAGE_LEFT: f32 = EDGE + RAIL_W + SP_32;
/// Minimum internal viewport for transitional Backstage section headers/lists.
/// The bounded outer section scrolls to this viewport in compact windows.
/// Remove this bridge when the service interiors migrate to kit layout.
pub const BACKSTAGE_SECTION_MIN_H: f32 = 240.0;
/// Messaging connector-list column width in logical px before zoom.
pub const BACKSTAGE_MESSAGING_LIST_W: f32 = 280.0;
/// Messaging detail column's preferred flex basis in logical px before zoom.
pub const BACKSTAGE_MESSAGING_DETAIL_W: f32 = 320.0;
/// Messaging connector-list scroll area's maximum height in logical px before zoom.
pub const BACKSTAGE_MESSAGING_LIST_MAX_H: f32 = 480.0;
/// Messaging environment-variable list's maximum height in logical px before zoom.
pub const BACKSTAGE_MESSAGING_ENV_MAX_H: f32 = 360.0;
/// The stage's right: clear of the panel (`PANEL_W + 3 × EDGE` = 420).
pub const STAGE_RIGHT: f32 = PANEL_W + EDGE * 3.0;
/// The stage's bottom: clear of the dock.
pub const STAGE_BOTTOM: f32 = 210.0;
/// The floating panel's top (the mockup's `.panel { top: 88px }`).
pub const PANEL_TOP: f32 = 88.0;
/// A page's right and bottom padding (the mockup's `.page { padding: 96px
/// 48px 72px 132px }`).
pub const PAGE_PAD_R: f32 = 48.0;
/// A page's bottom padding.
pub const PAGE_PAD_B: f32 = 72.0;
/// The owner's message bubble's widest (the mockup's `.you { max-width:
/// min(78%, 560px) }`).
pub const YOU_MAX: f32 = 560.0;
/// The owner's bubble's widest share of the thread (the `78%` of the same
/// rule).
pub const YOU_FRAC: f32 = 0.78;
/// The Design screen's empty-state top margin, below the page's top
/// padding: `EMPTY_TOP` (12 %) of the screen frame, the mockup's `12vh` of
/// its window.
pub const DESIGN_EMPTY_TOP: f32 = SCREEN_H * EMPTY_TOP;
/// A settings row's width in the gallery (two panel widths).
pub const SETTINGS_ROW_W: f32 = PANEL_W * 2.0;

/// The compact-window floor, in logical px before zoom (KIT.md § Sheet,
/// batch K3): below this width a floating `Panel` becomes a `Sheet`
/// (`ui::is_compact`).
pub const COMPACT_W: f32 = 880.0;
/// The top bar collapses its labels (project name, model family, mode, New,
/// the run capsule) when the bar itself is narrower than this, in logical px
/// before zoom. The bar is the window less the rail and any docked panel; the
/// full labels need about this much room.
pub const BAR_NARROW_W: f32 = 1180.0;
/// Above this width the run capsule also shows the plan step and posture.
pub const BAR_WIDE_W: f32 = 1400.0;
/// The run capsule's reserved slot in the top bar: full, and collapsed to a
/// dot and timer. The slot is this wide whether or not a run is showing, so
/// starting or ending a run moves nothing.
pub const RUN_SLOT_W: f32 = 268.0;
/// The slot at Wide, sized to hold the step and plan posture (measured: the
/// widest real capsule is about 430 px).
pub const RUN_SLOT_WIDE_W: f32 = 440.0;
pub const RUN_SLOT_NARROW_W: f32 = 176.0;
/// The top bar is as tall as one kit target plus a gap above and below.
pub fn bar_h() -> Pixels {
    target() + z(SP_4) * 2.0
}
/// Minimum readable Council track width, including body and control padding.
pub const COUNCIL_TRACK_MIN_W: f32 = 160.0;

/// The float shadow as `gpui::BoxShadow`s: built from [`FLOAT_SHADOW`]
/// through `z()` (interface zoom scales it), black at each layer's alpha,
/// `offset(0, y)`, the given blur, spread 0.
pub fn float_shadow() -> Vec<gpui::BoxShadow> {
    FLOAT_SHADOW
        .iter()
        .map(|&(y, blur, alpha)| gpui::BoxShadow {
            color: black_a(alpha).into(),
            offset: gpui::point(px(0.), z(y)),
            blur_radius: z(blur),
            spread_radius: px(0.),
            inset: false,
        })
        .collect()
}

// ---- v2 kit tone (slice K, batch K1) ----------------------------------------
//
// The four semantic tones (DESIGN.md §2). The `tone_*` accessors read the
// live palette, so a light-mode swap (slice 11) retints them with no kit
// change.

/// The semantic tone of a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Running or done; the quiet default.
    Neutral,
    /// Result ready or needs a look — not urgent.
    Attention,
    /// Blocked on the owner or must-fix — the only red.
    ActNow,
    /// Selection or a numbered marker.
    Contrast,
}

/// The tone's dot / icon colour.
pub fn tone_color(tone: Tone) -> Rgba {
    let p = palette();
    match tone {
        Tone::Neutral => p.text_dim,
        Tone::Attention => p.attention,
        Tone::ActNow => p.act_now,
        Tone::Contrast => p.contrast,
    }
}

/// The tone's text colour.
pub fn tone_text(tone: Tone) -> Rgba {
    let p = palette();
    match tone {
        Tone::Neutral => p.text_dim,
        Tone::Attention => p.attention,
        Tone::ActNow => p.act_now_text,
        Tone::Contrast => p.contrast_text,
    }
}

/// The tone's soft fill; `Neutral` has none (the transparent no-fill token).
pub fn tone_soft(tone: Tone) -> Rgba {
    let p = palette();
    match tone {
        Tone::Neutral => p.transparent,
        Tone::Attention => p.attention_soft,
        Tone::ActNow => p.act_now_soft,
        Tone::Contrast => p.contrast_soft,
    }
}

/// The tone's 1 px outline.
pub fn tone_edge(tone: Tone) -> Rgba {
    let p = palette();
    match tone {
        Tone::Neutral => p.line,
        Tone::Attention => p.attention_edge,
        Tone::ActNow => p.act_now_edge,
        Tone::Contrast => p.contrast,
    }
}

// ---- v2 kit palette (slice K, batch K1) ------------------------------------
//
// Palette access so light mode (slice 11) is a swap: every v2 colour token as
// a field, one named palette per mode, and [`palette`] the accessor. Kit parts
// read colours ONLY through `palette()` — never the colour consts directly.
// Existing app code keeps the consts until its screen migrates.

/// One UI colour palette: every v2 colour token as a field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Legacy keyboard-focus halo, with enough contrast on the active surface.
    pub legacy_focus_ring: Rgba,
    /// V2 ground — the darkest surface.
    pub ground: Rgba,
    /// V2 window / page / message-list canvas.
    pub canvas: Rgba,
    /// V2 translucent panel fill.
    pub panel: Rgba,
    /// V2 solid panel / bubble surface.
    pub panel_solid: Rgba,
    /// V2 raised surface.
    pub raise: Rgba,
    /// V2 hairline rules.
    pub line: Rgba,
    /// V2 input / control hairline.
    pub control_line: Rgba,
    /// V2 ghost fill for quiet interactive regions.
    pub fill_ghost: Rgba,
    /// V2 row hover fill.
    pub fill_hover: Rgba,
    /// V2 highlight wash behind selected / focused rows.
    pub highlight: Rgba,
    /// V2 explicit no-fill.
    pub transparent: Rgba,
    /// Body text.
    pub text: Rgba,
    /// Long reading text.
    pub text_2: Rgba,
    /// Faint meta — placeholders, hints.
    pub text_dim: Rgba,
    /// Primary-action colour: action, selection, focus, highlight (fills,
    /// rings, dots — not text).
    pub contrast: Rgba,
    /// Text on the contrast fill.
    pub on_contrast: Rgba,
    /// Identity mark: the Council accent and the rail edge (purple, never a fill).
    pub identity: Rgba,
    /// Blue text: links, "More · Code", the task-bar lead label.
    pub contrast_text: Rgba,
    /// Quiet accent tint behind active / thinking rows.
    pub contrast_soft: Rgba,
    /// Hover fill for the contrast face.
    pub contrast_hover: Rgba,
    /// Hover fill for the soft contrast (lead) face.
    pub contrast_soft_hover: Rgba,
    /// Attention colour — needs attention, not urgent.
    pub attention: Rgba,
    /// Quiet attention tint.
    pub attention_soft: Rgba,
    /// The attention tone's outline.
    pub attention_edge: Rgba,
    /// Act-now red — the act-now signal, and nothing else.
    pub act_now: Rgba,
    /// Act-now label text.
    pub act_now_text: Rgba,
    /// Quiet act-now tint.
    pub act_now_soft: Rgba,
    /// The act-now tone's outline.
    pub act_now_edge: Rgba,
    /// The dimming layer behind dialogs and sheets (the existing token,
    /// kept out of the v2 block only because it predates it).
    pub scrim: Rgba,
    /// The thinking halo edge (the existing token, for the same reason).
    pub thinking_ring: Rgba,
    /// The thinking dot's halo edge: brand blue at 0.48 (the approved mockup,
    /// studio.css `.think`). The legacy sage `thinking_ring` stays until
    /// slice 3 repoints it.
    pub contrast_ring: Rgba,
    /// A selected block of content (a reply): a whisper of the brand blue
    /// (the mockup's `.reply.sel` background, contrast at 0.05).
    pub selection_fill: Rgba,
    /// Its inset edge (contrast at 0.45).
    pub selection_edge: Rgba,
    /// Code-block fill (the existing token), for `ui::CodeBlock`.
    pub code_bg: Rgba,
    /// Inline-code fill (the existing token).
    pub inline_code_bg: Rgba,
    /// Code-block hairline (the existing token).
    pub code_border: Rgba,
    /// Legacy BG paint role; its Dark value remains exact.
    pub bg: Rgba,
    /// Legacy CHROME paint role; its Dark value remains exact.
    pub chrome: Rgba,
    /// Legacy RAISED paint role; its Dark value remains exact.
    pub raised: Rgba,
    /// Legacy BUBBLE_ASSISTANT paint role; its Dark value remains exact.
    pub bubble_assistant: Rgba,
    /// Legacy BORDER paint role; its Dark value remains exact.
    pub border: Rgba,
    /// Legacy BORDER_INPUT paint role; its Dark value remains exact.
    pub border_input: Rgba,
    /// Legacy MUTED paint role; its Dark value remains exact.
    pub muted: Rgba,
    /// Legacy FAINT paint role; its Dark value remains exact.
    pub faint: Rgba,
    /// Legacy ON_ACCENT paint role; its Dark value remains exact.
    pub on_accent: Rgba,
    /// Legacy CHIP_BG paint role; its Dark value remains exact.
    pub chip_bg: Rgba,
    /// Legacy CHIP_TEXT paint role; its Dark value remains exact.
    pub chip_text: Rgba,
    /// Legacy CHIP_BORDER paint role; its Dark value remains exact.
    pub chip_border: Rgba,
    /// Legacy CHIP_WARN paint role; its Dark value remains exact.
    pub chip_warn: Rgba,
    /// Legacy SEV_CRITICAL paint role; its Dark value remains exact.
    pub sev_critical: Rgba,
    /// Legacy SEV_ATTENTION paint role; its Dark value remains exact.
    pub sev_attention: Rgba,
    /// Legacy COUNCIL_LIVE paint role; its Dark value remains exact.
    pub council_live: Rgba,
    /// Legacy COUNCIL_LOOK paint role; its Dark value remains exact.
    pub council_look: Rgba,
    /// Legacy COUNCIL_ACT paint role; its Dark value remains exact.
    pub council_act: Rgba,
    /// Legacy ROW_HOVER paint role; its Dark value remains exact.
    pub row_hover: Rgba,
    /// Legacy ROW_ACTIVE paint role; its Dark value remains exact.
    pub row_active: Rgba,
    /// Legacy SEARCH_HIGHLIGHT paint role; its Dark value remains exact.
    pub search_highlight: Rgba,
    /// Legacy SELECTION paint role; its Dark value remains exact.
    pub selection: Rgba,
    /// Legacy SELECTION_ON_ACCENT paint role; its Dark value remains exact.
    pub selection_on_accent: Rgba,
    /// Legacy ACCENT paint role; its Dark value remains exact.
    pub accent: Rgba,
    /// Legacy accent foreground, independently legible on Light surfaces.
    pub accent_text: Rgba,
    /// Code Copy hover on the blue user bubble.
    pub copy_hover_on_accent: Rgba,
    /// Legacy user header Copy foreground and hover.
    pub user_copy_text: Rgba,
    pub user_copy_hover: Rgba,
    /// Legacy ACCENT_BRIGHT paint role; its Dark value remains exact.
    pub accent_bright: Rgba,
    /// Legacy ACCENT_SECONDARY paint role; its Dark value remains exact.
    pub accent_secondary: Rgba,
    /// Legacy THINKING_HALO paint role; its Dark value remains exact.
    pub thinking_halo: Rgba,
    /// Legacy SCROLLBAR_TRACK paint role; its Dark value remains exact.
    pub scrollbar_track: Rgba,
    /// Legacy SCROLLBAR_THUMB paint role; its Dark value remains exact.
    pub scrollbar_thumb: Rgba,
    /// Legacy LINK paint role; its Dark value remains exact.
    pub link: Rgba,
    /// Legacy LINK_ON_ACCENT paint role; its Dark value remains exact.
    pub link_on_accent: Rgba,
    /// Legacy QUOTE_BAR paint role; its Dark value remains exact.
    pub quote_bar: Rgba,
}

/// The dark palette — the only mode until slice 11 lands light.
pub static DARK: Palette = Palette {
    legacy_focus_ring: ACCENT_BRIGHT,
    ground: GROUND,
    canvas: CANVAS,
    panel: PANEL,
    panel_solid: PANEL_SOLID,
    raise: RAISE,
    line: LINE,
    control_line: CONTROL_LINE,
    fill_ghost: FILL_GHOST,
    fill_hover: FILL_HOVER,
    highlight: HIGHLIGHT,
    transparent: TRANSPARENT,
    text: TEXT,
    text_2: TEXT_2,
    text_dim: TEXT_DIM,
    contrast: CONTRAST,
    on_contrast: ON_CONTRAST,
    identity: COUNCIL_LOOK,
    contrast_text: CONTRAST_TEXT,
    contrast_soft: CONTRAST_SOFT,
    contrast_hover: CONTRAST_HOVER,
    contrast_soft_hover: CONTRAST_SOFT_HOVER,
    attention: ATTENTION,
    attention_soft: ATTENTION_SOFT,
    attention_edge: ATTENTION_EDGE,
    act_now: ACT_NOW,
    act_now_text: ACT_NOW_TEXT,
    act_now_soft: ACT_NOW_SOFT,
    act_now_edge: ACT_NOW_EDGE,
    scrim: SCRIM,
    thinking_ring: THINKING_RING,
    contrast_ring: ACCENT_BRIGHT,
    selection_fill: color_a(BRAND_BLUE, 0.05),
    selection_edge: color_a(BRAND_BLUE, 0.45),
    code_bg: CODE_BG,
    inline_code_bg: INLINE_CODE_BG,
    code_border: CODE_BORDER,
    bg: BG,
    chrome: CHROME,
    raised: RAISED,
    bubble_assistant: BUBBLE_ASSISTANT,
    border: BORDER,
    border_input: BORDER_INPUT,
    muted: MUTED,
    faint: FAINT,
    on_accent: ON_ACCENT,
    chip_bg: CHIP_BG,
    chip_text: CHIP_TEXT,
    chip_border: CHIP_BORDER,
    chip_warn: CHIP_WARN,
    sev_critical: SEV_CRITICAL,
    sev_attention: SEV_ATTENTION,
    council_live: COUNCIL_LIVE,
    council_look: COUNCIL_LOOK,
    council_act: COUNCIL_ACT,
    row_hover: ROW_HOVER,
    row_active: ROW_ACTIVE,
    search_highlight: SEARCH_HIGHLIGHT,
    selection: SELECTION,
    selection_on_accent: SELECTION_ON_ACCENT,
    accent: ACCENT,
    accent_text: ACCENT_BRIGHT,
    copy_hover_on_accent: TEXT,
    user_copy_text: ON_ACCENT,
    user_copy_hover: ON_ACCENT,
    accent_bright: ACCENT_BRIGHT,
    accent_secondary: ACCENT_SECONDARY,
    thinking_halo: THINKING_HALO,
    scrollbar_track: SCROLLBAR_TRACK,
    scrollbar_thumb: SCROLLBAR_THUMB,
    link: LINK,
    link_on_accent: LINK_ON_ACCENT,
    quote_bar: QUOTE_BAR,
};

/// The Light palette: warm surfaces, dark ink and legible semantic signals.
pub static LIGHT: Palette = Palette {
    legacy_focus_ring: rgb(0x245aa4),
    ground: rgb(0xe7e3da),
    canvas: rgb(0xf5f2eb),
    panel: color_a(rgb(0xfffcf5), 0.94),
    panel_solid: rgb(0xfffcf5),
    raise: rgb(0xe9e3d7),
    line: black_a(0.10),
    control_line: black_a(0.55),
    fill_ghost: black_a(0.05),
    fill_hover: black_a(0.09),
    highlight: black_a(0.04),
    transparent: TRANSPARENT,
    text: rgb(0x202a30),
    text_2: rgb(0x485358),
    text_dim: rgb(0x586268),
    contrast: BRAND_BLUE,
    on_contrast: ON_ACCENT,
    identity: rgb(0x685095),
    contrast_text: rgb(0x245aa4),
    contrast_soft: color_a(BRAND_BLUE, 0.10),
    contrast_hover: CONTRAST_HOVER,
    contrast_soft_hover: color_a(BRAND_BLUE, 0.16),
    attention: rgb(0x24633e),
    attention_soft: color_a(BRAND_SAGE, 0.12),
    attention_edge: rgb(0x24633e),
    act_now: rgb(0xb2362d),
    act_now_text: rgb(0xb2362d),
    act_now_soft: color_a(rgb(0xb2362d), 0.08),
    act_now_edge: rgb(0xb2362d),
    scrim: SCRIM,
    thinking_ring: THINKING_RING,
    contrast_ring: ACCENT_BRIGHT,
    selection_fill: color_a(BRAND_BLUE, 0.10),
    selection_edge: color_a(BRAND_BLUE, 0.70),
    code_bg: black_a(0.04),
    inline_code_bg: black_a(0.02),
    code_border: black_a(0.14),
    bg: rgb(0xf5f2eb),
    chrome: rgb(0xe7e3da),
    raised: rgb(0xe9e3d7),
    bubble_assistant: rgb(0xfffcf5),
    border: black_a(0.10),
    border_input: black_a(0.55),
    muted: rgb(0x586268),
    faint: rgb(0x586268),
    on_accent: ON_ACCENT,
    chip_bg: rgb(0xe9e3d7),
    chip_text: rgb(0x485358),
    chip_border: black_a(0.55),
    chip_warn: rgb(0x805300),
    sev_critical: rgb(0xb2362d),
    sev_attention: rgb(0x805300),
    council_live: rgb(0x24633e),
    council_look: rgb(0x685095),
    council_act: rgb(0xb2362d),
    row_hover: black_a(0.05),
    row_active: color_a(BRAND_BLUE, 0.10),
    search_highlight: color_a(BRAND_BLUE, 0.20),
    selection: color_a(BRAND_BLUE, 0.18),
    selection_on_accent: SELECTION_ON_ACCENT,
    accent: BRAND_BLUE,
    accent_text: rgb(0x245aa4),
    copy_hover_on_accent: ON_ACCENT,
    user_copy_text: ON_ACCENT,
    user_copy_hover: ON_ACCENT,
    accent_bright: rgb(0x245aa4),
    accent_secondary: rgb(0x24633e),
    thinking_halo: THINKING_HALO,
    scrollbar_track: TRANSPARENT,
    scrollbar_thumb: black_a(0.42),
    link: rgb(0x245aa4),
    link_on_accent: LINK_ON_ACCENT,
    quote_bar: rgb(0x24633e),
};

/// Whether the app paints a backdrop blur behind floating faces. The
/// translucent `PANEL` only reads as glass over a blur; until the app wires
/// one (a later slice), every floating face uses the opaque `PANEL_SOLID`
/// (KIT.md § Surfaces, the solid fallback).
pub const BLUR_AVAILABLE: bool = false;

impl Palette {
    /// The fill of a floating face (Panel, Pill, Popover, Menu, Palette,
    /// Dialog, Sheet, Notice): `PANEL` where the app can blur behind it,
    /// else `PANEL_SOLID`.
    pub fn float_fill(&self) -> Rgba {
        if BLUR_AVAILABLE {
            self.panel
        } else {
            self.panel_solid
        }
    }
}

#[cfg(not(test))]
static LIGHT_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
thread_local! {
    static LIGHT_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Change the effective palette. Preference/persistence and notification belong to the app.
pub fn set_light_mode(light: bool) -> bool {
    #[cfg(not(test))]
    {
        LIGHT_MODE.swap(light, Ordering::Relaxed) != light
    }
    #[cfg(test)]
    {
        LIGHT_MODE.with(|v| v.replace(light) != light)
    }
}

pub fn is_light_mode() -> bool {
    #[cfg(not(test))]
    {
        LIGHT_MODE.load(Ordering::Relaxed)
    }
    #[cfg(test)]
    {
        LIGHT_MODE.with(|v| v.get())
    }
}

/// The effective immutable palette. GUI tests keep overrides thread-local like zoom.
pub fn palette() -> &'static Palette {
    if is_light_mode() {
        &LIGHT
    } else {
        &DARK
    }
}

/// Runtime palette accessor used by migrating app paint consumers (PLAN 11a).
pub fn p() -> &'static Palette {
    palette()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DESIGN.md §9.1: every design value lives in theme.rs. The kit's files
    /// (`src/ui/*.rs`) are parsed with `syn`, and every `const` of a number
    /// type (any integer or float, `Pixels`, `Duration`, or an array of them,
    /// parenthesised or not) outside a `#[cfg(test)]` item must not be given
    /// a literal value: a number (signed, parenthesised, cast or combined
    /// with other numbers), a numeric array or repeat, `px(…)` or
    /// `Duration::from_*(…)` of numbers. It must alias a theme token.
    /// Comments, visibility and attributes cannot hide one (it is a parse,
    /// not a text scan). Values computed from tokens are design-lint's
    /// `token_math`; inline literals are its `raw_px` / `raw_z`.
    fn kit_local_size_constants(dir: &std::path::Path) -> Vec<String> {
        use syn::visit::Visit;

        const NUMBER_TYPES: [&str; 16] = [
            "f32", "f64", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
            "i128", "isize", "Pixels", "Duration",
        ];

        fn cfg_test(attrs: &[syn::Attribute]) -> bool {
            attrs.iter().any(|a| {
                a.path().is_ident("cfg")
                    && a.parse_args::<syn::Meta>()
                        .is_ok_and(|m| m.path().is_ident("test"))
            })
        }

        fn numeric_type(ty: &syn::Type) -> bool {
            match ty {
                syn::Type::Paren(p) => numeric_type(&p.elem),
                syn::Type::Group(g) => numeric_type(&g.elem),
                syn::Type::Array(a) => numeric_type(&a.elem),
                syn::Type::Path(p) => p
                    .path
                    .segments
                    .last()
                    .is_some_and(|s| NUMBER_TYPES.contains(&s.ident.to_string().as_str())),
                _ => false,
            }
        }

        fn literal(e: &syn::Expr) -> bool {
            match e {
                syn::Expr::Lit(l) => matches!(l.lit, syn::Lit::Int(_) | syn::Lit::Float(_)),
                syn::Expr::Unary(u) => matches!(u.op, syn::UnOp::Neg(_)) && literal(&u.expr),
                syn::Expr::Paren(p) => literal(&p.expr),
                syn::Expr::Group(g) => literal(&g.expr),
                syn::Expr::Cast(c) => literal(&c.expr),
                syn::Expr::Binary(b) => literal(&b.left) && literal(&b.right),
                syn::Expr::Array(a) => !a.elems.is_empty() && a.elems.iter().all(literal),
                syn::Expr::Repeat(r) => literal(&r.expr),
                syn::Expr::Call(c) => {
                    // The callee, through any parentheses or groups around it.
                    let mut func = c.func.as_ref();
                    loop {
                        match func {
                            syn::Expr::Paren(p) => func = &p.expr,
                            syn::Expr::Group(g) => func = &g.expr,
                            _ => break,
                        }
                    }
                    let callee = match func {
                        syn::Expr::Path(p) => p
                            .path
                            .segments
                            .iter()
                            .map(|s| s.ident.to_string())
                            .collect::<Vec<_>>(),
                        _ => return false,
                    };
                    let last = callee.last().map(String::as_str).unwrap_or("");
                    let px = last == "px";
                    let duration = last.starts_with("from_")
                        && callee.iter().rev().nth(1).is_some_and(|s| s == "Duration");
                    (px || duration) && !c.args.is_empty() && c.args.iter().all(literal)
                }
                _ => false,
            }
        }

        struct Scan<'a> {
            file: String,
            offenders: &'a mut Vec<String>,
            module_dir: std::path::PathBuf,
            path_dir: std::path::PathBuf,
            test_only: bool,
            visited: &'a mut std::collections::HashSet<(std::path::PathBuf, bool, bool)>,
        }
        impl Scan<'_> {
            fn check(
                &mut self,
                attrs: &[syn::Attribute],
                name: &syn::Ident,
                ty: &syn::Type,
                e: &syn::Expr,
            ) {
                if !self.test_only && !cfg_test(attrs) && numeric_type(ty) && literal(e) {
                    self.offenders.push(format!("{}: const {name}", self.file));
                }
            }
        }
        impl<'ast> Visit<'ast> for Scan<'_> {
            fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
                let old_dir = self.module_dir.clone();
                let old_test_only = self.test_only;
                let old_path_dir = self.path_dir.clone();
                self.test_only |= cfg_test(&m.attrs);
                let explicit_path = m.attrs.iter().find_map(|a| {
                    if !a.path().is_ident("path") {
                        return None;
                    }
                    match &a.meta {
                        syn::Meta::NameValue(v) => match &v.value {
                            syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(s),
                                ..
                            }) => Some(self.path_dir.join(s.value())),
                            _ => None,
                        },
                        _ => None,
                    }
                });
                if m.content.is_some() {
                    self.module_dir =
                        explicit_path.unwrap_or_else(|| self.module_dir.join(m.ident.to_string()));
                    self.path_dir = self.module_dir.clone();
                    syn::visit::visit_item_mod(self, m);
                } else {
                    let name = m.ident.to_string();
                    let via_path = explicit_path.is_some();
                    let path = explicit_path.unwrap_or_else(|| {
                        let flat = self.module_dir.join(format!("{name}.rs"));
                        if flat.exists() {
                            flat
                        } else {
                            self.module_dir.join(name).join("mod.rs")
                        }
                    });
                    scan_file(
                        &path,
                        self.test_only,
                        via_path,
                        self.offenders,
                        self.visited,
                    );
                }
                self.module_dir = old_dir;
                self.path_dir = old_path_dir;
                self.test_only = old_test_only;
            }
            fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
                if !cfg_test(&f.attrs) {
                    syn::visit::visit_item_fn(self, f);
                }
            }
            fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
                if !cfg_test(&i.attrs) {
                    syn::visit::visit_item_impl(self, i);
                }
            }
            fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
                if !cfg_test(&t.attrs) {
                    syn::visit::visit_item_trait(self, t);
                }
            }
            fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
                if !cfg_test(&f.attrs) {
                    syn::visit::visit_impl_item_fn(self, f);
                }
            }
            fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
                if !cfg_test(&f.attrs) {
                    syn::visit::visit_trait_item_fn(self, f);
                }
            }
            // An exempt (test-only) constant is skipped whole, its value
            // included.
            fn visit_item_const(&mut self, c: &'ast syn::ItemConst) {
                if !cfg_test(&c.attrs) {
                    self.check(&c.attrs, &c.ident, &c.ty, &c.expr);
                    syn::visit::visit_item_const(self, c);
                }
            }
            fn visit_impl_item_const(&mut self, c: &'ast syn::ImplItemConst) {
                if !cfg_test(&c.attrs) {
                    self.check(&c.attrs, &c.ident, &c.ty, &c.expr);
                    syn::visit::visit_impl_item_const(self, c);
                }
            }
            fn visit_trait_item_const(&mut self, c: &'ast syn::TraitItemConst) {
                if !cfg_test(&c.attrs) {
                    if let Some((_, e)) = &c.default {
                        self.check(&c.attrs, &c.ident, &c.ty, e);
                    }
                    syn::visit::visit_trait_item_const(self, c);
                }
            }
        }

        fn scan_file(
            path: &std::path::Path,
            test_only: bool,
            via_path: bool,
            offenders: &mut Vec<String>,
            visited: &mut std::collections::HashSet<(std::path::PathBuf, bool, bool)>,
        ) {
            let path = path
                .canonicalize()
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            // A shared source may be imported by both a test-only module and
            // production code. Check the production import even if tests saw it first.
            if !visited.insert((path.clone(), test_only, via_path)) {
                return;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let file = syn::parse_file(&text)
                .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
            let path_dir = path.parent().unwrap().to_path_buf();
            let mut module_dir = path_dir.clone();
            // Rust treats #[path] sources as owning their containing directory.
            if !via_path && path.file_name().unwrap() != "mod.rs" {
                module_dir.push(path.file_stem().unwrap());
            }
            Scan {
                file: path.display().to_string(),
                offenders,
                module_dir,
                path_dir,
                test_only: test_only || cfg_test(&file.attrs),
                visited,
            }
            .visit_file(&file);
        }

        fn scan_unvisited(
            dir: &std::path::Path,
            offenders: &mut Vec<String>,
            visited: &mut std::collections::HashSet<(std::path::PathBuf, bool, bool)>,
        ) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan_unvisited(&path, offenders, visited);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let canonical = path.canonicalize().unwrap();
                    if !visited.iter().any(|(p, _, _)| p == &canonical) {
                        scan_file(&path, false, false, offenders, visited);
                    }
                }
            }
        }

        let mut offenders = Vec::new();
        let mut visited = std::collections::HashSet::new();
        scan_file(
            &dir.join("mod.rs"),
            false,
            false,
            &mut offenders,
            &mut visited,
        );
        // Undeclared files still count as kit source; a filename alone never
        // makes a test exemption. Only a module's actual attributes do.
        scan_unvisited(dir, &mut offenders, &mut visited);
        offenders
    }

    #[test]
    fn kit_parts_define_no_local_size_constants() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
        let offenders = kit_local_size_constants(&dir);
        assert!(
            offenders.is_empty(),
            "local design constants in kit parts:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn kit_constant_scan_inherits_out_of_line_test_exemptions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("mod.rs"), "#[cfg(test)] mod fixtures; mod part;").unwrap();
        std::fs::write(
            root.join("fixtures.rs"),
            "const FIXTURE: f32 = 12.5; mod nested;",
        )
        .unwrap();
        std::fs::create_dir(root.join("fixtures")).unwrap();
        std::fs::write(
            root.join("fixtures/nested.rs"),
            "const NESTED_FIXTURE: u8 = 12;",
        )
        .unwrap();
        std::fs::write(
            root.join("part.rs"),
            "const WIDTH: f32 = std::f32::consts::PI;",
        )
        .unwrap();
        assert!(
            kit_local_size_constants(root).is_empty(),
            "test modules inherit the exemption"
        );
        std::fs::write(root.join("part.rs"), "const WIDTH: f32 = 12.5;").unwrap();
        let violations = kit_local_size_constants(root);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].ends_with("const WIDTH"));
        // Conventional and explicit imports of shared source have different
        // child directories. Both layouts must compile and be checked.
        std::fs::write(root.join("nested.rs"), "const PRODUCTION_NESTED: u8 = 12;").unwrap();
        // The same source referenced by production must still be checked.
        std::fs::write(
            root.join("mod.rs"),
            "#[cfg(test)] mod fixtures; #[path = \"fixtures.rs\"] mod production; mod part;",
        )
        .unwrap();
        let compiled = std::process::Command::new("rustc")
            .args(["--crate-type", "lib", "--cfg", "test", "--emit", "metadata"])
            .arg(root.join("mod.rs"))
            .arg("-o")
            .arg(root.join("layout.rmeta"))
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "fixture must compile: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let violations = kit_local_size_constants(root);
        assert_eq!(
            violations.len(),
            3,
            "shared source is not exempt in production"
        );
    }

    #[test]
    fn kit_constant_scan_resolves_explicit_paths_and_unreferenced_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("mod.rs"), "mod part;").unwrap();
        std::fs::write(
            root.join("part.rs"),
            "#[cfg(test)] #[path = \"fixtures.rs\"] mod fixtures;",
        )
        .unwrap();
        std::fs::write(root.join("fixtures.rs"), "const FIXTURE: f32 = 12.5;").unwrap();
        std::fs::write(
            root.join("file_test.rs"),
            "#![cfg(test)] const FIXTURE: f32 = 12.5;",
        )
        .unwrap();
        assert!(kit_local_size_constants(root).is_empty());
        std::fs::write(root.join("orphan.rs"), "const WIDTH: f32 = 12.5;").unwrap();
        let violations = kit_local_size_constants(root);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].ends_with("const WIDTH"));
        std::fs::write(
            root.join("part.rs"),
            "#[path = \"fixtures.rs\"] mod fixtures;",
        )
        .unwrap();
        assert_eq!(kit_local_size_constants(root).len(), 2);
    }

    // `rgb()` is the const hex decoder EVERY opaque theme color flows through;
    // a wrong shift or mask silently miscolors the whole instrument panel and
    // no view test would catch it (the views only read the named tokens). Pin
    // the R/G/B byte order through two real brand colors, with each channel
    // expectation written as an independent integer fraction (NOT the `>>`/`&`
    // formula) so a shift-swap or mask bug is caught, not restated.
    #[test]
    fn rgb_decodes_hex_channels_in_r_g_b_byte_order() {
        // ACCENT = BRAND_BLUE = rgb(0x2563eb) — Awili blue (white-text-safe fill).
        assert_eq!(ACCENT.r, 37.0 / 255.0, "red is the high byte 0x25");
        assert_eq!(ACCENT.g, 99.0 / 255.0, "green is the mid byte 0x63");
        assert_eq!(ACCENT.b, 235.0 / 255.0, "blue is the low byte 0xeb");
        assert_eq!(ACCENT.a, 1.0, "rgb() is fully opaque");

        // A second, differently-ordered color so a coincidental channel
        // symmetry in ACCENT can't hide a swapped shift.
        assert_eq!(ACCENT_BRIGHT.r, 96.0 / 255.0); // 0x60
        assert_eq!(ACCENT_BRIGHT.g, 165.0 / 255.0); // 0xa5
        assert_eq!(ACCENT_BRIGHT.b, 250.0 / 255.0); // 0xfa
    }

    // The five brand colors are contractual — `assets/brand/README.md` v1.0 is
    // the source of truth and the logo/icon artwork is drawn in these exact
    // values. Drifting a channel here desynchronises the app from the shipped
    // artwork, which no view test would notice. Pinned as whole hex words.
    #[test]
    fn brand_palette_matches_the_shipped_brand_pack() {
        fn hex(c: Rgba) -> u32 {
            ((c.r * 255.0).round() as u32) << 16
                | ((c.g * 255.0).round() as u32) << 8
                | ((c.b * 255.0).round() as u32)
        }
        assert_eq!(hex(BRAND_BLUE), 0x2563eb, "Awili blue");
        assert_eq!(hex(BRAND_SAGE), 0x6dba8e, "Observatory sage");
        assert_eq!(hex(BRAND_NAVY), 0x071a2e, "Deep navy");
        assert_eq!(hex(BRAND_BONE), 0xe6e6e6, "Observatory bone (neutral gray since the purple theme)");
        assert_eq!(hex(BRAND_CHARCOAL), 0x141414, "Observatory charcoal");
    }

    // The alpha helpers must vary ONLY alpha, keeping their base hue — a search
    // highlight is the accent at 30%, a hairline is white at low alpha, a code
    // fill is black at 27%. A wrong base channel in a helper tints the overlay.
    #[test]
    fn alpha_helpers_keep_base_hue_and_only_move_alpha() {
        // accent_a shares ACCENT's exact rgb, dimmed to 0.30.
        assert_eq!(
            (SEARCH_HIGHLIGHT.r, SEARCH_HIGHLIGHT.g, SEARCH_HIGHLIGHT.b),
            (ACCENT.r, ACCENT.g, ACCENT.b),
            "highlight is the accent hue"
        );
        assert_eq!(SEARCH_HIGHLIGHT.a, 0.30);

        // white_a is opaque-white channels at the requested alpha.
        assert_eq!((white_a(0.07).r, white_a(0.07).g, white_a(0.07).b), (1.0, 1.0, 1.0));
        assert_eq!(white_a(0.07).a, 0.07);
        assert_eq!(BORDER, LINE, "BORDER is the opaque neutral hairline");

        // black_a is zero channels at the requested alpha (reads on any bubble).
        assert_eq!((CODE_BG.r, CODE_BG.g, CODE_BG.b), (0.0, 0.0, 0.0));
        assert_eq!(CODE_BG.a, 0.27);
    }

    // `TRANSPARENT` is the token that replaced the five `gpui::rgba(0x00000000)`
    // call sites — it must be EXACTLY (0, 0, 0, 0), value-identical to the
    // original literal (docs/v2/slices/02b-lint-fixes.md item 10). A white
    // base at alpha 0 paints identically, but the value must be exact.
    #[test]
    fn transparent_is_exactly_rgba_0x00000000() {
        assert_eq!(TRANSPARENT, black_a(0.0), "value-identical to the literal");
        assert_eq!(TRANSPARENT.r, 0.0, "red channel is 0");
        assert_eq!(TRANSPARENT.g, 0.0, "green channel is 0");
        assert_eq!(TRANSPARENT.b, 0.0, "blue channel is 0");
        assert_eq!(TRANSPARENT.a, 0.0, "alpha channel is 0");
    }

    // The markdown heading scale is user-visible taste: strictly descending
    // h1>h2>h3, and h4+ (plus any out-of-range level) fall back to BODY size —
    // never to the tiny label size. A fallthrough or wrong fallback target
    // shrinks deep headings to unreadable captions with no view test failing.
    #[test]
    fn heading_size_descends_then_falls_back_to_body_not_label() {
        assert_eq!(heading_size(1), type_::TITLE.size);
        assert_eq!(heading_size(2), type_::HEADING.size);
        assert_eq!(heading_size(3), type_::READ.size);
        assert!(
            heading_size(1) > heading_size(2) && heading_size(2) > heading_size(3),
            "the heading scale must strictly descend"
        );
        for lvl in [4u8, 5, 6, 0] {
            assert_eq!(
                heading_size(lvl),
                type_::READ.size,
                "h{lvl} renders at body size"
            );
        }
        const {
            assert!(
                TEXT_SIZE > LABEL_SIZE,
                "the body fallback must not be the label size"
            )
        };
    }

    // v2 (slice 1): the signal tokens ARE the brand colors — the owner rule is
    // that CONTRAST (brand blue) carries primary action / selection / focus /
    // highlight, ATTENTION (brand sage) carries non-urgent attention, and the
    // ACT_NOW red is reserved for act-now and nothing else. Repointing any of
    // these at a fresh hex would desynchronise v2 from the brand pack, which
    // no view test would notice.
    #[test]
    fn v2_signal_tokens_follow_the_brand() {
        assert_eq!(CONTRAST, BRAND_BLUE, "CONTRAST must be the brand blue");
        assert_eq!(ATTENTION, BRAND_SAGE, "ATTENTION must be the brand sage");
        assert_eq!(
            ON_CONTRAST, ON_ACCENT,
            "ON_CONTRAST must be the on-accent ink"
        );

        fn hex(c: Rgba) -> u32 {
            ((c.r * 255.0).round() as u32) << 16
                | ((c.g * 255.0).round() as u32) << 8
                | ((c.b * 255.0).round() as u32)
        }
        assert_eq!(hex(ACT_NOW), 0xd4574a, "ACT_NOW must be 0xd4574a");

        assert_eq!(
            (CONTRAST_SOFT.r, CONTRAST_SOFT.g, CONTRAST_SOFT.b),
            (BRAND_BLUE.r, BRAND_BLUE.g, BRAND_BLUE.b),
            "CONTRAST_SOFT keeps the brand blue hue"
        );
        assert_eq!(
            CONTRAST_SOFT.a, 0.14,
            "CONTRAST_SOFT must be that hue at a = 0.14"
        );
    }

    // v2 (slice 1): every body-text token must clear WCAG AA (4.5:1) on its two
    // main surfaces, and the on-contrast ink must clear it on the brand blue.
    // The math is WCAG's own: linearise each sRGB channel
    // (c <= 0.04045 ? c/12.92 : ((c + 0.055)/1.055)^2.4), L =
    // 0.2126·R + 0.7152·G + 0.0722·B, ratio = (L_lighter + 0.05)/(L_darker + 0.05).
    #[test]
    fn v2_text_tokens_meet_wcag_aa() {
        fn linear(c: f32) -> f32 {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        fn luminance(c: Rgba) -> f32 {
            0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
        }
        fn ratio(a: Rgba, b: Rgba) -> f32 {
            let (l1, l2) = (luminance(a), luminance(b));
            let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
            (hi + 0.05) / (lo + 0.05)
        }

        for (fg, bg, name) in [
            (TEXT, CANVAS, "TEXT on CANVAS"),
            (TEXT_2, CANVAS, "TEXT_2 on CANVAS"),
            (TEXT_DIM, CANVAS, "TEXT_DIM on CANVAS"),
            (TEXT, PANEL_SOLID, "TEXT on PANEL_SOLID"),
            (TEXT_2, PANEL_SOLID, "TEXT_2 on PANEL_SOLID"),
            (TEXT_DIM, PANEL_SOLID, "TEXT_DIM on PANEL_SOLID"),
            (ON_CONTRAST, CONTRAST, "ON_CONTRAST on CONTRAST"),
            (ON_CONTRAST, CONTRAST_HOVER, "ON_CONTRAST on CONTRAST_HOVER"),
        ] {
            let r = ratio(fg, bg);
            assert!(
                r >= 4.5,
                "{name} measures {r:.3}:1, below the WCAG AA 4.5:1 floor"
            );
        }
    }

    // v2 (slice 1): the new spacing ladder sits on a 4 px grid — off-grid steps
    // would break the vertical rhythm without any view test noticing.
    #[test]
    fn v2_spacing_is_on_a_4px_grid() {
        for sp in [SP_4, SP_8, SP_12, SP_16, SP_20, SP_24, SP_32, SP_40, SP_56] {
            assert!((sp % 4.0).abs() < f32::EPSILON, "{sp} is off the 4 px grid");
        }
    }

    // v2 (slice 1): the type scale strictly ascends CAPTION → TITLE and every
    // line height clears its own size — a collapse or inversion in the ladder
    // would clip dense rows without any view test noticing.
    #[test]
    fn v2_type_scale_ascends() {
        let styles = [
            type_::CAPTION,
            type_::LABEL,
            type_::UI,
            type_::BODY,
            type_::READ,
            type_::HEADING,
            type_::TITLE,
        ];
        for pair in styles.windows(2) {
            assert!(
                pair[0].size < pair[1].size,
                "the type scale must strictly ascend: {} then {}",
                pair[0].size,
                pair[1].size
            );
        }
        for style in styles {
            assert!(
                style.line > style.size,
                "line height {} must clear its own size {}",
                style.line,
                style.size
            );
        }
    }

    // v2 (slice 1): the hit-area floor — `target()` must never lay out below
    // 44 px on screen: zooming out (0.5) clamps to 44, zooming in (1.25) grows
    // 44 to 55. The test scale is thread-local (see the module docs), so
    // reset it to 1.0 on the way out.
    #[test]
    fn v2_target_never_below_44() {
        set_ui_scale(0.5);
        assert!(
            target() >= px(44.0),
            "zooming out to 0.5 must not shrink target() below 44 px on screen"
        );
        set_ui_scale(1.25);
        assert_eq!(
            target(),
            px(55.0),
            "at 1.25 scale target() is 55 px on screen"
        );
        set_ui_scale(1.0);
    }

    // WCAG relative luminance and contrast ratio (the same math as the
    // slice-1 test), plus the "composited where translucent" rule: a
    // translucent fill or hairline is tested against what it actually sits
    // on, alpha-composited over its surface.
    mod wcag {
        use gpui::Rgba;

        pub fn linear(c: f32) -> f32 {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }

        pub fn luminance(c: Rgba) -> f32 {
            0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
        }

        pub fn ratio(a: Rgba, b: Rgba) -> f32 {
            let (l1, l2) = (luminance(a), luminance(b));
            let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
            (hi + 0.05) / (lo + 0.05)
        }

        /// Alpha-composite translucent `fg` over opaque `bg`.
        pub fn over(fg: Rgba, bg: Rgba) -> Rgba {
            Rgba {
                r: fg.r * fg.a + bg.r * (1.0 - fg.a),
                g: fg.g * fg.a + bg.g * (1.0 - fg.a),
                b: fg.b * fg.a + bg.b * (1.0 - fg.a),
                a: 1.0,
            }
        }
    }

    // K1 (KIT.md v3): the owner-ruled value changes and the new tokens are
    // pinned to the spec's exact values — drifting one channel desynchronises
    // the kit from the contrast floors the K1 tests assert below.
    #[test]
    fn v2_k1_tokens_are_the_spec_values() {
        fn hex(c: Rgba) -> u32 {
            ((c.r * 255.0).round() as u32) << 16
                | ((c.g * 255.0).round() as u32) << 8
                | ((c.b * 255.0).round() as u32)
        }
        assert_eq!(hex(TEXT_DIM), 0xa0a0a0, "TEXT_DIM must be #a0a0a0");
        assert_eq!(
            CONTROL_LINE,
            rgb(0x555555),
            "CONTROL_LINE must be opaque #555555"
        );
        assert_eq!(
            CONTRAST_TEXT, ACCENT_BRIGHT,
            "CONTRAST_TEXT must be ACCENT_BRIGHT #60a5fa"
        );
        assert_eq!(
            ATTENTION_EDGE,
            BRAND_SAGE,
            "ATTENTION_EDGE must be opaque brand sage"
        );
        assert_eq!(
            ACT_NOW_EDGE,
            ACT_NOW,
            "ACT_NOW_EDGE must be the opaque act-now red"
        );
    }

    #[test]
    fn runtime_palettes_keep_semantic_text_and_controls_legible() {
        for (name, p) in [("Dark", &DARK), ("Light", &LIGHT)] {
            let panel = wcag::over(p.panel, p.canvas);
            for (surface, bg) in [
                ("Canvas", p.canvas),
                ("Panel solid", p.panel_solid),
                ("Raise", p.raise),
                ("Panel composite", panel),
            ] {
                for (role, fg) in [
                    ("Text", p.text),
                    ("Text 2", p.text_2),
                    ("Dim", p.text_dim),
                    ("Contrast", p.contrast_text),
                    ("Act now", p.act_now_text),
                ] {
                    assert!(
                        wcag::ratio(fg, bg) >= 4.5,
                        "{name} {role} on {surface}: {}",
                        wcag::ratio(fg, bg)
                    );
                }
                // Council 2026-11-01: dark lines are crisp opaque neutrals. The dark floor
                // is 2.0:1 (owner-palette trade vs the WCAG 3:1 non-text guidance;
                // #555555 is the lowest neutral reaching 2:1 on #202020 and #262626).
                assert!(
                    wcag::ratio(wcag::over(p.control_line, bg), bg) >= if name == "Dark" { 2.0 } else { 3. },
                    "{name} control on {surface}"
                );
            }
            for base in [p.panel_solid, panel] {
                for (role, fill, ink) in [
                    ("Hover", p.fill_hover, p.text_2),
                    ("Contrast soft", p.contrast_soft, p.contrast_text),
                    (
                        "Contrast soft hover",
                        p.contrast_soft_hover,
                        p.contrast_text,
                    ),
                    ("Attention soft", p.attention_soft, p.attention),
                    ("Act now soft", p.act_now_soft, p.act_now_text),
                ] {
                    let bg = wcag::over(fill, base);
                    for fg in [p.text, p.text_2, ink] {
                        assert!(
                            wcag::ratio(fg, bg) >= 4.5,
                            "{name} on {role}: {}",
                            wcag::ratio(fg, bg)
                        );
                    }
                }
            }
            assert!(wcag::ratio(p.on_contrast, p.contrast) >= 4.5);
            assert!(
                wcag::ratio(p.on_accent, p.contrast) >= 4.5,
                "{name}: user-bubble ink on the blue fill"
            );
            assert!(p.inline_code_bg.a < p.code_bg.a);
        }
    }

    #[test]
    fn runtime_light_legacy_foreground_text_has_readable_contrast() {
        let p = &LIGHT;
        for bg in [
            p.canvas,
            p.panel_solid,
            p.raise,
            wcag::over(p.panel, p.canvas),
        ] {
            for fill in [p.transparent, p.row_hover, p.row_active] {
                let surface = wcag::over(fill, bg);
                for (role, ink) in [
                    ("Legacy accent text", p.accent_text),
                    ("Legacy sage text", p.accent_secondary),
                ] {
                    assert!(
                        wcag::ratio(ink, surface) >= 4.5,
                        "{role}: {}",
                        wcag::ratio(ink, surface)
                    );
                }
            }
        }
        assert_eq!(DARK.accent_text, ACCENT_BRIGHT);
        assert_eq!(DARK.accent_secondary, ACCENT_SECONDARY);
        assert!(
            wcag::ratio(p.text, p.chrome) >= 3.,
            "Actual off-switch knob (TEXT) remains visible"
        );
    }

    #[test]
    fn runtime_light_actual_status_and_copy_foregrounds() {
        let old = is_light_mode();
        set_light_mode(true);
        let p = palette();
        let parts = crate::status_run_parts(
            None,
            Some("Estimated cost".into()),
            None,
            None,
            true,
            Some(true),
        );
        for (label, ink) in parts {
            for bg in [p.canvas, p.raise, p.raised] {
                assert!(
                    wcag::ratio(ink, bg) >= 4.5,
                    "Actual status {label}: {}",
                    wcag::ratio(ink, bg)
                );
            }
        }
        let assistant = crate::markdown_plan::assistant();
        let user = crate::markdown_plan::user();
        assert!(
            wcag::ratio(
                assistant.copy_hover,
                wcag::over(p.code_bg, p.bubble_assistant)
            ) >= 4.5,
            "Actual assistant Copy hover remains readable"
        );
        assert!(
            wcag::ratio(user.copy_hover, wcag::over(p.code_bg, p.accent)) >= 4.5,
            "Actual user Copy hover remains readable"
        );
        set_light_mode(old);
    }

    // K1 (KIT.md v3 § Tests): the spec's contrast contract, composited where
    // translucent —
    // - every text token (TEXT, TEXT_2, TEXT_DIM, CONTRAST_TEXT) ≥ 4.5:1 on
    //   CANVAS, PANEL_SOLID, RAISE and the PANEL-over-CANVAS composite;
    // - CONTRAST_TEXT and ACT_NOW_TEXT ≥ 4.5:1 on the same four surfaces;
    // - CONTROL_LINE ≥ 3:1 (the non-text UI floor) on the same four surfaces.
    #[test]
    fn v2_k1_contrast_on_every_real_combination() {
        // The spec's four surfaces (PANEL composited over CANVAS, since it
        // is translucent).
        let surfaces: [(&str, Rgba); 4] = [
            ("CANVAS", CANVAS),
            ("PANEL_SOLID", PANEL_SOLID),
            ("RAISE", RAISE),
            ("PANEL over CANVAS", wcag::over(PANEL, CANVAS)),
        ];

        // Every text token ≥ 4.5:1 (WCAG AA, normal text) on every surface.
        for (fg_name, fg) in [
            ("TEXT", TEXT),
            ("TEXT_2", TEXT_2),
            ("TEXT_DIM", TEXT_DIM),
            ("CONTRAST_TEXT", CONTRAST_TEXT),
        ] {
            for (bg_name, bg) in surfaces {
                assert!(
                    wcag::ratio(fg, bg) >= 4.5,
                    "{fg_name} on {bg_name} measures {:.3}:1, below 4.5:1",
                    wcag::ratio(fg, bg)
                );
            }
        }

        // CONTRAST_TEXT and ACT_NOW_TEXT ≥ 4.5:1 on every surface.
        for (fg_name, fg) in [
            ("CONTRAST_TEXT", CONTRAST_TEXT),
            ("ACT_NOW_TEXT", ACT_NOW_TEXT),
        ] {
            for (bg_name, bg) in surfaces {
                assert!(
                    wcag::ratio(fg, bg) >= 4.5,
                    "{fg_name} on {bg_name} measures {:.3}:1, below 4.5:1",
                    wcag::ratio(fg, bg)
                );
            }
        }

        // CONTROL_LINE ≥ 3:1 (the non-text UI floor) on every surface.
        // CONTROL_LINE is opaque neutral now (#555555); composite anyway over
        // each surface first — that is the line's apparent colour there
        // (the spec's "composited where translucent").
        for (bg_name, bg) in surfaces {
            let line = wcag::over(CONTROL_LINE, bg);
            assert!(
                wcag::ratio(line, bg) >= 2.0,
                "CONTROL_LINE on {bg_name} measures {:.3}:1, below the 2.0:1 dark floor (owner-palette trade vs WCAG 3:1)",
                wcag::ratio(line, bg)
            );
        }
    }

    // K1b (Codex + DeepSeek K1 review): the combinations the kit actually
    // produces beyond the four base surfaces, composited —
    // - a hovered row: FILL_HOVER over PANEL_SOLID, and over PANEL-on-CANVAS
    //   (a selected RAISE row adds no hover fill, KIT.md § Tokens);
    // - each soft fill (CONTRAST_SOFT, ATTENTION_SOFT, ACT_NOW_SOFT) over
    //   PANEL_SOLID (tone cards, lead buttons, banners) and over the
    //   PANEL-on-CANVAS composite (the lead button inside a pill).
    // On these, primary and reading text (TEXT, TEXT_2) and the tone's own
    // text colour must hold ≥ 4.5:1. TEXT_DIM does NOT hold on soft fills —
    // the rule (KIT.md) is that secondary text on a soft fill uses TEXT_2,
    // and this test pins both halves so the rule cannot silently rot.
    #[test]
    fn v2_k1b_contrast_on_hover_and_soft_fills() {
        let panel = wcag::over(PANEL, CANVAS);
        let hovered: [(&str, Rgba); 2] = [
            (
                "FILL_HOVER over PANEL_SOLID",
                wcag::over(FILL_HOVER, PANEL_SOLID),
            ),
            ("FILL_HOVER over PANEL", wcag::over(FILL_HOVER, panel)),
        ];
        // TEXT_DIM is deliberately absent: secondary text brightens to TEXT_2 on
        // hover (KIT.md), because TEXT_DIM on a hovered row is ~3.9:1.
        for (fg_name, fg) in [
            ("TEXT", TEXT),
            ("TEXT_2", TEXT_2),
            ("CONTRAST_TEXT", CONTRAST_TEXT),
        ] {
            for (bg_name, bg) in hovered {
                assert!(
                    wcag::ratio(fg, bg) >= 4.5,
                    "{fg_name} on {bg_name} measures {:.3}:1, below 4.5:1",
                    wcag::ratio(fg, bg)
                );
            }
        }

        let soft: [(&str, Rgba, Rgba, &str); 4] = [
            (
                "CONTRAST_SOFT",
                CONTRAST_SOFT,
                CONTRAST_TEXT,
                "CONTRAST_TEXT",
            ),
            // K2c: the hovered lead face keeps its text readable.
            (
                "CONTRAST_SOFT_HOVER",
                CONTRAST_SOFT_HOVER,
                CONTRAST_TEXT,
                "CONTRAST_TEXT",
            ),
            ("ATTENTION_SOFT", ATTENTION_SOFT, ATTENTION, "ATTENTION"),
            ("ACT_NOW_SOFT", ACT_NOW_SOFT, ACT_NOW_TEXT, "ACT_NOW_TEXT"),
        ];
        for (soft_name, fill, tone_text, tone_name) in soft {
            for (base_name, base) in [("PANEL_SOLID", PANEL_SOLID), ("PANEL", panel)] {
                let bg = wcag::over(fill, base);
                for (fg_name, fg) in [("TEXT", TEXT), ("TEXT_2", TEXT_2), (tone_name, tone_text)] {
                    assert!(
                        wcag::ratio(fg, bg) >= 4.5,
                        "{fg_name} on {soft_name} over {base_name} measures {:.3}:1, below 4.5:1",
                        wcag::ratio(fg, bg)
                    );
                }
            }
        }
        assert!(
            wcag::ratio(TEXT_DIM, hovered[0].1) >= 4.5,
            "TEXT_DIM (#a0a0a0) must hold 4.5:1 on a hovered row"
        );
        // The other half of the rule. TEXT_DIM on the soft fills measures
        // 3.97–4.64:1: below 4.5:1 on CONTRAST_SOFT over PANEL_SOLID, on
        // CONTRAST_SOFT_HOVER and on ATTENTION_SOFT (both bases), and only just
        // above it elsewhere — never comfortably clear — so the rule is uniform:
        // secondary text on any soft fill is TEXT_2.
        for (soft_name, fill, _, _) in soft {
            for (base_name, base) in [("PANEL_SOLID", PANEL_SOLID), ("PANEL", panel)] {
                let r = wcag::ratio(TEXT_DIM, wcag::over(fill, base));
                assert!(
                    r >= 4.5,
                    "TEXT_DIM on {soft_name} over {base_name} measures {r:.3}:1, below 4.5:1"
                );
            }
        }
        for (fill_name, fill, base) in [
            ("CONTRAST_SOFT over PANEL_SOLID", CONTRAST_SOFT, PANEL_SOLID),
            (
                "CONTRAST_SOFT_HOVER over PANEL_SOLID",
                CONTRAST_SOFT_HOVER,
                PANEL_SOLID,
            ),
            ("CONTRAST_SOFT_HOVER over PANEL", CONTRAST_SOFT_HOVER, panel),
            (
                "ATTENTION_SOFT over PANEL_SOLID",
                ATTENTION_SOFT,
                PANEL_SOLID,
            ),
            ("ATTENTION_SOFT over PANEL", ATTENTION_SOFT, panel),
        ] {
            assert!(
                wcag::ratio(TEXT_DIM, wcag::over(fill, base)) >= 4.5,
                "TEXT_DIM fails on {fill_name}: it must hold 4.5:1 (brightened to #a0a0a0)"
            );
        }
    }

    // K1: the tone table is contractual — each accessor returns the spec's
    // exact token for each of the four tones (Neutral's soft fill is the
    // transparent no-fill token, and the Contrast edge is the contrast fill
    // itself).
    #[test]
    fn v2_k1_tone_table_matches_the_spec() {
        assert_eq!(tone_color(Tone::Neutral), TEXT_DIM);
        assert_eq!(tone_text(Tone::Neutral), TEXT_DIM);
        assert_eq!(
            tone_soft(Tone::Neutral),
            TRANSPARENT,
            "Neutral has no soft fill"
        );
        assert_eq!(tone_edge(Tone::Neutral), LINE);

        assert_eq!(tone_color(Tone::Attention), ATTENTION);
        assert_eq!(tone_text(Tone::Attention), ATTENTION);
        assert_eq!(tone_soft(Tone::Attention), ATTENTION_SOFT);
        assert_eq!(tone_edge(Tone::Attention), ATTENTION_EDGE);

        assert_eq!(tone_color(Tone::ActNow), ACT_NOW);
        assert_eq!(tone_text(Tone::ActNow), ACT_NOW_TEXT);
        assert_eq!(tone_soft(Tone::ActNow), ACT_NOW_SOFT);
        assert_eq!(tone_edge(Tone::ActNow), ACT_NOW_EDGE);

        assert_eq!(tone_color(Tone::Contrast), CONTRAST);
        assert_eq!(tone_text(Tone::Contrast), CONTRAST_TEXT);
        assert_eq!(tone_soft(Tone::Contrast), CONTRAST_SOFT);
        assert_eq!(
            tone_edge(Tone::Contrast),
            CONTRAST,
            "the contrast edge is the contrast fill"
        );
    }

    // K1: `palette()` is `&DARK` until slice 11, and every field of `DARK`
    // is the matching token — a field repointed at the wrong constant (e.g.
    // `raise: CANVAS`) would silently flatten the surface stack.
    // Purple stays an identity mark: scan the view code for every use of the
    // purple tokens and allow only the sanctioned sites (Council marks, the
    // rail edge). Never a fill, never another edge. The transcript's "Awili"
    // label is sage green (owner 2026-11-02), so main.rs may not use purple.
    #[test]
    fn purple_tokens_are_used_only_at_sanctioned_sites() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let needles = [
            "p().identity",
            "palette().identity",
            ".council_look",
            "COUNCIL_LOOK",
            "RAIL_EDGE",
        ];
        let mut rail_edge_seen = false;
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            if name == "theme.rs" {
                continue;
            }
            let src = std::fs::read_to_string(&f).unwrap();
            for (i, line) in src.lines().enumerate() {
                if !needles.iter().any(|n| line.contains(n)) {
                    continue;
                }
                let site = format!("{name}:{}", i + 1);
                match name.as_str() {
                    // Council marks (dots / text) and their tests.
                    "council_view.rs" | "council_tests.rs" => {
                        assert!(!line.contains("border_color"), "purple edge at {site}")
                    }
                    // The one sanctioned purple edge.
                    "nav_rail.rs" => {
                        assert!(line.contains("border_color") || line.trim_start().starts_with("//"));
                        rail_edge_seen = true;
                    }
                    _ => panic!("purple token used outside the sanctioned sites: {site}"),
                }
            }
        }
        assert!(rail_edge_seen, "the nav rail must draw the identity edge");
    }

    // Purple theme (owner 2026-11-01): the dark palette is blue, green, red
    // (alerts only), gray (surfaces and text) and purple (edges/accents only). No tan,
    // bone, amber, orange or yellow may creep back in.
    #[test]
    fn dark_palette_is_only_blue_green_purple_gray_red() {
        fn hue_sat(c: Rgba) -> (f32, f32) {
            let (r, g, b) = (c.r, c.g, c.b);
            let mx = r.max(g).max(b);
            let mn = r.min(g).min(b);
            let d = mx - mn;
            if d == 0.0 {
                return (0.0, 0.0);
            }
            let h = if mx == r {
                ((g - b) / d).rem_euclid(6.0)
            } else if mx == g {
                (b - r) / d + 2.0
            } else {
                (r - g) / d + 4.0
            };
            (h * 60.0, d / mx)
        }
        fn hex_of(c: Rgba) -> u32 {
            ((c.r * 255.0).round() as u32) << 16
                | ((c.g * 255.0).round() as u32) << 8
                | ((c.b * 255.0).round() as u32)
        }
        fn family(c: Rgba) -> &'static str {
            let (h, s) = hue_sat(c);
            if s < 0.12 {
                "gray"
            } else if !(15.0..345.0).contains(&h) {
                "red"
            } else if (100.0..=170.0).contains(&h) {
                "green"
            } else if (195.0..250.0).contains(&h) {
                "blue"
            } else if (250.0..=320.0).contains(&h) {
                "purple"
            } else {
                "OUT-OF-FAMILY"
            }
        }
        let p = &DARK;
        let all: Vec<(&str, Rgba)> = vec![
            ("legacy_focus_ring", p.legacy_focus_ring),
            ("ground", p.ground),
            ("canvas", p.canvas),
            ("panel", p.panel),
            ("panel_solid", p.panel_solid),
            ("raise", p.raise),
            ("line", p.line),
            ("control_line", p.control_line),
            ("fill_ghost", p.fill_ghost),
            ("fill_hover", p.fill_hover),
            ("highlight", p.highlight),
            ("transparent", p.transparent),
            ("text", p.text),
            ("text_2", p.text_2),
            ("text_dim", p.text_dim),
            ("contrast", p.contrast),
            ("on_contrast", p.on_contrast),
            ("contrast_text", p.contrast_text),
            ("contrast_soft", p.contrast_soft),
            ("contrast_hover", p.contrast_hover),
            ("contrast_soft_hover", p.contrast_soft_hover),
            ("attention", p.attention),
            ("attention_soft", p.attention_soft),
            ("attention_edge", p.attention_edge),
            ("act_now", p.act_now),
            ("act_now_text", p.act_now_text),
            ("act_now_soft", p.act_now_soft),
            ("act_now_edge", p.act_now_edge),
            ("scrim", p.scrim),
            ("thinking_ring", p.thinking_ring),
            ("contrast_ring", p.contrast_ring),
            ("selection_fill", p.selection_fill),
            ("selection_edge", p.selection_edge),
            ("code_bg", p.code_bg),
            ("inline_code_bg", p.inline_code_bg),
            ("code_border", p.code_border),
            ("bg", p.bg),
            ("chrome", p.chrome),
            ("raised", p.raised),
            ("bubble_assistant", p.bubble_assistant),
            ("border", p.border),
            ("border_input", p.border_input),
            ("muted", p.muted),
            ("faint", p.faint),
            ("on_accent", p.on_accent),
            ("chip_bg", p.chip_bg),
            ("chip_text", p.chip_text),
            ("chip_border", p.chip_border),
            ("chip_warn", p.chip_warn),
            ("sev_critical", p.sev_critical),
            ("sev_attention", p.sev_attention),
            ("council_live", p.council_live),
            ("council_look", p.council_look),
            ("identity", p.identity),
            ("council_act", p.council_act),
            ("row_hover", p.row_hover),
            ("row_active", p.row_active),
            ("search_highlight", p.search_highlight),
            ("selection", p.selection),
            ("selection_on_accent", p.selection_on_accent),
            ("accent", p.accent),
            ("accent_text", p.accent_text),
            ("copy_hover_on_accent", p.copy_hover_on_accent),
            ("user_copy_text", p.user_copy_text),
            ("user_copy_hover", p.user_copy_hover),
            ("accent_bright", p.accent_bright),
            ("accent_secondary", p.accent_secondary),
            ("thinking_halo", p.thinking_halo),
            ("scrollbar_track", p.scrollbar_track),
            ("scrollbar_thumb", p.scrollbar_thumb),
            ("link", p.link),
            ("link_on_accent", p.link_on_accent),
            ("quote_bar", p.quote_bar),
        ];
        for (name, c) in all.iter().copied() {
            let fam = family(c);
            assert_ne!(
                fam,
                "OUT-OF-FAMILY",
                "dark palette {name} ({:02x}{:02x}{:02x}, hue/sat {:?}) is outside blue/green/purple/gray/red",
                (c.r * 255.0).round() as u32,
                (c.g * 255.0).round() as u32,
                (c.b * 255.0).round() as u32,
                hue_sat(c)
            );
        }
        // Status colours are green, never amber.
        assert_eq!(family(CHIP_WARN), "green");
        assert_eq!(family(SEV_ATTENTION), "green");
        assert_eq!(family(ACT_NOW), "red");
        // Council 2026-11-01: surfaces and lines are opaque neutral gray;
        // purple is exactly #a48dd0 and lives only in identity tokens.
        for c in [
            GROUND, CANVAS, PANEL, PANEL_SOLID, RAISE, CHIP_BG, BG, RAISED, BUBBLE_ASSISTANT,
            LINE, CONTROL_LINE, BORDER, BORDER_INPUT, CHIP_BORDER, CODE_BORDER,
        ] {
            assert_eq!(family(c), "gray", "surfaces and lines must be neutral");
            assert_eq!(c.a, 1.0, "surfaces and lines are opaque");
        }
        assert_eq!(PANEL, PANEL_SOLID);
        let (name_ok, purple) = ("identity", rgb(0xa48dd0));
        assert_eq!(COUNCIL_LOOK, purple, "{name_ok}");
        assert_eq!(DARK.identity, purple, "{name_ok}");
        assert_eq!(RAIL_EDGE, DARK.identity, "the rail edge is the one purple edge");
        for (name, c) in all.iter().copied() {
            if family(c) == "purple" {
                assert!(
                    matches!(name, "council_look" | "identity"),
                    "purple may only appear in identity tokens, found in {name}"
                );
            }
        }
        // Blue fills carry white text at AA.
        assert!(wcag::ratio(ON_CONTRAST, CONTRAST) >= 4.5);
        assert!(wcag::ratio(ON_CONTRAST, CONTRAST_HOVER) >= 4.5);
        assert_eq!(hex_of(CONTRAST), 0x2563eb);
        assert_eq!(hex_of(CONTRAST_HOVER), 0x1d4ed8);
        assert_eq!(hex_of(ACT_NOW), 0xd4574a);
        assert_eq!(hex_of(BRAND_SAGE), 0x6dba8e);
        assert_eq!(ACT_NOW_SOFT, TRANSPARENT, "no red wash");
        for c in [TEXT, TEXT_2, TEXT_DIM, MUTED, FAINT, CHIP_TEXT] {
            assert_eq!(family(c), "gray");
        }
    }

    #[test]
    fn v2_k1_palette_dark_holds_every_token() {
        // `palette()` is the dark palette until slice 11 swaps in light.
        // (`DARK` is an immutable static; its identity also keys cached plans.)
        let p = palette();
        assert_eq!(
            *p, DARK,
            "palette() must be the DARK palette until slice 11"
        );
        assert_eq!(p.ground, GROUND);
        assert_eq!(p.canvas, CANVAS);
        assert_eq!(p.panel, PANEL);
        assert_eq!(p.panel_solid, PANEL_SOLID);
        assert_eq!(p.raise, RAISE);
        assert_eq!(p.line, LINE);
        assert_eq!(p.control_line, CONTROL_LINE);
        assert_eq!(p.fill_ghost, FILL_GHOST);
        assert_eq!(p.fill_hover, FILL_HOVER);
        assert_eq!(p.highlight, HIGHLIGHT);
        assert_eq!(p.transparent, TRANSPARENT);
        assert_eq!(p.text, TEXT);
        assert_eq!(p.text_2, TEXT_2);
        assert_eq!(p.text_dim, TEXT_DIM);
        assert_eq!(p.contrast, CONTRAST);
        assert_eq!(p.on_contrast, ON_CONTRAST);
        assert_eq!(p.contrast_text, CONTRAST_TEXT);
        assert_eq!(p.contrast_soft, CONTRAST_SOFT);
        assert_eq!(p.attention, ATTENTION);
        assert_eq!(p.attention_soft, ATTENTION_SOFT);
        assert_eq!(p.attention_edge, ATTENTION_EDGE);
        assert_eq!(p.act_now, ACT_NOW);
        assert_eq!(p.act_now_text, ACT_NOW_TEXT);
        assert_eq!(p.act_now_soft, ACT_NOW_SOFT);
        assert_eq!(p.act_now_edge, ACT_NOW_EDGE);
        assert_eq!(p.scrim, SCRIM);
        assert_eq!(p.thinking_ring, THINKING_RING);
    }

    // K1: the kit size tokens are the spec's values verbatim — the kit parts
    // (K2+) lay their painted sizes out from exactly these constants.
    #[test]
    fn v2_k1_kit_size_tokens_match_the_spec() {
        assert_eq!(ICON, 18.0);
        assert_eq!(ICON_L, 20.0);
        assert_eq!(ICON_S, 14.0);
        assert_eq!(DOT, 8.0);
        assert_eq!(DOT_L, 10.0);
        assert_eq!(BADGE, 9.0);
        assert_eq!(SWITCH_W, 44.0);
        assert_eq!(SWITCH_H, 26.0);
        assert_eq!(MENU_W, 280.0);
        assert_eq!(PANEL_W, 360.0);
        assert_eq!(COMPOSER_MAX, 720.0);
        assert_eq!(READING_MAX, 680.0);
        assert_eq!(PAGE_MAX, 960.0);
        assert_eq!(EDGE, 20.0);
        assert_eq!(RAIL_BUTTON, 48.0);
        assert_eq!(SCROLLBAR_HIT, 20.0);
        assert_eq!(SCROLLBAR_THUMB_W, 6.0);
        assert_eq!(THINK_HALO, 20.0);
        assert_eq!(THINK_MIN, 6.0);
        assert_eq!(THINK_MAX, 12.0);
        // K3 (batch K3, KIT.md § Sheet): the compact-window floor.
        assert_eq!(COMPACT_W, 880.0);
        // K4 (batch K4, KIT.md § NavRail): the 64 × 58 rail item.
        assert_eq!(RAIL_ITEM_W, 64.0);
        assert_eq!(RAIL_ITEM_H, 58.0);
        assert_eq!(RAIL_W, 80.0);
        assert_eq!(SWITCH_KNOB_PAD, 2.0);
        assert_eq!(SWITCH_KNOB, 22.0);
        assert_eq!(SWITCH_KNOB_ON_X, 20.0);
        assert_eq!(
            SWITCH_W - (SWITCH_KNOB_ON_X + SWITCH_KNOB),
            SWITCH_KNOB_PAD,
            "the on knob keeps the same inset on the right"
        );
    }

    // K1: float_shadow() builds one z()-scaled black shadow per FLOAT_SHADOW
    // layer: offset (0, y), the layer's blur, spread 0, never inset. A drift
    // (e.g. an unscaled px offset) would make floaters ignore interface zoom.
    #[test]
    fn v2_k1_float_shadow_is_two_zscaled_black_layers() {
        let shadows = float_shadow();
        assert_eq!(
            shadows.len(),
            FLOAT_SHADOW.len(),
            "one shadow per FLOAT_SHADOW layer"
        );
        for (s, &(y, blur, alpha)) in shadows.iter().zip(FLOAT_SHADOW.iter()) {
            assert_eq!(s.offset.x, px(0.0), "the offset is (0, y)");
            assert_eq!(s.offset.y, z(y), "the y offset is scaled through z()");
            assert_eq!(s.blur_radius, z(blur), "the blur is scaled through z()");
            assert_eq!(s.spread_radius, px(0.0), "spread is 0");
            assert!(!s.inset, "float shadows are never inset");
            let c = s.color.to_rgb();
            assert_eq!((c.r, c.g, c.b), (0.0, 0.0, 0.0), "each layer is black");
            assert_eq!(c.a, alpha, "each layer keeps its FLOAT_SHADOW alpha");
        }
        // At a non-default zoom the shadow scales with the layout…
        set_ui_scale(1.25);
        let scaled = float_shadow();
        assert_eq!(scaled[0].offset.y, px(15.0), "y 12 scales to 15 at 1.25");
        assert_eq!(
            scaled[0].blur_radius,
            px(50.0),
            "blur 40 scales to 50 at 1.25"
        );
        set_ui_scale(1.0);
    }
}
