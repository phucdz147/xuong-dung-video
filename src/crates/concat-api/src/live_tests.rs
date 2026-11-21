// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Xưởng Dựng Video contributors

//! Live mode: a request on the folder the window holds goes to the window
//! ([`Live`]), and every other folder is the API's own as before.

use super::*;

/// A window that keeps one session of its own and records what it was
/// asked, standing in for the real one's event loop.
struct FakeWindow {
    session: Mutex<Session>,
    calls: Mutex<Vec<String>>,
}

impl Live for FakeWindow {
    fn call(&self, request: Request) -> Response {
        let mut session = self.session.lock().unwrap();
        let name = format!("{request:?}")
            .split([' ', '{'])
            .next()
            .unwrap_or("")
            .to_owned();
        self.calls.lock().unwrap().push(name);
        let view = match request {
            Request::ProjectGet { .. } => Ok(session.view()),
            Request::EditApply { command, .. } => session.apply(*command).map_err(refused),
            Request::EditUndo { .. } => Ok(session.undo()),
            _ => Err(refused("not in this test".to_owned())),
        };
        view.map(|view| Reply::View(Box::new(view))).into()
    }

    fn snapshot(
        &self,
        _path: &str,
    ) -> Option<(concat_project::DocumentSettings, serde_json::Value)> {
        let session = self.session.lock().unwrap();
        Some((session.settings(), session.document()))
    }
}

fn answered(response: Response) -> Reply {
    match response {
        Response::Result(reply) => reply,
        Response::Error(error) => panic!("refused: {error}"),
    }
}

fn tracks(reply: Reply) -> usize {
    match reply {
        Reply::View(view) => view.project.active().tracks.len(),
        _ => panic!("not a view"),
    }
}

#[test]
fn the_project_on_screen_is_edited_in_the_window() {
    let scratch = tempfile::tempdir().expect("scratch");
    let dirs = AppDirs {
        config: scratch.path().join("config"),
        data: scratch.path().join("data"),
    };
    let mut api = Api::with_dirs(dirs, Arc::new(|_event: Event| {}));
    // The window's project: made, then handed to the window, which holds it.
    answered(api.dispatch(Request::ProjectCreate {
        location: scratch.path().to_string_lossy().into_owned(),
        name: "on-screen".to_owned(),
        video: None,
    }));
    let path = scratch
        .path()
        .join("on-screen")
        .to_string_lossy()
        .into_owned();
    answered(api.dispatch(Request::ProjectClose {
        path: path.clone(),
        save: true,
    }));
    let info = projects::list(&api.dirs().config)
        .into_iter()
        .find(|info| info.name == "on-screen")
        .expect("in recents");
    let open = OpenProjects::default();
    open.claim(&path, Holder::Window)
        .expect("the window holds it");
    api.share_open_projects(open.clone());
    let window = Arc::new(FakeWindow {
        session: Mutex::new(Session::open_info(&info).expect("opens")),
        calls: Mutex::new(Vec::new()),
    });
    api.share_live(window.clone());

    // Opened through the window, not refused.
    let before = tracks(answered(
        api.dispatch(Request::ProjectOpen { path: path.clone() }),
    ));
    // An edit lands in the window's session.
    let after = tracks(answered(api.dispatch(Request::EditApply {
        path: path.clone(),
        command: Box::new(Command::AddTrack { bottom: false }),
    })));
    assert_eq!(after, before + 1);
    assert_eq!(
        window
            .session
            .lock()
            .unwrap()
            .project()
            .active()
            .tracks
            .len(),
        after
    );
    // Undo is the window's too.
    assert_eq!(
        tracks(answered(
            api.dispatch(Request::EditUndo { path: path.clone() })
        )),
        before
    );
    // Closing it from here is refused; it stays the window's.
    assert!(matches!(
        api.dispatch(Request::ProjectClose {
            path: path.clone(),
            save: false
        }),
        Response::Error(_)
    ));
    assert_eq!(open.holder(&path), Some(Holder::Window));
    assert_eq!(
        *window.calls.lock().unwrap(),
        vec!["ProjectGet", "EditApply", "EditUndo"]
    );
}
