// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer Abdullah and Concat contributors
// SPDX-FileCopyrightText: 2026 Xưởng Dựng Video contributors

//! Xưởng Dựng Video's live mode, the window's half: what the API's thread
//! calls to edit the project on screen (see `concat_api::Live`).
//!
//! The API runs on a thread of its own; the window's state lives on the
//! event loop's. Each call is handed over with `invoke_from_event_loop`
//! and waited for here, on the API's thread, so the agent's edit runs
//! exactly where the person's would, between two of their own, and the
//! window draws it at once.

use std::sync::mpsc;
use std::time::Duration;

use concat_api::{ApiError, ErrorCode, Request, Response};
use concat_project::DocumentSettings;

use crate::host::Shell;

/// How long a call waits for the window before it gives up: a modal
/// dialog or a long gesture can hold the event loop, never this long.
const PATIENCE: Duration = Duration::from_secs(30);

/// The window, as the API's [`concat_api::Live`].
pub struct WindowLive;

impl concat_api::Live for WindowLive {
    fn call(&self, request: Request) -> Response {
        on_window(move |shell, app| {
            let response = shell.studio.borrow_mut().agent_call(request);
            shell.studio.borrow().publish(&app, &shell.models);
            response
        })
        .unwrap_or_else(|| {
            Response::Error(ApiError::new(
                ErrorCode::Failed,
                "the window did not answer in time: close any dialog that is open and try again",
            ))
        })
    }

    fn snapshot(&self, _path: &str) -> Option<(DocumentSettings, serde_json::Value)> {
        // The API only asks for the folder the window holds, and the
        // window holds one project at a time.
        on_window(|shell, _| {
            let studio = shell.studio.borrow();
            let session = studio.session.as_ref()?;
            Some((session.settings(), session.document()))
        })
        .flatten()
    }
}

/// Runs `body` on the event loop and waits for what it returns. `None`
/// when the window is gone or did not answer within [`PATIENCE`].
fn on_window<T: Send + 'static>(
    body: impl FnOnce(&Shell, crate::ui::App) -> T + Send + 'static,
) -> Option<T> {
    let (send, receive) = mpsc::channel();
    slint::invoke_from_event_loop(move || {
        Shell::with(|shell, app| {
            let _ = send.send(body(shell, app));
        });
    })
    .ok()?;
    receive.recv_timeout(PATIENCE).ok()
}
