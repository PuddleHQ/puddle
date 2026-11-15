// SPDX-License-Identifier: AGPL-3.0-or-later
//! WebView2's own shortcuts (F5 and Ctrl+R reload, F7's caret-browsing prompt, print, find) off, so
//! the editor and the terminal get every key. The setting has no portable counterpart: WebKitGTK
//! has none to turn off, and the terminal page cancels those defaults itself there.

use tauri::{Runtime, WebviewWindow};
use webview2_com::AcceleratorKeyPressedEventHandler;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_KEY_EVENT_KIND, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, ICoreWebView2Settings3,
};
use windows::core::Interface;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};

/// Turns the browser accelerator keys off in `window`'s webview. A failure is logged: the window
/// still works, with the browser's shortcuts on.
pub(crate) fn turn_off<R: Runtime>(window: &WebviewWindow<R>) {
    let label = window.label().to_owned();
    let asked = window.with_webview(move |webview| {
        // SAFETY: `controller()` is the live controller of this window's webview, called on
        // the thread that owns it (`with_webview` runs this closure there); the calls only read
        // the settings object it returns and set one boolean on it.
        #[expect(unsafe_code, reason = "WebView2 COM call with no safe wrapper")]
        let done = unsafe {
            webview
                .controller()
                .CoreWebView2()
                .and_then(|core| core.Settings())
                .and_then(|settings| settings.cast::<ICoreWebView2Settings3>())
                .and_then(|settings| settings.SetAreBrowserAcceleratorKeysEnabled(false))
        };
        if let Err(err) = done {
            tracing::warn!(%err, label, "the browser shortcuts stay on in a window");
        }
    });
    if let Err(err) = asked {
        tracing::warn!(%err, "the browser shortcuts stay on in a window");
    }
}

/// Whether a key event is Ctrl+Q going down: no other modifier, and not the release.
fn is_quit_chord(
    kind: COREWEBVIEW2_KEY_EVENT_KIND,
    key: u32,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> bool {
    kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && key == u32::from(b'Q') && ctrl && !shift && !alt
}

/// Whether `key` is held down now.
fn held(key: u16) -> bool {
    // SAFETY: `GetKeyState` reads the calling thread's keyboard state and has no preconditions.
    #[expect(unsafe_code, reason = "Win32 call with no safe wrapper")]
    let state = unsafe { GetKeyState(i32::from(key)) };
    state < 0
}

/// Calls `pressed` when Ctrl+Q goes down in `window`'s webview, and keeps the key from the page.
/// The window has no menu bar, so the quit entry's accelerator never fires there; the webview
/// hands every key to the host first, and this is where the host takes this one. A failure is
/// logged: the shortcut then does nothing on this window.
pub(crate) fn on_ctrl_q<R: Runtime>(
    window: &WebviewWindow<R>,
    pressed: impl Fn() + Send + Sync + 'static,
) {
    let asked = window.with_webview(move |webview| {
        let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
            let mut key = 0_u32;
            // SAFETY: `args` is the live event argument the webview passed to this callback; the
            // calls write one integer each into locals that outlive them.
            #[expect(unsafe_code, reason = "WebView2 COM calls with no safe wrapper")]
            unsafe {
                args.KeyEventKind(&raw mut kind)?;
                args.VirtualKey(&raw mut key)?;
            }
            if is_quit_chord(kind, key, held(VK_CONTROL), held(VK_SHIFT), held(VK_MENU)) {
                // SAFETY: as above; one boolean set on the same event.
                #[expect(unsafe_code, reason = "WebView2 COM call with no safe wrapper")]
                unsafe {
                    args.SetHandled(true)?;
                }
                pressed();
            }
            Ok(())
        }));
        let mut token = 0_i64;
        // SAFETY: `controller()` is the live controller of this window's webview, on the thread
        // that owns it; the handler lives as long as the webview holds it (the window's life).
        #[expect(unsafe_code, reason = "WebView2 COM call with no safe wrapper")]
        let added = unsafe {
            webview
                .controller()
                .add_AcceleratorKeyPressed(&handler, &raw mut token)
        };
        if let Err(err) = added {
            tracing::warn!(%err, "Ctrl+Q will not reach the quit question on this window");
        }
    });
    if let Err(err) = asked {
        tracing::warn!(%err, "Ctrl+Q will not reach the quit question on this window");
    }
}

#[cfg(test)]
mod tests;
