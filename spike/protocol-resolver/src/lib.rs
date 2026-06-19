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

/// Relative resource paths of the workbench entry document. The window loads
/// `workbench.html` when built, `workbench-dev.html` from sources. See
/// `windowImpl.ts` (`FileAccess.asBrowserUri('vs/code/.../workbench{,-dev}.html')`).
pub const WORKBENCH_HTML: &str = "vs/code/electron-browser/workbench/workbench.html";
pub const WORKBENCH_DEV_HTML: &str = "vs/code/electron-browser/workbench/workbench-dev.html";

/// The outcome of a protocol request, matching Electron's two callback shapes:
/// serve a file (with a Content-Type + headers) or abort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Serve this on-disk path with the given MIME type and response headers.
    Allow {
        path: PathBuf,
        mime: String,
        headers: Vec<(String, String)>,
    },
    /// Refuse the request (Electron `-3` ABORTED).
    Block,
}

pub struct ResourceResolver {
    valid_roots: Vec<PathBuf>,
    valid_extensions: HashSet<String>,
    /// Whether the renderer runs cross-origin-isolated. When true the workbench
    /// document gets COOP+COEP. Mirrors `environmentService.crossOriginIsolated`.
    cross_origin_isolated: bool,
    /// `false` when running from sources: adds `Cache-Control: no-cache,no-store`
    /// (protocolMainService evicts the renderer memory cache in OSS dev).
    is_built: bool,
}

impl ResourceResolver {
    pub fn new() -> Self {
        ResourceResolver {
            valid_roots: Vec::new(),
            valid_extensions: VALID_EXTENSIONS.iter().map(|s| s.to_string()).collect(),
            cross_origin_isolated: false,
            is_built: true,
        }
    }

    pub fn with_cross_origin_isolated(mut self, value: bool) -> Self {
        self.cross_origin_isolated = value;
        self
    }

    pub fn with_built(mut self, value: bool) -> Self {
        self.is_built = value;
        self
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
            let headers = self.headers_for(&path, request_url);
            Resolution::Allow { path, mime, headers }
        } else {
            Resolution::Block
        }
    }

    /// Response headers for an allowed resource, mirroring the accumulation in
    /// `protocolMainService.handleResourceRequest`:
    /// 1. if cross-origin-isolated: COOP+COEP for the workbench document, else
    ///    whatever the `vscode-coi` query param requests;
    /// 2. if not built: `Cache-Control: no-cache, no-store`;
    /// 3. always: `Document-Policy` on the workbench document.
    fn headers_for(&self, path: &Path, request_url: &str) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = Vec::new();
        let is_workbench = is_workbench_document(path);

        if self.cross_origin_isolated {
            if is_workbench {
                push_coop_coep(&mut headers);
            } else {
                push_coi_from_query(&mut headers, request_url);
            }
        }

        if !self.is_built {
            headers.push(("Cache-Control".into(), "no-cache, no-store".into()));
        }

        if is_workbench {
            headers.push((
                "Document-Policy".into(),
                "include-js-call-stacks-in-crash-reports".into(),
            ));
        }

        headers
    }
}

fn is_workbench_document(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("workbench.html") | Some("workbench-dev.html")
    )
}

fn push_coop_coep(headers: &mut Vec<(String, String)>) {
    headers.push(("Cross-Origin-Opener-Policy".into(), "same-origin".into()));
    headers.push(("Cross-Origin-Embedder-Policy".into(), "require-corp".into()));
}

/// `COI.getHeadersFromQuery`: the `vscode-coi` param selects 1=COOP, 2=COEP, 3=both.
fn push_coi_from_query(headers: &mut Vec<(String, String)>, request_url: &str) {
    let value = Url::parse(request_url)
        .ok()
        .and_then(|u| u.query_pairs().find(|(k, _)| k == "vscode-coi").map(|(_, v)| v.into_owned()));
    match value.as_deref() {
        Some("1") => headers.push(("Cross-Origin-Opener-Policy".into(), "same-origin".into())),
        Some("2") => headers.push(("Cross-Origin-Embedder-Policy".into(), "require-corp".into())),
        Some("3") => push_coop_coep(headers),
        _ => {}
    }
}

