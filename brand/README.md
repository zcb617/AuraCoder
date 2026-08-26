# AuraCoder brand kit

This kit is the source of truth for the AuraCoder identity selected in July 2026.

## Identity

- Symbol: Split Focus
- Accent: Terminal Mint 400, `#61D596`
- Wordmark: lowercase `auracoder`, outlined from Proxima Nova Alt Bold
- Product UI: Geist
- Code and metadata: Geist Mono
- App icon: dark dimensional shell with restrained top-left light, shaded Split Focus frame, and a lit mint pane

The Proxima Nova font file is not included. Portable wordmark and lockup SVGs contain outlined glyphs, so the app and website do not depend on a locally installed font.

## Asset map

- `brand-kit.html`: visual identity and product token guide
- `tokens.css`: raw palette and semantic dark and light CSS variables
- `tokens.json`: platform-neutral token export
- `APP_INTEGRATION.md`: mapping from the kit to `src/globals.css` and Tauri icon files
- `assets/auracoder-mark-on-dark.svg`: default mark on dark surfaces
- `assets/auracoder-mark-on-light.svg`: default mark on light surfaces
- `assets/auracoder-mark-mono-light.svg`: white one-color mark
- `assets/auracoder-mark-mono-dark.svg`: black one-color mark
- `assets/auracoder-wordmark-on-dark.svg`: outlined lowercase wordmark for dark surfaces
- `assets/auracoder-wordmark-on-light.svg`: outlined lowercase wordmark for light surfaces
- `assets/auracoder-lockup-on-dark.svg`: horizontal mark and wordmark lockup for dark surfaces
- `assets/auracoder-lockup-on-light.svg`: horizontal mark and wordmark lockup for light surfaces
- `assets/auracoder-symbolic.svg`: inline-friendly symbolic mark using `currentColor`
- `assets/app-icon-source.svg`: editable 1024px app icon source
- `app-icon/auracoder.icns`: macOS app icon
- `app-icon/auracoder.ico`: Windows app icon
- `app-icon/auracoder-1024.png`: full-resolution raster app icon

## Product usage

Use Terminal Mint for active, selected, connected, and focus states. Avoid using it as general decoration. Blue communicates running or informational states. Amber communicates pending or warning states. Coral communicates errors and destructive actions. Violet is reserved for model, agent, or review context.

Use the mark by itself at 16px to 28px. Use the horizontal lockup at 96px wide or larger. Keep clear space around the mark equal to the width of its left rail.

## Product application

The July 2026 application pass applied these assets to `src/globals.css`, the sidebar, onboarding, terminal theme, browser title and favicon, and the complete Tauri icon bundle. The original files are preserved in the external backup documented in `BACKUP_INFO.md` at the backup location.

## Rebuilding assets

Run:

```sh
node brand/scripts/build-assets.mjs
```

The script requires `hb-view`, `rsvg-convert`, ImageMagick, `iconutil`, and the locally installed Proxima Nova Alt Bold font.
