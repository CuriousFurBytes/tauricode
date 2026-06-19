# Porting VS Code (Code – OSS) from Electron to Tauri

Status: **spike + plan** (first increment). This document is the migration map;
the runnable spike lives in [`/spike`](../spike). Authored against the codebase
at branch `claude/vscode-tauri-port-07lnm1`.

> Scope honesty: a *complete* Electron→Tauri port that keeps "100% compatibility
> with plugins, themes and config" is a multi-quarter, multi-engineer effort, not
> a single change. This plan exists so the work can be done incrementally (YAGNI)
> with each step independently shippable and benchmarked. It does not pretend the
> port is finished.

---

## 1. How VS Code uses Electron today

Electron appears in **two** layers (verified by grep on `src/`):

| Layer | Dirs / files | Role |
|---|---|---|
| `*/electron-main/**` | 46 files importing `electron` | The **main process**: windows, menus, dialogs, lifecycle, native OS, custom protocols, child processes |
| `*/electron-browser/**` | 116 dirs | The **renderer**: workbench UI that talks to main over a sandboxed `ipcRenderer` preload and loads assets over `vscode-file://` |
| `*/electron-utility/**` | e.g. `request/electron-utility` | Code running in Electron `utilityProcess` children |

Three process kinds matter for the port:

1. **Main** (Electron `app`) → becomes a **Rust/Tauri** process.
2. **Renderer** (Chromium) → becomes the **OS-native webview** (WebView2 / WKWebView / WebKitGTK). *Highest-risk swap.*
3. **Extension host / shared process / pty host** (`utilityProcess`, Node.js) → must **stay Node.js** to keep plugin compatibility → shipped as a **Tauri sidecar**.

```
Electron today                         Tauri target
┌──────────────┐                       ┌──────────────┐
│ main (Node)  │  electron.app         │ main (Rust)  │  tauri::App
│  BrowserWin  │  ──renderer──▶        │  WebviewWindow│ ──renderer──▶
│  ipcMain     │                       │  IPC/commands │
│  protocol    │  vscode-file://       │ uri_scheme    │  vscode-file:// (ported ✔ spike)
│ utilityProc  │  ──fork──▶ extHost    │  sidecar(node)│ ──spawn──▶ extHost (unchanged)
└──────────────┘                       └──────────────┘
   renderer = bundled Chromium            renderer = system webview  ⚠ biggest risk
```

---

## 2. The three real blockers (read before estimating)

1. **Rendering engine swap (⚠ critical).** Electron bundles a known Chromium.
   Tauri uses whatever webview the OS ships. The workbench + Monaco assume
   Chromium quirks (CSS, `ResizeObserver` timing, GPU compositing, `webview`
   tags, `<iframe>` sandboxing). On **Linux/WebKitGTK** the gap is large and the
   "100% compatibility" bar is genuinely at risk. The spike exists specifically
   to start measuring this.
2. **Extension host must remain Node.js.** Plugins use the `vscode` API, native
   node addons, `process`, etc. Tauri gives Rust, not Node. The only way to keep
   plugin compatibility is to **ship Node as a sidecar** and keep the existing
   extension-host code almost unchanged. This is a subsystem, not a line change.
3. **Electron-specific IPC + `webview`/`BrowserView`.** `MessagePortMain`,
   sandboxed preload, `<webview>` tags and `BrowserView`/`WebContentsView` have
   no 1:1 Tauri analog and need redesign on Tauri IPC + child webviews.

**Config & themes are the easy win.** Settings, keybindings and color themes are
plain JSON/CSS interpreted *inside* the workbench (renderer), with no Electron
dependency. They keep working as long as the renderer and the file-system
services keep working — no porting required. Compatibility here is "don't break
it", not "rebuild it".

---

## 3. Compatibility matrix — Electron API → Tauri/Rust

Risk: 🟢 direct equivalent · 🟡 needs glue · 🔴 hard / partial.
"Crate" follows the rule *use an equivalent Rust package directly* where one exists.

### 3a. App & lifecycle
| Electron | Used in | Tauri / Rust replacement | Risk |
|---|---|---|---|
| `app` ready/quit/paths/`requestSingleInstanceLock` | `main.ts`, `code/electron-main/{main,app}.ts`, `launch`, `lifecycle` | `tauri::App` lifecycle + `tauri-plugin-single-instance`; paths via `dirs`/`tauri::path` | 🟡 |
| `crashReporter` | `main.ts` | `tauri-plugin-sentry` or breakpad via `minidumper`/`crashpad` | 🟡 |
| `contentTracing`, `profiling/windowProfiling` | `app.ts`, profiling | Drop initially (YAGNI); later `tracing` + chrome-trace export | 🟡 |
| `safeStorage` (`encryption`) | `encryption/electron-main` | `keyring` crate (Keychain/libsecret/DPAPI) | 🟢 |
| `update` (Squirrel/darwin/win32) | `update/electron-main/*` | `tauri-plugin-updater` | 🟡 |

