//! Tauri shell for the VS Code → Tauri spike.
//!
//! Goal: prove the workbench's renderer engine (Monaco) runs in the OS-native
//! webview (WebView2 / WKWebView / WebKitGTK) instead of Electron's bundled
//! Chromium, and that local assets can be served through the same
//! `vscode-file://` scheme VS Code already emits from `FileAccess.asBrowserUri`.
//!
//! This replaces, for the spike's slice, what
//! `protocolMainService.ts` + `BrowserWindow` do in the Electron main process.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use tauri::http::{Response, StatusCode};
use vscode_protocol_resolver::{ResourceResolver, Resolution};

/// Build the resolver with the same roots VS Code's ProtocolMainService trusts.
/// In the real port these come from the environment service; for the spike we
/// trust the app's own resource directory (where `ui/` and Monaco are bundled).
fn build_resolver() -> ResourceResolver {
    let mut r = ResourceResolver::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            r.add_valid_root(dir);
        }
    }
    r
}

fn main() {
    let resolver = build_resolver();

    tauri::Builder::default()
        // The Tauri analog of `defaultSession.protocol.registerFileProtocol`.
        // Every `vscode-file://` request the renderer makes routes through the
        // exact same security decision we unit-tested in protocol-resolver.
        .register_uri_scheme_protocol("vscode-file", move |_ctx, request| {
            match resolver.resolve(&request.uri().to_string()) {
                Resolution::Allow { path, mime } => serve_file(path, mime),
                Resolution::Block => Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Vec::new())
                    .unwrap(),
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn serve_file(path: PathBuf, mime: String) -> Response<Vec<u8>> {
    match std::fs::read(&path) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", mime)
            .body(bytes)
            .unwrap(),
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Vec::new())
            .unwrap(),
    }
}
