//! Rust port of VS Code's `vscode-file://` protocol handler.
//!
//! Mirrors the security contract of
//! `src/vs/platform/protocol/electron-main/protocolMainService.ts` and the URI
//! transform in `src/vs/base/common/network.ts` (`FileAccess.uriToFileUri`):
//!
//! 1. Only `vscode-file:` URLs are served. The fallback authority `vscode-app`
//!    is stripped (other authorities are kept, for Windows UNC paths).
//! 2. The path is lexically normalized so `..` segments cannot escape a root.
//! 3. A resource is served only if it lives under a *valid root*, OR its
//!    extension is in a small media allowlist; otherwise it is blocked
//!    (Electron returned `net::ERR_ABORTED` / -3).
//!
//! In Electron this ran inside the bundled Chromium net stack. Under Tauri the
//! same decision is made here and wired into `register_uri_scheme_protocol`.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use percent_encoding::percent_decode_str;
use url::Url;

/// Authority that VS Code adds when a `file:` URI has none. Stripped on the way
/// back to a filesystem path. See `network.ts` `VSCODE_AUTHORITY`.
pub const FALLBACK_AUTHORITY: &str = "vscode-app";

pub const SCHEME: &str = "vscode-file";

/// Media extensions VS Code allows from *any* location, independent of roots.
/// Source: validExtensions set in protocolMainService.ts (issue #119384).
const VALID_EXTENSIONS: &[&str] = &[
    "svg", "png", "jpg", "jpeg", "gif", "bmp", "webp", "mp4", "otf", "ttf",
];

/// The outcome of a protocol request, matching Electron's two callback shapes:
/// serve a file (with a Content-Type) or abort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Serve this on-disk path with the given MIME type.
    Allow { path: PathBuf, mime: String },
    /// Refuse the request (Electron `-3` ABORTED).
    Block,
}

pub struct ResourceResolver {
    valid_roots: Vec<PathBuf>,
    valid_extensions: HashSet<String>,
}

impl ResourceResolver {
    pub fn new() -> Self {
        ResourceResolver {
            valid_roots: Vec::new(),
            valid_extensions: VALID_EXTENSIONS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Register a directory whose descendants may be served. Mirrors
    /// `addValidFileRoot`: the app root, extensions path and storage homes.
    pub fn add_valid_root<P: AsRef<Path>>(&mut self, root: P) -> &mut Self {
        let normalized = lexically_normalize(root.as_ref());
        if !self.valid_roots.contains(&normalized) {
            self.valid_roots.push(normalized);
        }
        self
    }

    /// Decide whether a `vscode-file:` URL may be served, and as what.
    pub fn resolve(&self, request_url: &str) -> Resolution {
        let path = match uri_to_file_path(request_url) {
            Some(p) => p,
            None => return Resolution::Block,
        };

        let under_root = self.valid_roots.iter().any(|root| is_descendant(&path, root));

        let ext_ok = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| self.valid_extensions.contains(&e.to_ascii_lowercase()))
            .unwrap_or(false);

        if under_root || ext_ok {
            let mime = mime_guess::from_path(&path)
                .first_or_octet_stream()
                .to_string();
            Resolution::Allow { path, mime }
        } else {
            Resolution::Block
        }
    }
}

/// True if `path` is `root` itself or lives beneath it. Comparison is on
/// normalized components so `/opt/app/out-evil` is NOT under `/opt/app/out`.
fn is_descendant(path: &Path, root: &Path) -> bool {
    let mut p = path.components();
    for rc in root.components() {
        match p.next() {
            Some(pc) if pc == rc => {}
            _ => return false,
        }
    }
    true
}

impl Default for ResourceResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a `vscode-file:` URL to an absolute filesystem path, applying the
/// authority rule from `FileAccess.uriToFileUri`. `None` if the scheme is wrong.
pub fn uri_to_file_path(request_url: &str) -> Option<PathBuf> {
    let url = Url::parse(request_url).ok()?;
    if url.scheme() != SCHEME {
        return None;
    }

    // Authority: dropped when it is the fallback, kept otherwise (UNC). On
    // non-Windows a real authority would be a `//host/share` prefix; the spike
    // targets the common (fallback) case and preserves others verbatim.
    let authority = url.host_str().unwrap_or("");
    let raw_path = url.path(); // always starts with '/'
    let decoded = percent_decode_str(raw_path).decode_utf8_lossy();

    let joined = if authority.is_empty() || authority == FALLBACK_AUTHORITY {
        decoded.to_string()
    } else {
        format!("//{}{}", authority, decoded)
    };

    Some(lexically_normalize(Path::new(&joined)))
}

/// Lexically resolve `.`/`..` without touching the filesystem (no symlink
/// canonicalization) — matching VS Code's `path.normalize`.
pub fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver_with_root(root: &str) -> ResourceResolver {
        let mut r = ResourceResolver::new();
        r.add_valid_root(root);
        r
    }

