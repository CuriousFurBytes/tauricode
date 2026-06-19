# VS Code → Tauri spike

First increment of the Electron→Tauri port. See the full plan in
[`/docs/tauri-migration.md`](../docs/tauri-migration.md).

Two crates:

| Crate | What | Runs headless? |
|---|---|---|
| [`protocol-resolver`](protocol-resolver) | Pure-Rust port of VS Code's `vscode-file://` handler (`protocolMainService.ts`) with the same security rules. TDD'd. | ✅ `cargo test` |
| [`tauri-monaco`](tauri-monaco) | Tauri shell that opens a window, hosts **Monaco** in the OS-native webview, and serves assets through the resolver. | ❌ needs webkit + a display |

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
cd spike/tauri-monaco
cargo tauri dev
```

Expected: a window titled "VS Code on Tauri — Monaco spike" with a working
Monaco editor (syntax highlighting, dark theme) and a status bar showing the
native webview's user agent plus the `vscode-file://` protocol result — proving
the editor core runs outside Electron and the Rust protocol handler serves
local assets.

> This spike could not be GUI-run in the authoring container: no
> `webkit2gtk-4.1`/GTK dev libraries, no display, and apt mirrors were
> unreachable. The resolver tests were run and pass.
