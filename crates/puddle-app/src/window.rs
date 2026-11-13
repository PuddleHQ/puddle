// SPDX-License-Identifier: AGPL-3.0-or-later
//! The main window: its label, its start-up script and how it is built.

use std::sync::Arc;

use tauri::webview::NewWindowResponse;
use tauri::{Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use url::Url;

use crate::navigation::{Navigation, OriginPolicy};

/// The main window's label. Puddle's one capability file names this label and grants it the folder dialog only;
/// workspace windows get labels of their own (`vs-<workspace>`), never this one.
pub const MAIN_LABEL: &str = "main";

/// Hands a web address to the default browser. In the app this is the opener plugin called from
/// Rust; tests pass a recorder.
pub type OpenExternal = Arc<dyn Fn(&Url) + Send + Sync>;

/// Tells the user a link was refused (see [`crate::links`]). Tests pass a recorder.
pub type Refused = Arc<dyn Fn(&Url) + Send + Sync>;

/// The script that runs before the page's own scripts, in the main window's top-level frame only.
///
/// It freezes `Object.prototype`: the SPA is Puddle's own bundle and writes to no built-in, so a
/// polluted prototype can only come from a bug. Tauri's `freezePrototype` setting is off because it is one
/// switch for every webview of the app, and the workspace editor windows need a writable prototype (VS
/// Code assigns `toString` on plain objects). Then it sets `window.__PUDDLE__ = { token }`, the one way
/// the SPA learns the API token (see `ui/src/lib/api/connection.ts`). The
/// property can't be reassigned or deleted by the page. The script does nothing unless the page's
/// origin is the API's, so a navigation that somehow got elsewhere would see no token.
#[must_use]
pub fn init_script(origin: &str, token: &str) -> String {
    // `serde_json` writes a string literal that is also a valid JavaScript string literal, so a
    // value with quotes, backslashes or line breaks stays inside it.
    let origin = serde_json::Value::from(origin);
    let token = serde_json::Value::from(token);
    format!(
        "Object.freeze(Object.prototype);\n\
         (function () {{\n\
         \x20 if (window.location.origin !== {origin}) return;\n\
         \x20 Object.defineProperty(window, \"__PUDDLE__\", {{\n\
         \x20   value: Object.freeze({{ token: {token} }}),\n\
         \x20 }});\n\
         }})();\n"
    )
}

/// The navigation callback for the main window: keeps it on the allowed origins, sends web
/// addresses elsewhere to `open` and tells `refused` about the rest.
pub fn navigation_handler(
    policy: OriginPolicy,
    open: OpenExternal,
    refused: Refused,
) -> impl Fn(&Url) -> bool + Send + Sync + 'static {
    move |target| match policy.classify(target) {
        Navigation::Stay => true,
        Navigation::External => {
            open(target);
            false
        }
        Navigation::Refuse => {
            tracing::warn!(scheme = target.scheme(), "main window navigation refused");
            refused(target);
            false
        }
    }
}

/// What a request for a new window (a `target="_blank"` link) does: web addresses go to the
/// browser, refused ones are reported, and the window's own pages are dropped.
fn new_window_handler(policy: &OriginPolicy, target: &Url, open: &OpenExternal, refused: &Refused) {
    match policy.classify(target) {
        Navigation::External => open(target),
        Navigation::Refuse => refused(target),
        Navigation::Stay => {}
    }
}

/// Opens the main window on `url` with the token handed over. Returns the existing window if
/// there is one. `debug_port` opens WebView2's DevTools Protocol on that port (Windows, for the
/// smoke test; ignored elsewhere).
///
/// # Errors
///
/// Whatever Tauri reports when the window can't be created.
pub fn open_main<R: Runtime, M: Manager<R>>(
    manager: &M,
    url: &Url,
    token: &str,
    dev_url: Option<&Url>,
    debug_port: Option<u16>,
    open: OpenExternal,
    refused: Refused,
) -> tauri::Result<WebviewWindow<R>> {
    if let Some(existing) = manager.get_webview_window(MAIN_LABEL) {
        return Ok(existing);
    }
    let policy = OriginPolicy::new(url, dev_url);
    let start = dev_url.unwrap_or(url).clone();
    let origin = start.origin().ascii_serialization();
    let on_new_window_open = open.clone();
    let builder = WebviewWindowBuilder::new(manager, MAIN_LABEL, WebviewUrl::External(start))
        .title("Puddle")
        .inner_size(1280.0, 800.0)
        .min_inner_size(900.0, 600.0)
        .initialization_script(init_script(&origin, token))
        .on_navigation(navigation_handler(policy.clone(), open, refused.clone()))
        .on_new_window(move |target, _features| {
            new_window_handler(&policy, &target, &on_new_window_open, &refused);
            // Never `Allow`: that is the webview's own pop-up, without our handlers.
            NewWindowResponse::Deny
        });
    #[cfg(windows)]
    let builder = match debug_port {
        // WebView2 only takes this from the API (the environment variable is ignored once the
        // API sets arguments, which wry always does); the first two are wry's own defaults.
        Some(port) => builder.additional_browser_args(&format!(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port={port} --remote-allow-origins=*"
        )),
        None => builder,
    };
    #[cfg(not(windows))]
    let _ = debug_port;
    let window = builder.build()?;
    // The terminal needs F5, Ctrl+R, Ctrl+P and Ctrl+F; WebKitGTK has no such switch.
    #[cfg(windows)]
    {
        crate::accelerators::turn_off(&window);
        let app = manager.app_handle().clone();
        crate::accelerators::on_ctrl_q(&window, move || {
            crate::quit_shortcut::pressed(&app);
        });
    }
    Ok(window)
}

#[cfg(test)]
mod tests;