    // --- uri_to_file_path: the network.ts transform ---

    #[test]
    fn strips_fallback_authority() {
        let p = uri_to_file_path("vscode-file://vscode-app/opt/app/out/vs/workbench.js").unwrap();
        assert_eq!(p, PathBuf::from("/opt/app/out/vs/workbench.js"));
    }

    #[test]
    fn decodes_percent_encoding() {
        let p = uri_to_file_path("vscode-file://vscode-app/opt/my%20app/x.js").unwrap();
        assert_eq!(p, PathBuf::from("/opt/my app/x.js"));
    }

    #[test]
    fn rejects_non_vscode_file_scheme() {
        assert_eq!(uri_to_file_path("file:///etc/passwd"), None);
        assert_eq!(uri_to_file_path("https://example.com/x"), None);
    }

    // --- resolve: the protocolMainService security decision ---

    #[test]
    fn allows_file_under_root() {
        let r = resolver_with_root("/opt/app/out");
        assert_eq!(
            r.resolve("vscode-file://vscode-app/opt/app/out/vs/workbench.js"),
            Resolution::Allow {
                path: PathBuf::from("/opt/app/out/vs/workbench.js"),
                mime: "text/javascript".to_string(),
            }
        );
    }

    #[test]
    fn blocks_path_traversal_out_of_root() {
        let r = resolver_with_root("/opt/app/out");
        // `..` escapes the root and lands on a non-allowlisted extension.
        assert_eq!(
            r.resolve("vscode-file://vscode-app/opt/app/out/../../../etc/passwd"),
            Resolution::Block
        );
    }

    #[test]
    fn blocks_arbitrary_script_outside_root() {
        let r = resolver_with_root("/opt/app/out");
        assert_eq!(
            r.resolve("vscode-file://vscode-app/home/user/.ssh/evil.js"),
            Resolution::Block
        );
    }

    #[test]
    fn allows_media_extension_anywhere() {
        // validExtensions are allowed regardless of root (issue #119384).
        let r = resolver_with_root("/opt/app/out");
        assert_eq!(
            r.resolve("vscode-file://vscode-app/home/user/pictures/photo.png"),
            Resolution::Allow {
                path: PathBuf::from("/home/user/pictures/photo.png"),
                mime: "image/png".to_string(),
            }
        );
    }

    #[test]
    fn media_extension_check_is_case_insensitive() {
        let r = resolver_with_root("/opt/app/out");
        assert!(matches!(
            r.resolve("vscode-file://vscode-app/x/Y/Logo.PNG"),
            Resolution::Allow { .. }
        ));
    }

    #[test]
    fn blocks_wrong_scheme() {
        let r = resolver_with_root("/opt/app/out");
        assert_eq!(r.resolve("file:///opt/app/out/vs/workbench.js"), Resolution::Block);
    }

    #[test]
    fn root_prefix_must_be_a_path_boundary() {
        // `/opt/app/out-evil` must NOT be considered inside `/opt/app/out`.
        let r = resolver_with_root("/opt/app/out");
        assert_eq!(
            r.resolve("vscode-file://vscode-app/opt/app/out-evil/x.js"),
            Resolution::Block
        );
    }
}