/// `FileAccess.asBrowserUri`: turn an absolute file path into the
/// `vscode-file://vscode-app/...` URL the renderer loads. Inverse of
/// `uri_to_file_path`.
pub fn file_path_to_uri(path: &Path) -> String {
    // Percent-encode each path segment but keep the `/` separators.
    const SEG: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    let normalized = lexically_normalize(path);
    let mut encoded = String::new();
    for comp in normalized.components() {
        if let Component::Normal(seg) = comp {
            encoded.push('/');
            encoded.push_str(
                &percent_encoding::utf8_percent_encode(&seg.to_string_lossy(), SEG).to_string(),
            );
        }
    }
    if encoded.is_empty() {
        encoded.push('/');
    }
    format!("{}://{}{}", SCHEME, FALLBACK_AUTHORITY, encoded)
}

/// The `vscode-file://` URL of the workbench document to load, given the app
/// root that contains the `vs/` bundle. Mirrors the `windowImpl.ts` `loadURL`.
pub fn workbench_url(app_root: &Path, is_built: bool) -> String {
    let rel = if is_built { WORKBENCH_HTML } else { WORKBENCH_DEV_HTML };
    file_path_to_uri(&app_root.join(rel))
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
                headers: vec![],
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
                headers: vec![],
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

    // --- Phase 1: booting the workbench window ---

    fn headers_of(res: &Resolution) -> Vec<(String, String)> {
        match res {
            Resolution::Allow { headers, .. } => headers.clone(),
            Resolution::Block => panic!("expected Allow, got Block"),
        }
    }
    fn has_header(res: &Resolution, k: &str, v: &str) -> bool {
        headers_of(res).iter().any(|(hk, hv)| hk == k && hv == v)
    }

    #[test]
    fn workbench_url_points_at_built_html() {
        let url = workbench_url(Path::new("/opt/app/out"), true);
        assert_eq!(
            url,
            "vscode-file://vscode-app/opt/app/out/vs/code/electron-browser/workbench/workbench.html"
        );
    }

    #[test]
    fn workbench_url_uses_dev_html_from_sources() {
        let url = workbench_url(Path::new("/opt/app/out"), false);
        assert!(url.ends_with("/workbench-dev.html"), "got {url}");
    }

    #[test]
    fn browser_uri_roundtrips_with_file_path() {
        // asBrowserUri then uriToFileUri must return the original path,
        // including paths that need percent-encoding.
        let original = PathBuf::from("/opt/my app/vs/loader.js");
        let url = file_path_to_uri(&original);
        assert_eq!(uri_to_file_path(&url), Some(original));
    }

    #[test]
    fn workbench_html_always_gets_document_policy() {
        // Unconditional in protocolMainService, even when built and not COI.
        let r = resolver_with_root("/opt/app/out");
        let res = r.resolve(&workbench_url(Path::new("/opt/app/out"), true));
        assert!(has_header(
            &res,
            "Document-Policy",
            "include-js-call-stacks-in-crash-reports"
        ));
    }

    #[test]
    fn workbench_html_gets_coop_coep_when_cross_origin_isolated() {
        let mut r = ResourceResolver::new().with_cross_origin_isolated(true);
        r.add_valid_root("/opt/app/out");
        let res = r.resolve(&workbench_url(Path::new("/opt/app/out"), true));
        assert!(has_header(&res, "Cross-Origin-Opener-Policy", "same-origin"));
        assert!(has_header(&res, "Cross-Origin-Embedder-Policy", "require-corp"));
    }

    #[test]
    fn non_workbench_gets_no_coi_without_query_even_when_isolated() {
        let mut r = ResourceResolver::new().with_cross_origin_isolated(true);
        r.add_valid_root("/opt/app/out");
        let res = r.resolve("vscode-file://vscode-app/opt/app/out/vs/main.js");
        assert!(headers_of(&res).is_empty());
    }

    #[test]
    fn non_workbench_honors_vscode_coi_query() {
        let mut r = ResourceResolver::new().with_cross_origin_isolated(true);
        r.add_valid_root("/opt/app/out");
        let res = r.resolve("vscode-file://vscode-app/opt/app/out/w.js?vscode-coi=3");
        assert!(has_header(&res, "Cross-Origin-Opener-Policy", "same-origin"));
        assert!(has_header(&res, "Cross-Origin-Embedder-Policy", "require-corp"));
    }

    #[test]
    fn unbuilt_adds_no_cache() {
        let r = ResourceResolver::new().with_built(false);
        let mut r = r;
        r.add_valid_root("/opt/app/out");
        let res = r.resolve("vscode-file://vscode-app/opt/app/out/vs/main.js");
        assert!(has_header(&res, "Cache-Control", "no-cache, no-store"));
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
