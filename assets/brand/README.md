# Awili brand assets v1.1

Approved concept: two conversational/code brackets converging on one shared cursor.

All application tiles use the same Observatory bone background as awili.ai.
The prior deep-navy application background is retired.

## Brand colors

- Awili blue: `#3B82F6`
- Observatory sage: `#6DBA8E`
- Deep navy: `#071A2E`
- Observatory bone: `#E2DCCF`
- Observatory charcoal: `#141414`

## Package map

- `brand/svg` and `brand/png`: standalone mark, color and monochrome variants.
- `logos/svg` and `logos/png`: horizontal, stacked, light, dark, and monochrome logos.
- `app-icons/master`: editable SVG icon sources, including maskable web source.
- `app-icons/generic-png`: 18 framework-neutral PNG sizes from 16 to 1024.
- `app-icons/macos`: `.icns`, complete source iconset, and 1024px App Store art.
- `app-icons/windows`: multi-resolution `.ico` and Windows square/store assets.
- `app-icons/linux/hicolor`: complete freedesktop raster hierarchy, scalable SVG,
  and monochrome symbolic icon.
- `app-icons/rust-tauri`: complete Tauri v2 desktop icon directory, including
  Windows square/store assets.
- `app-icons/web`: favicon, Apple touch, PWA, maskable icon, and web manifest.
- `app-icons/tray`: color and monochrome template sizes for tray/status-bar use.

## Rust integration

### Tauri v2

Copy `app-icons/rust-tauri/*` into `src-tauri/icons/`. The conventional filenames
match Tauri's generated desktop project layout for macOS, Windows, and Linux.

### Generic native Rust

- macOS bundle: set `CFBundleIconFile` to `awili` and place `awili.icns` in bundle resources.
- Windows: embed `awili.ico` through your resource compiler (`winresource`, `embed-resource`, or framework tooling).
- Linux: install the `hicolor` tree under `/usr/share/icons/hicolor` or `$XDG_DATA_HOME/icons/hicolor`, preserving directories.

## Usage rules

- Use the color mark on light or transparent surfaces.
- Use Observatory bone (`#E2DCCF`) behind every full application icon.
- Use the bone/dark logo on charcoal or navy surfaces.
- Use monochrome variants where color is unavailable.
- Do not recolor the blue or sage independently; they represent the two collaborating voices.
- Keep the cursor centered and preserve generous clear space around the mark.
