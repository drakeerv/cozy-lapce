use std::{path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use anyhow::Result;
use floem::{
    ViewId,
    action::{TimerToken, exec_after},
    peniko::kurbo::{Point, Size},
    reactive::{
        ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        use_context,
    },
    window::WindowId,
};
use lapce_core::{directory::Directory, workspace::LapceWorkspace};
use log::error;
use serde::{Deserialize, Serialize};

use crate::{
    app::AppCommand,
    command::{InternalCommand, WindowCommand},
    config::LapceConfig,
    db::LapceDb,
    keypress::EventRef,
    listener::Listener,
    local_task::LocalTaskRequester,
    update::ReleaseInfo,
    window_workspace::{CommonData, Focus, SignalManager, WindowWorkspaceData},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub size:      Size,
    pub pos:       Point,
    pub maximised: bool,
    #[serde(default)]
    pub workspace: Arc<LapceWorkspace>,
}

#[derive(Clone)]
pub struct WindowCommonData {
    pub window_command:           Listener<WindowCommand>,
    pub window_scale:             RwSignal<f64>,
    pub size:                     RwSignal<Size>,
    pub window_maximized:         RwSignal<bool>,
    pub window_tab_header_height: RwSignal<f64>,
    pub latest_release:           ReadSignal<Option<ReleaseInfo>>,
    pub ime_allowed:              RwSignal<bool>,
    // pub cursor_blink_timer:       RwSignal<TimerToken>,
    // the value to be update by curosr blinking
    pub hide_cursor:              RwSignal<bool>,
    pub app_view_id:              RwSignal<ViewId>,
    pub extra_plugin_paths:       Arc<Vec<PathBuf>>,
}

/// `WindowData` is the application model for a top-level window.
///
/// A top-level window can be independently moved around and
/// resized using your window manager. Normally Lapce has only one
/// top-level window, but new ones can be created using the "New Window"
/// command.
///
/// Each window has its own collection of "window tabs" (again, there is
/// normally only one window tab), size, position etc.
#[derive(Clone)]
pub struct WindowData {
    pub window_id:    WindowId,
    pub scope:        Scope,
    /// The set of tabs within the window. These tabs are high-level
    /// constructs for workspaces, in particular they are not **editor tabs**.
    pub window_tabs:  RwSignal<WindowWorkspaceData>,
    /// The index of the active window tab.
    // pub active: RwSignal<usize>,
    pub app_command: Listener<AppCommand>,
    pub position:     RwSignal<Point>,
    pub root_view_id: RwSignal<ViewId>,
    pub window_scale: RwSignal<f64>,
    pub config:       RwSignal<LapceConfig>,
    pub ime_enabled:  RwSignal<bool>,
    pub common:       Rc<WindowCommonData>,
    pub directory:    Directory,
    pub local_task:   LocalTaskRequester, /* pub watcher:      Arc<RwLock<notify::RecommendedWatcher>> */
}

impl WindowData {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        window_id: WindowId,
        app_view_id: RwSignal<ViewId>,
        info: WindowInfo,
        window_scale: RwSignal<f64>,
        latest_release: ReadSignal<Option<ReleaseInfo>>,
        extra_plugin_paths: Arc<Vec<PathBuf>>,
        app_command: Listener<AppCommand>,
        directory: &Directory,
        local_task: LocalTaskRequester,
        config: RwSignal<LapceConfig>, /* watcher: Arc<RwLock<notify::RecommendedWatcher>> */
    ) -> Result<Self> {
        let cx = Scope::new();
        // let config =
        //     LapceConfig::load(&LapceWorkspace::default(), &[],
        // &extra_plugin_paths, directory); let config =
        // cx.create_rw_signal(config);
        let root_view_id = cx.create_rw_signal(ViewId::new());

        let window_command = Listener::new_empty(cx);
        let ime_allowed = cx.create_rw_signal(false);
        let window_maximized = cx.create_rw_signal(false);
        let size = cx.create_rw_signal(Size::ZERO);
        let window_tab_header_height = cx.create_rw_signal(0.0);

        let hide_cursor = cx.create_rw_signal(false);

        let common = Rc::new(WindowCommonData {
            window_command,
            window_scale,
            size,
            window_maximized,
            window_tab_header_height,
            latest_release,
            ime_allowed,
            // cursor_blink_timer,
            hide_cursor,
            app_view_id,
            extra_plugin_paths,
        });

        //
        // let cursor_info = CursorInfo {
        //     blink_interval: Rc::new(move || config.editor.blink_interval()),
        //     blink_timer:    common.cursor_blink_timer,
        //     hidden:         common.hide_cursor,
        //     should_blink:   Rc::new(crate::doc::should_blink(
        //         common.focus,
        //         common.keyboard_focus
        //     ))
        // };

        let w = info.workspace.clone();
        log::info!("WindowData {:?} window_id={}", w, window_id.into_raw());
        // w.watch_project_setting(&watcher);
        let window_tabs = cx.create_rw_signal(WindowWorkspaceData::new(
            cx,
            w,
            common.clone(),
            directory,
            local_task.clone(),
            config,
        )?);

        // for w in info.tabs.workspaces {
        //     log::info!("WindowData {:?}", w);
        //     w.watch_project_setting(&watcher);
        //
        //     let window_tab =
        //         Rc::new(WindowTabData::new(cx, Arc::new(w), common.clone()));
        //     window_tabs.update(|window_tabs| {
        //         window_tabs.push_back((cx.create_rw_signal(0), window_tab));
        //     });
        // }

        // if window_tabs.with_untracked(|window_tabs| window_tabs.is_empty()) {
        //     let window_tab = Rc::new(WindowTabData::new(
        //         cx,
        //         Arc::new(LapceWorkspace::default()),
        //         common.clone(),
        //     ));
        //     window_tabs.update(|window_tabs| {
        //         window_tabs.push_back((cx.create_rw_signal(0), window_tab));
        //     });
        // }

        // let active = cx.create_rw_signal(active);
        let position = cx.create_rw_signal(info.pos);

        let window_data = Self {
            window_id,
            scope: cx,
            window_tabs,
            position,
            root_view_id,
            window_scale,
            app_command,
            config,
            ime_enabled: cx.create_rw_signal(false),
            common,
            directory: directory.clone(),
            local_task, // watcher
        };

        {
            let window_data = window_data.clone();
            window_data.common.window_command.listen(move |cmd| {
                if let Err(err) = window_data.run_window_command(cmd) {
                    error!("{}", err.to_string());
                }
            });
        }

        // {
        //     cx.create_effect(move |_| {
        //         let active = active.get();
        //         let tab = window_tabs
        //             .with(|tabs| tabs.get(active).map(|(_, tab)| tab.clone()));
        //         if let Some(tab) = tab {
        //             tab.common
        //                 .internal_command
        //                 .send(InternalCommand::ResetBlinkCursor);
        //         }
        //     })
        // }

        Ok(window_data)
    }

    pub fn reload_config(&self) {
        let config = LapceConfig::load(
            &LapceWorkspace::default(),
            &[],
            &self.common.extra_plugin_paths,
            &self.directory,
        );
        self.config.set(config);
        self.window_tabs.with_untracked(|x| x.reload_config());
    }

    pub fn run_window_command(&self, cmd: WindowCommand) -> Result<()> {
        match cmd {
            WindowCommand::SetWorkspace { workspace } => {
                let db: Arc<LapceDb> = use_context().unwrap();
                db.update_recent_workspace(workspace.clone(), &self.local_task);
                if let Err(err) =
                    db.insert_window_tab(self.window_tabs.get_untracked().clone())
                {
                    log::error!("{:?}", err);
                }
                log::info!("SetWorkspace {:?}", workspace);
                let window_tab = WindowWorkspaceData::new(
                    self.scope,
                    workspace.clone(),
                    self.common.clone(),
                    &self.directory,
                    self.local_task.clone(),
                    self.config,
                )?;

                self.window_tabs.set(window_tab);
                // workspace.watch_project_setting(&self.watcher);
            },
            WindowCommand::NewWorkspaceTab {
                workspace,
                end: _end,
            } => {
                let db: Arc<LapceDb> = use_context().unwrap();
                db.update_recent_workspace(workspace.clone(), &self.local_task);
                log::info!("NewWorkspaceTab {:?}", workspace);
                // workspace.watch_project_setting(&self.watcher);
                let window_tab = WindowWorkspaceData::new(
                    self.scope,
                    workspace,
                    self.common.clone(),
                    &self.directory,
                    self.local_task.clone(),
                    self.config,
                )?;
                self.window_tabs.set(window_tab);
                // let active = self.active.get_untracked();
                // let active = self
                //     .window_tabs
                //     .try_update(|tabs| {
                //         if end || tabs.is_empty() {
                //             tabs.push_back((
                //                 self.scope.create_rw_signal(0),
                //                 window_tab,
                //             ));
                //             tabs.len() - 1
                //         } else {
                //             let index = tabs.len().min(active + 1);
                //             tabs.insert(
                //                 index,
                //                 (self.scope.create_rw_signal(0), window_tab),
                //             );
                //             index
                //         }
                //     })
                //     .unwrap();
                // self.active.set(active);
            },
            WindowCommand::CloseWorkspaceTab { index: _index } => {
                // let active = self.active.get_untracked();
                // let index = index.unwrap_or(active);
                // self.window_tabs.update(|window_tabs| {
                //     if window_tabs.len() < 2 {
                //         return;
                //     }
                //
                //     if index < window_tabs.len() {
                //         let (_, old_window_tab) = window_tabs.remove(index);
                //         old_window_tab.proxy.shutdown();
                //         let db: Arc<LapceDb> = use_context().unwrap();
                //         if let Err(err) = db.save_window_tab(old_window_tab)
                // {             log::error!("{:?}", err);
                //         }
                //     }
                // });
                //
                // let tabs_len = self.window_tabs.with_untracked(|tabs|
                // tabs.len());
                //
                // if active > index && active > 0 {
                //     self.active.set(active - 1);
                // } else if active >= tabs_len.saturating_sub(1) {
                //     self.active.set(tabs_len.saturating_sub(1));
                // }
            },
            WindowCommand::NextWorkspaceTab => {
                // let active = self.active.get_untracked();
                // let tabs_len = self.window_tabs.with_untracked(|tabs|
                // tabs.len()); if tabs_len > 1 {
                //     let active = if active >= tabs_len - 1 {
                //         0
                //     } else {
                //         active + 1
                //     };
                //     self.active.set(active);
                // }
            },
            WindowCommand::PreviousWorkspaceTab => {
                // let active = self.active.get_untracked();
                // let tabs_len = self.window_tabs.with_untracked(|tabs|
                // tabs.len()); if tabs_len > 1 {
                //     let active = if active == 0 {
                //         tabs_len - 1
                //     } else {
                //         active - 1
                //     };
                //     self.active.set(active);
                // }
            },
            WindowCommand::NewWindow => {
                self.app_command
                    .send(AppCommand::NewWindow { folder: None });
            },
            WindowCommand::CloseWindow => {
                self.app_command
                    .send(AppCommand::CloseWindow(self.window_id));
            },
        }
        self.app_command.send(AppCommand::SaveApp);
        Ok(())
    }

    pub fn key_down<'a>(&self, event: impl Into<EventRef<'a>> + Copy) -> bool {
        self.window_tabs.get_untracked().key_down(event)
    }

    pub fn info(&self) -> WindowInfo {
        let workspace: Arc<LapceWorkspace> =
            self.window_tabs.get_untracked().workspace.clone();
        WindowInfo {
            size: self.common.size.get_untracked(),
            pos: self.position.get_untracked(),
            maximised: false,
            workspace,
        }
    }

    pub fn active_window_tab(&self) -> WindowWorkspaceData {
        self.window_tabs.get_untracked()
    }

    // pub fn move_tab(&self, from_index: usize, to_index: usize) {
    //     if from_index == to_index {
    //         return;
    //     }
    //
    //     let to_index = if from_index < to_index {
    //         to_index - 1
    //     } else {
    //         to_index
    //     };
    //     self.window_tabs.update(|tabs| {
    //         let tab = tabs.remove(from_index);
    //         tabs.insert(to_index, tab);
    //     });
    //     self.active.set(to_index);
    // }
}

