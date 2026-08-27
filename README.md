## Development

```bash
pnpm tauri:dev          # full desktop app in dev mode
pnpm tauri:build        # native desktop bundles
pnpm tauri:build --no-bundle # 只生成auracoder.exe程序，不制作安装包
pnpm dev                # frontend-only dev server
pnpm build              # frontend production build
pnpm test               # Vitest suite
pnpm typecheck          # TypeScript no-emit check

pnpm build:claude-sidecar   # bundle the runtime Claude sidecar
pnpm build:desktop          # build frontend + bundled sidecar assets, not native app bundles
pnpm prune:artifacts:check  # inspect generated artifacts that are safe to remove
pnpm prune:artifacts        # remove repo-local generated artifacts like src-tauri/target
pnpm prune:artifacts:stale:check  # inspect stale Rust/Tauri artifacts older than 7 days
pnpm prune:artifacts:stale        # remove stale Rust/Tauri artifacts older than 7 days
pnpm release:check          # evaluate whether a release should be cut
pnpm release                # run release-it
```

Rust-only:

```bash
cd src-tauri
cargo check
cargo fmt
cargo clippy
```