### 3b. Windows & rendering
| Electron | Used in | Tauri / Rust replacement | Risk |
|---|---|---|---|
| `BrowserWindow`/`BaseWindow` | `windows/electron-main/windowImpl.ts`, `app.ts` | `tauri::WebviewWindowBuilder` (wry/tao) | 🟡 |
| Renderer = bundled Chromium | all `electron-browser/**` | OS-native webview | 🔴 **the core risk** |
| `auxiliaryWindow` (popouts) | `auxiliaryWindow/electron-main/*` | Multiple `WebviewWindow`s | 🟡 |
| `BrowserView`/`WebContentsView` | `browserView/electron-main/*` | Child webviews (`add_child` in wry 0.4x) | 🔴 |
| `webContents`/`WebFrameMain` | `webview`, `auth`, `native`, `diagnostics` | `Webview` handle + JS eval; no full parity | 🔴 |
| `<webview>` tag (extension webviews) | `webview/electron-main/*` | Child webview or sandboxed `<iframe>` | 🔴 |
| `screen`/`Display` | `native`, `browserView` | `tauri`/`tao` monitor APIs | 🟢 |
| `nativeTheme` | `theme/electron-main` | `tauri` theme events + `dark-light` crate | 🟢 |

### 3c. IPC
| Electron | Used in | Tauri / Rust replacement | Risk |
|---|---|---|---|
| `ipcMain`/`validatedIpcMain` | `base/parts/ipc/electron-main/*` | `#[tauri::command]` + `emit`/`listen` behind VS Code's `IChannel` abstraction | 🟡 |
| `MessagePortMain`/`MessageChannelMain` | `ipc.mp.ts`, `sharedProcess`, `utilityProcess` | Browser `MessageChannel` + Tauri IPC bridge | 🔴 |
| sandboxed preload `ipcRenderer` | `platform/window/electron-browser`, preload scripts | Tauri injected API / `withGlobalTauri` + custom preload | 🟡 |

> Strategy: keep VS Code's own `IPCServer`/`IChannel`/`IPCClient` abstraction and
> swap only the **transport** underneath. Most callers never touch `electron`
> directly — they use `IMainProcessService`/`IChannel`, which shrinks the real
> surface dramatically.

### 3d. Native OS integration
| Electron | Used in | Tauri / Rust replacement | Risk |
|---|---|---|---|
| `dialog` (open/save/message) | `dialogs/electron-main`, `native` | `tauri-plugin-dialog` (rfd) | 🟢 |
| `Menu`/`MenuItem`/menubar | `menubar/electron-main`, `contextmenu` | `tauri::menu` (muda) | 🟡 |
| `clipboard` | `native`, `browserView` | `tauri-plugin-clipboard-manager` (arboard) | 🟢 |
| `shell` openExternal/trash | `native`, `files`, `windows` | `tauri-plugin-opener` + `trash` crate | 🟢 |
| `Notification` | `native` | `tauri-plugin-notification` | 🟢 |
| `globalShortcut` | (where used) | `tauri-plugin-global-shortcut` | 🟢 |
| `powerMonitor`/`powerSaveBlocker` | `native`, `app.ts` | `tauri-plugin-os` + `keepawake`/`nosleep` crate | 🟡 |
| `systemPreferences` | `native`, `app.ts` | per-OS crates / `objc2`, `windows` | 🟡 |
| `desktopCapturer` | `app.ts` | `scap`/`xcap` crate; partial | 🔴 |
| `JumpList` (Windows) | `workspaces/electron-main/history` | `windows` crate `ICustomDestinationList` | 🟡 |

### 3e. Protocols, network, files
| Electron | Used in | Tauri / Rust replacement | Risk |
|---|---|---|---|
| `protocol.registerFileProtocol('vscode-file')` | `protocol`, `webview/webviewProtocolProvider` | **Ported in spike** → `register_uri_scheme_protocol` + `vscode-protocol-resolver` | 🟢 ✔ |
| `protocol.interceptFileProtocol('file')` block | `protocolMainService` | resolver returns `Block` for non-`vscode-file` | 🟢 ✔ |
| `net` (HTTP) | `request/electron-utility` | `reqwest` crate | 🟢 |
| `session` config (COOP/COEP, headers) | `app.ts`, `protocol` | **Ported (Phase 1)** → resolver returns COOP/COEP, `vscode-coi` query headers, Cache-Control and Document-Policy in the `uri_scheme` response | 🟢 ✔ |
| disk FS provider | `files/electron-main` | Stays Node in sidecar; Rust `std::fs`/`notify` if moved | 🟡 |