#[derive(Clone)]
pub struct CursorBlink {
    pub hide_cursor:    RwSignal<bool>,
    pub blink_timer:    RwSignal<TimerToken>,
    pub blink_interval: RwSignal<u64>,
    pub common_data:    Rc<CommonData>,
}

impl CursorBlink {
    pub fn blink(&self, hidden: Option<bool>) {
        let mut blink_interval = self.blink_interval.get_untracked();
        if blink_interval == 0 {
            return;
        } else if blink_interval < 500 {
            blink_interval = 500;
        }
        // log::info!("CursorBlink {}", blink_interval);
        let info = self.clone();
        let blink_timer = info.blink_timer;
        let timer_token =
            exec_after(Duration::from_millis(blink_interval), move |timer_token| {
                let blink_timer_token = blink_timer.try_get_untracked();
                if blink_timer_token == Some(timer_token) {
                    if let Some(hidden) = hidden {
                        info.hide_cursor.set(hidden);
                    } else {
                        info.hide_cursor.update(|hide| {
                            *hide = !*hide;
                        });
                    }
                    if should_blink(
                        info.common_data.focus,
                        info.common_data.keyboard_focus,
                    ) {
                        info.common_data
                            .internal_command
                            .send(InternalCommand::BlinkCursor);
                    }
                    info.blink(None);
                    // } else {
                    //     warn!("blink_timer not equal {:?} {:?} id={:?}",
                    // blink_timer_token, timer_token,
                    // floem::prelude::SignalGet::id(&blink_timer));
                }
            });
        // warn!("set id={:?} {:?}", floem::prelude::SignalGet::id(&blink_timer),
        // timer_token);
        blink_timer.set(timer_token);
    }

    pub fn blink_right_now(&self) {
        self.hide_cursor.set(false);
        self.common_data
            .internal_command
            .send(InternalCommand::BlinkCursor);
        self.blink(None);
    }
}

pub fn should_blink(
    _focus: SignalManager<Focus>,
    _keyboard_focus: RwSignal<Option<ViewId>>,
) -> bool {
    let Some(focus) = _focus.try_get_untracked() else {
        return false;
    };
    if matches!(
        focus,
        Focus::Workbench
            | Focus::Palette
            | Focus::Panel(lapce_core::panel::PanelKind::Plugin)
            | Focus::Panel(lapce_core::panel::PanelKind::Search)
            | Focus::Panel(lapce_core::panel::PanelKind::SourceControl)
            | Focus::Panel(lapce_core::panel::PanelKind::Terminal)
    ) {
        return true;
    }

    if _keyboard_focus.get_untracked().is_some() {
        return true;
    }
    false
}
