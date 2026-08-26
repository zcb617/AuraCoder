# AuraCoder app integration map

This map documents how the brand kit translates into the existing variables in `src/globals.css`. The July 2026 application pass used this mapping for the product theme and icon bundle.

## Dark theme mapping

| Existing variable | Brand token | Value |
| --- | --- | --- |
| `--bg-0` | `--auracoder-canvas` | `#010102` |
| `--bg-1` | `--auracoder-canvas` | `#010102` |
| `--bg-2` | `--auracoder-pane` | `#040406` |
| `--bg-3` | `--auracoder-surface` | `#0D0D11` |
| `--bg-4` | `--auracoder-surface-hover` | `#18181C` |
| `--bg-5` | `--auracoder-border` | `#2D2D32` |
| `--text-1` | `--auracoder-text-primary` | `#FAFAFB` |
| `--text-2` | `--auracoder-text-secondary` | `#B1B1B4` |
| `--text-3` | `--auracoder-text-tertiary` | `#89898D` |
| `--accent` | `--auracoder-accent` | `#61D596` |
| `--accent-dim` | Mint 400 at 10 percent | `rgba(97, 213, 150, 0.10)` |
| `--accent-glow` | Remove | `none` |
| `--accent-2` | `--auracoder-status-agent` | `#A88DFF` |
| `--success` | Mint 500 | `#33B978` |
| `--danger` | `--auracoder-status-danger` | `#F9776E` |
| `--warning` | `--auracoder-status-warning` | `#F1B047` |
| `--info` | `--auracoder-status-info` | `#57B6FF` |
| `--sidebar-bg` | `--auracoder-sidebar` | `#0D0D11` |
| `--content-bg` | `--auracoder-pane` | `#040406` |
| `--code-bg` | `--auracoder-canvas` | `#010102` |

## Light theme mapping

| Existing variable | Brand token | Value |
| --- | --- | --- |
| `--bg-0` | `--auracoder-pane` | `#FFFFFF` |
| `--bg-1` | `--auracoder-pane` | `#FFFFFF` |
| `--bg-2` | `--auracoder-canvas` | `#FAFAFB` |
| `--bg-3` | `--auracoder-sidebar` | `#ECECEF` |
| `--bg-4` | Neutral 200 | `#D4D4D7` |
| `--bg-5` | Neutral 300 | `#B1B1B4` |
| `--text-1` | `--auracoder-text-primary` | `#18181C` |
| `--text-2` | `--auracoder-text-secondary` | `#47474C` |
| `--text-3` | `--auracoder-text-tertiary` | `#636367` |
| `--accent` | `--auracoder-accent` | `#006D40` |
| `--accent-dim` | Mint 700 at 8 percent | `rgba(0, 109, 64, 0.08)` |
| `--accent-glow` | Remove | `none` |
| `--accent-2` | `--auracoder-status-agent` | `#6E4FC1` |
| `--success` | Mint 700 | `#006D40` |
| `--danger` | `--auracoder-status-danger` | `#BD413D` |
| `--warning` | `--auracoder-status-warning` | `#935C00` |
| `--info` | `--auracoder-status-info` | `#006EB8` |
| `--sidebar-bg` | `--auracoder-sidebar` | `#ECECEF` |
| `--content-bg` | `--auracoder-pane` | `#FFFFFF` |
| `--code-bg` | Neutral 100 | `#ECECEF` |

## Asset replacement map

| Current Tauri file | Brand asset |
| --- | --- |
| `src-tauri/icons/icon.svg` | `brand/assets/app-icon-source.svg` |
| `src-tauri/icons/icon.png` | `brand/app-icon/auracoder-512.png` |
| `src-tauri/icons/icon.icns` | `brand/app-icon/auracoder.icns` |
| `src-tauri/icons/icon.ico` | `brand/app-icon/auracoder.ico` |
| `src-tauri/icons/32x32.png` | `brand/app-icon/auracoder-32.png` |
| `src-tauri/icons/64x64.png` | `brand/app-icon/auracoder-64.png` |
| `src-tauri/icons/128x128.png` | `brand/app-icon/auracoder-128.png` |
| `src-tauri/icons/128x128@2x.png` | `brand/app-icon/auracoder-256.png` |

## Implementation order

1. Import `brand/tokens.css` concepts into `src/globals.css` while preserving the existing variable names used by components.
2. Replace identity assets in the app shell and release surfaces.
3. Replace Tauri desktop icon files using the map above.
4. Audit every use of `--accent`. Selection and connection states should become mint. Destructive or error uses should become coral. Agent and model uses should become violet.
5. Verify dark and light themes at hover, focus, selected, warning, error, and disabled states.