### 3f. Child processes (the Node sidecar)
| Electron | Used in | Tauri / Rust replacement | Risk |
|---|---|---|---|
| `utilityProcess` | `utilityProcess/electron-main` | **Node sidecar** via `tauri-plugin-shell` `sidecar()` + `MessageChannel` | 🔴 |
| shared process | `sharedProcess/electron-main` | Node sidecar window/worker | 🔴 |
| pty host | `terminal/electron-main/electronPtyHostStarter` | Node sidecar (keep `node-pty`) or `portable-pty` crate | 🟡 |
| extension host | `agentHost`, ext host starters | Node sidecar — **must stay Node for plugin compat** | 🔴 |

---

## 4. Phased plan (YAGNI — each phase ships & benchmarks)

- **Phase 0 — Spike (this PR).** Prove Monaco renders in the native webview;
  port the `vscode-file://` security handler to Rust with TDD. Establish the
  benchmark harness. ✔
- **Phase 1 — Boot a window. ✔ (this PR)** Rust main computes the workbench
  `vscode-file://` URL (ported `FileAccess.asBrowserUri` + the `windowImpl`
  `loadURL` choice of `workbench{,-dev}.html`) and opens one `WebviewWindow` at
  it, serving assets through the resolver — now including the
  COOP/COEP/Cache-Control/Document-Policy headers from
  `protocolMainService.handleResourceRequest`. No IPC yet → workbench will error
  on first `ipcRenderer` call; that's the Phase-2 boundary. Benchmark cold start
  vs Electron here.
- **Phase 2 — IPC transport swap.** Implement Tauri transport behind
  `IPCServer`/`IChannel`; bring up the small set of services the workbench needs
  to reach "empty window" (host, lifecycle, storage, log).
- **Phase 3 — Node sidecar.** Spawn extension host + shared process as Node
  sidecars; wire `MessagePort`. → plugins load.
- **Phase 4 — Native services.** dialog/menu/clipboard/shell/notification via
  Tauri plugins (mostly 🟢).
- **Phase 5 — Hard renderer parity.** `<webview>`/`BrowserView`, terminal,
  auxiliary windows, WebKitGTK fixes. The long tail.

A natural decision point after Phase 1–2: if WebKitGTK parity proves too costly,
fall back to Tauri's optional Chromium/Servo backend or keep Electron on Linux.

---

## 5. What the spike proves (and does not)

Proves (runnable on a dev machine with webkit installed):
- VS Code's editor core (Monaco) loads and renders in the OS-native webview.
- The `vscode-file://` scheme — VS Code's existing asset URL — can be served by
  a Rust handler with the **same security decision** as `protocolMainService.ts`
  (roots + media-extension allowlist + `..` traversal block), proven by tests.

Does **not** prove: full workbench boot, IPC, plugins, or Linux/WebKitGTK
parity. Those are Phases 1–5.

**In-container limitation:** this environment has no `webkit2gtk-4.1`/GTK dev
libs, no display, and apt mirrors are unreachable, so the Tauri GUI binary
cannot be compiled or run here. The **pure-Rust resolver crate builds and its
tests pass here** (`cargo test`), which is the security-critical logic. The GUI
crate is structured to `cargo tauri dev` on a normal dev machine.

---

## 6. Benchmark methodology (RAM / binary size / startup)

Harness: [`/bench/benchmark.sh`](../bench/benchmark.sh). It measures what *can* be
measured without a GUI (binary/installer size) and, on a real machine, peak RSS
and time-to-first-paint for both builds.

| Metric | How | Electron baseline | Tauri (to measure) |
|---|---|---|---|
| Installer size | size of packaged app | ~110–150 MB (bundled Chromium+Node) | target < 40 MB (no Chromium) |
| Idle RAM (empty window) | peak RSS of all procs | ~300–500 MB | target 1.5–3× lower |
| Cold startup | spawn → first paint | baseline | measure; native webview usually faster to init |

Numbers above are *expectations from public Electron-vs-Tauri comparisons*, not
measurements of this app — they are placeholders the harness fills in. The
honest comparison can only be produced once Phase 1 boots a real window on a
machine with a display; the harness is wired so that is a one-command run.
