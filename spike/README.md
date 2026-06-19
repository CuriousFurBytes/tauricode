# VS Code → Tauri spike

First increment of the Electron→Tauri port. See the full plan in
[`/docs/tauri-migration.md`](../docs/tauri-migration.md).

Two crates:

| Crate | What | Runs headless? |
|---|---|---|
| [`protocol-resolver`](protocol-resolver) | Pure-Rust port of VS Code's `vscode-file://` handler (`protocolMainService.ts`): security rules **and** the COOP/COEP/Cache-Control/Document-Policy headers, plus `asBrowserUri`/`workbench_url` (`network.ts` + `windowImpl.ts`). TDD'd, 18 tests. | ✅ `cargo test` |
| [`tauri-monaco`](tauri-monaco) | Tauri shell. Phase 1: opens a window at the workbench's `vscode-file://` URL and serves assets through the resolver. (`ui/index.html` keeps a standalone Monaco smoke test.) | ❌ needs webkit + a display |

## Run the tested logic (works anywhere)

```bash
cargo test --manifest-path spike/protocol-resolver/Cargo.toml
```

10 tests cover authority stripping, percent-decoding, path-traversal blocking,
root-prefix boundaries, and the media-extension allowlist — the security
contract that must not regress when leaving Electron's Chromium net stack.

## Run the GUI spike (dev machine with a display)

Requires Rust, the Tauri CLI, and on Linux `libwebkit2gtk-4.1-dev` + `libgtk-3-dev`.

```bash
cargo install tauri-cli --version '^2'

# Phase 1 — boot the real workbench. Point at a built VS Code `out/` dir:
npm run compile           # in the repo root, produces ./out
VSCODE_APP_ROOT="$PWD/out" cargo tauri dev --manifest-path spike/tauri-monaco/Cargo.toml
```

Expected (Phase 1): a window titled "VS Code on Tauri — workbench" that loads
`workbench.html` and its script graph through the Rust `vscode-file://` handler
(with COOP/COEP so `SharedArrayBuffer` is available), then errors on the first
`ipcRenderer` call — that IPC transport is Phase 2. This proves the document and
asset graph load in the native webview outside Electron.

To instead run the standalone Monaco smoke test (`ui/index.html`), set the window
URL back to the bundled frontend — useful when you don't have a built `out/`.

> This spike could not be GUI-run in the authoring container: no
> `webkit2gtk-4.1`/GTK dev libraries, no display, and apt mirrors were
> unreachable. The resolver tests were run and pass.
