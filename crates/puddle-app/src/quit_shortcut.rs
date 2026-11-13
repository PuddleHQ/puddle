// SPDX-License-Identifier: AGPL-3.0-or-later
//! The quit shortcut's question in the operating system's own dialog. What it says and when it asks
//! is [`crate::desktop::on_quit_pressed`], tested there; this is the part that needs the real
//! window system (the dialog plugin shows an OS dialog, the test runtime has none).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::backend::Backend;
use crate::desktop::on_quit_pressed;

/// Whether the question is on screen: one process has one shortcut and one question.
static ASKING: std::sync::LazyLock<Arc<AtomicBool>> = std::sync::LazyLock::new(Arc::default);

/// Ctrl+Q was pressed: the count comes from the backend, never from the page. The tray's Quit and
/// a close button set to quit do not come through here.
pub(crate) fn pressed<R: Runtime>(app: &AppHandle<R>) {
    let open = app
        .try_state::<Backend>()
        .map_or(0, |backend| backend.open_terminals());
    let quitting = app.clone();
    let dialogs = app.clone();
    on_quit_pressed(
        open,
        &ASKING,
        move |question, answer| {
            dialogs
                .dialog()
                .message(question)
                .title("Quit Puddle")
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "Quit".to_owned(),
                    "Cancel".to_owned(),
                ))
                .show(answer);
        },
        move || quitting.exit(0),
    );
}
