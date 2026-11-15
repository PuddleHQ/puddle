// SPDX-License-Identifier: AGPL-3.0-or-later
//! The WebView2 environment every Puddle window starts from. Puddle builds it itself, instead of
//! leaving it to the webview library, for one setting the library cannot reach: custom crash
//! reporting. With it on, the browser still writes a crash dump into the window's data folder but
//! never uploads one to Microsoft. Everything else is what the library would have set.

use std::path::Path;
use std::sync::mpsc;

use tauri::{Manager, Runtime};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2Environment,
    ICoreWebView2EnvironmentOptions,
};
use webview2_com::{CoreWebView2EnvironmentOptions, CreateCoreWebView2EnvironmentCompletedHandler};
use windows::Win32::Globalization::{
    GetUserDefaultUILanguage, LCIDToLocaleName, LOCALE_ALLOW_NEUTRAL_NAMES,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::core::{HSTRING, PCWSTR};

/// The browser arguments a window gets when nothing asks for more: the library's own defaults
/// (wry 0.57: the mini menu and the `SmartScreen` filter off, autoplay allowed). An argument string replaces
/// them, so a caller that adds an argument starts from this text.
pub(crate) const DEFAULT_ARGUMENTS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required";

/// The browser arguments of a window: the defaults, or WebView2's DevTools Protocol open on
/// `debug_port` (for the scripted tests; the first argument is the library's default again).
pub(crate) fn arguments(debug_port: Option<u16>) -> Option<String> {
    debug_port.map(|port| {
        format!(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port={port} --remote-allow-origins=*"
        )
    })
}

/// The environment options for a window: `arguments` as given, the language the library would
/// pick, and custom crash reporting on.
pub(crate) fn options(arguments: &str, language: &str) -> CoreWebView2EnvironmentOptions {
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY: the setters write plain strings and a boolean into the options object that this
    // function owns, before anything else holds it.
    #[expect(unsafe_code, reason = "webview2-com marks its option setters unsafe")]
    unsafe {
        options.set_additional_browser_arguments(arguments.to_owned());
        options.set_language(language.to_owned());
        options.set_is_custom_crash_reporting_enabled(true);
    }
    options
}

/// The user's interface language as a locale name such as `nl-BE`, which is what the library
/// hands WebView2 too. Empty when the system does not know one.
fn ui_language() -> String {
    let mut name = [0_u16; 85];
    // SAFETY: `name` is a live buffer of the size Windows documents for a locale name
    // (`LOCALE_NAME_MAX_LENGTH`), and the call only writes into it.
    #[expect(unsafe_code, reason = "Win32 call with no safe wrapper")]
    let length = unsafe {
        LCIDToLocaleName(
            u32::from(GetUserDefaultUILanguage()),
            Some(&mut name),
            LOCALE_ALLOW_NEUTRAL_NAMES,
        )
    };
    let length = usize::try_from(length).unwrap_or(0).saturating_sub(1);
    String::from_utf16_lossy(name.get(..length).unwrap_or_default())
}

/// An environment on its way from the UI thread to the thread that asked for it.
struct Carried(webview2_com::Result<ICoreWebView2Environment>);

// SAFETY: the environment is only carried, never called, on the way back: it is created on the UI
// thread and handed to the window builder, which passes it to that same thread again to build the
// webview (the same hand-over the library does with an opener's environment for a new window).
#[expect(unsafe_code, reason = "a COM interface pointer is not Send by type")]
unsafe impl Send for Carried {}

/// Creates the environment for a window whose data folder is `data_folder` (`None`: WebView2's
/// default folder, as without an environment of our own). Runs on the thread that owns the UI,
/// which COM needs, and waits for it from any other. `None` when it cannot be created: the window
/// then opens on the library's own environment, which uploads crash reports, and a warning says so.
pub(crate) fn create<R: Runtime, M: Manager<R>>(
    manager: &M,
    data_folder: Option<&Path>,
    arguments: &str,
) -> Option<ICoreWebView2Environment> {
    let (sender, receiver) = mpsc::channel();
    let (folder, arguments) = (data_folder.map(HSTRING::from), arguments.to_owned());
    let asked = manager.app_handle().run_on_main_thread(move || {
        // the caller waiting on the other end may be gone; then nobody wants the answer
        let _ = sender.send(Carried(build(&folder.unwrap_or_default(), &arguments)));
    });
    let outcome = match asked {
        Ok(()) => receiver.recv().ok(),
        Err(err) => {
            tracing::warn!(%err, "no WebView2 environment of our own: the window uses the default, which uploads crash reports");
            return None;
        }
    };
    match outcome {
        Some(Carried(Ok(environment))) => {
            tracing::info!("WebView2 environment with custom crash reporting created");
            Some(environment)
        }
        Some(Carried(Err(err))) => {
            tracing::warn!(%err, "no WebView2 environment of our own: the window uses the default, which uploads crash reports");
            None
        }
        None => None,
    }
}

/// Asks WebView2 for the environment and waits for the answer, pumping the thread's messages.
fn build(folder: &HSTRING, arguments: &str) -> webview2_com::Result<ICoreWebView2Environment> {
    // The library does this itself before it creates an environment; our call comes before it
    // has run on a thread that has not been through it yet (the setup of the app). A thread that
    // already has an apartment answers with a code that is not a failure here.
    // SAFETY: no pointer is passed; this only enters the calling thread into a COM apartment.
    #[expect(unsafe_code, reason = "Win32 call with no safe wrapper")]
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let (sender, receiver) = mpsc::channel();
    let options = options(arguments, &ui_language());
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        move |error, environment| {
            let result = (|| {
                error?;
                environment.ok_or_else(|| {
                    windows::core::Error::from(windows::Win32::Foundation::E_POINTER).into()
                })
            })();
            sender
                .send(result)
                .map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_UNEXPECTED))
        },
    ));
    // SAFETY: the folder and the options outlive the call; the handler is moved into it.
    #[expect(unsafe_code, reason = "WebView2 COM call with no safe wrapper")]
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            folder,
            &ICoreWebView2EnvironmentOptions::from(options),
            &handler,
        )?;
    }
    webview2_com::wait_with_pump(receiver)?
}

#[cfg(test)]
mod tests;
