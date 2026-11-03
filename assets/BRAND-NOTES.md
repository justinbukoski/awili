# Brand pack — repo notes

`assets/brand/` is the shipped **`awili-brand-assets-v1.1`** pack, vendored
**verbatim** — every file byte-for-byte as delivered, nothing renamed, nothing
dropped. So this just works, with no caveats:

```bash
cd assets/brand && shasum -a 256 -c SHA256SUMS   # 131/131 OK
```

These notes live *outside* `assets/brand/` deliberately. In the v1.0 pass they
were appended to the pack's own `README.md`, which silently broke that manifest;
keeping the vendored tree pristine makes the check meaningful.

Dropping the parts we don't use (`rust-tauri/`, `web/`, `tray/`) was also
considered and rejected: pruning means rewriting `SHA256SUMS`, which is exactly
what turns a one-command integrity check into a thing needing explanation. The
unused files are a few hundred KB. Leave them.

## The app icon is COMPILED, not copied — and why

`assets/Awili.icns`, which `scripts/bundle-mac.sh` reads, is built with:

```bash
iconutil -c icns -o assets/Awili.icns assets/brand/app-icons/macos/awili.iconset
```

It is **deliberately not** a copy of `app-icons/macos/awili.icns`. The pack ships
both an `.iconset` and a pre-compiled `.icns`, and they disagree — **in v1.0 and
still in v1.1**:

| | `awili.iconset` (source) | pack's `awili.icns` | our compiled `Awili.icns` |
|---|---|---|---|
| 16, 32, 128, 256, 512, 1024 | ✅ | ✅ | ✅ |
| **64 (`32x32@2x`)** | ✅ | ❌ **missing** | ✅ |
| 48 (legacy, non-standard) | — | present | — |
| **entries** | **10** | **7** | **10** |

64px is the Retina representation of the 32pt icon — the size Finder list view
and several system surfaces request. Without it macOS rescales a neighbouring
size and the icon reads soft in exactly that slot. The `.iconset` contains a
correct 64px image; only the pack's compiled `.icns` drops it, so compiling from
the iconset is both more correct and closer to the designer's intent.

**This is a defect in the icon pack's build step, reported across two releases
now.** It is not a repo problem and re-vendoring will not fix it. To check
whether a future pack has finally fixed it:

```bash
iconutil -c iconset -o /tmp/check.iconset assets/brand/app-icons/macos/awili.icns && ls /tmp/check.iconset | wc -l
```

`10` means fixed, and the compile step here can be replaced by a plain copy.

`scripts/bundle-mac.sh` asserts the stack at bundle time and fails below 10, so
a regression here cannot ship silently.

## What changed in v1.1

- **The app tile is now Observatory bone (`#E2DCCF`)**, matching awili.ai. The
  v1.0 deep-navy application background is retired. The mark itself is unchanged:
  blue `#3B82F6` left bracket, sage `#6DBA8E` right bracket, navy `#071A2E` cursor.
- Much broader coverage: `generic-png` (18 sizes), `web` (favicon / Apple touch /
  PWA / maskable / manifest), `tray` (color + monochrome template), Linux
  symbolic icon, full Tauri v2 desktop set, and `macos/AppIcon-1024.png` App
  Store art (now the source for `assets/icon_1024x1024.png`).
- `awili-icon-size-qa.png` renders the mark from 16 to 256 on both grounds —
  useful when judging small-size legibility.

**The five brand colors are unchanged from v1.0**, so the `BRAND_*` constants in
`crates/awili-app/src/theme.rs` needed no edit. They stay pinned by
`brand_palette_matches_the_shipped_brand_pack`.
