//! Tauri shell for the VS Code → Tauri port.
//!
//! Phase 1: boot the workbench window.
//! The Rust main process computes the workbench's `vscode-file://` URL (the
//! analog of `windowImpl.ts` `loadURL`), opens one native-webview window at it,
//! and serves every asset through the TDD'd `vscode-protocol-resolver` — with
//! the same COOP/COEP/Cache-Control/Document-Policy headers
//! `protocolMainService.ts` attaches.
//!
//! Expected Phase-1 behavior: the workbench HTML + scripts load and begin
//! booting, then error on the first `ipcRenderer` call. Wiring that IPC
//! transport is Phase 2; this phase proves the document and asset graph load
//! in the native webview outside Electron.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};

use tauri::http::{Response, StatusCode};
use tauri::{WebviewUrl, WebviewWindowBuilder};
use vscode_protocol_resolver::{workbench_url, ResourceResolver, Resolution};

/// Where the built `vs/` bundle lives. Override with VSCODE_APP_ROOT; otherwise
/// assume it sits next to the executable (as in a packaged app).
fn app_root() -> PathBuf {
    if let Ok(root) = std::env::var("VSCODE_APP_ROOT") {
        return PathBuf::from(root);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `true` for a packaged build, `false` when running from sources (VSCODE_DEV).
fn is_built() -> bool {
    std::env::var("VSCODE_DEV").is_err()
}

fn build_resolver(root: &Path, built: bool) -> ResourceResolver {
    // ProtocolMainService trusts appRoot, extensionsPath and storage homes.
    // Phase 1 trusts the app root; later phases add the others.
    let mut r = ResourceResolver::new()
        .with_built(built)
        // The workbench requires cross-origin isolation for SharedArrayBuffer.
        .with_cross_origin_isolated(true);
    r.add_valid_root(root);
    r
}

fn main() {
    let root = app_root();
    let built = is_built();
    let url = workbench_url(&root, built);
    let resolver = build_resolver(&root, built);

    tauri::Builder::default()
        // The Tauri analog of `defaultSession.protocol.registerFileProtocol`.
        .register_uri_scheme_protocol("vscode-file", move |_ctx, request| {
            match resolver.resolve(&request.uri().to_string()) {
                Resolution::Allow { path, mime, headers } => serve_file(path, mime, headers),
                Resolution::Block => Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Vec::new())
                    .unwrap(),
            }
        })
        .setup(move |app| {
            // The analog of windowImpl's BrowserWindow + loadURL.
            let target = url::Url::parse(&url)?;
            WebviewWindowBuilder::new(app, "workbench", WebviewUrl::CustomProtocol(target))
                .title("VS Code on Tauri — workbench")
                .inner_size(1400.0, 900.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn serve_file(path: PathBuf, mime: String, headers: Vec<(String, String)>) -> Response<Vec<u8>> {
    match std::fs::read(&path) {
        Ok(bytes) => {
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", mime);
            for (k, v) in headers {
                builder = builder.header(k, v);
            }
            builder.body(bytes).unwrap()
        }
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Vec::new())
            .unwrap(),
    }
}
