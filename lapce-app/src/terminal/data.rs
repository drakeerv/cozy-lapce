use std::{collections::HashMap, path::PathBuf, rc::Rc, sync::Arc};

use alacritty_terminal::{
    Term,
    grid::{Dimensions, Scroll},
    selection::{Selection, SelectionType},
    term::{TermMode, test::TermSize},
    vi_mode::ViMotion
};
use anyhow::anyhow;
use doc::lines::{
    command::{EditCommand, FocusCommand, ScrollCommand},
    editor_command::CommandExecuted,
    mode::{Mode, VisualMode},
    movement::{LinePosition, Movement},
    register::Clipboard,
    text::SystemClipboard
};
use floem::{
    ViewId,
    keyboard::{Key, KeyEvent, Modifiers, NamedKey},
    reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith}
};
use lapce_core::{
    debug::{RunDebugMode, RunDebugProcess},
    workspace::LapceWorkspace
};
use lapce_rpc::{
    dap_types::RunDebugConfig,
    terminal::{TermId, TerminalProfile}
};
use parking_lot::RwLock;
use url::Url;
use lapce_core::icon::LapceIcons;
use super::{
    event::TermEvent,
    raw::{EventProxy, RawTerminal}
};
use crate::{
    command::CommandKind,
    keypress::{KeyPressFocus, condition::Condition},
    window_workspace::CommonData
};

#[derive(Clone, Debug)]
pub struct TerminalData {
    pub scope:        Scope,
    pub term_id:      TermId,
    pub workspace:    LapceWorkspace,
    pub raw_id:       RwSignal<u64>,
    pub title:        RwSignal<String>,
    pub launch_error: RwSignal<Option<String>>,
    pub mode:         RwSignal<Mode>,
    pub visual_mode:  RwSignal<VisualMode>,
    pub raw:          RwSignal<Arc<RwLock<RawTerminal>>>,
    pub run_debug:    RwSignal<Option<RunDebugProcess>>,
    pub common:       Rc<CommonData>,
    pub view_id:      RwSignal<Option<ViewId>>
}

impl TerminalData {
    pub fn icon(&self) -> &'static str {
        let run_debug = self.run_debug;
        if let Some((mode, stopped)) = run_debug.with(|run_debug| {
            run_debug.as_ref().map(|r| (r.mode, r.stopped))
        }) {
            let svg = match (mode, stopped) {
                (RunDebugMode::Run, false) => LapceIcons::START,
                (RunDebugMode::Run, true) => LapceIcons::RUN_ERRORS,
                (RunDebugMode::Debug, false) => LapceIcons::DEBUG,
                (RunDebugMode::Debug, true) => {
                    LapceIcons::DEBUG_DISCONNECT
                },
            };
            return svg;
        }
        LapceIcons::TERMINAL
    }

    pub fn content_tip(&self) -> (String, String) {
        let run_debug = self.run_debug;
        if let Some(name) = run_debug.with(|run_debug| {
            run_debug.as_ref().map(|r| r.config.name.clone())
        }) {
            return (name, "tip".to_owned());
        }
        (self.title.get(), "tip".to_owned())
    }
}

impl KeyPressFocus for TerminalData {
    fn get_mode(&self) -> Mode {
        self.mode.get_untracked()
    }

    fn check_condition(&self, condition: Condition) -> bool {
        matches!(condition, Condition::TerminalFocus | Condition::PanelFocus)
    }

    fn run_command(
        &self,
        command: &crate::command::LapceCommand,
        count: Option<usize>,
        _mods: Modifiers
    ) -> CommandExecuted {
        self.common.view_id.get_untracked().request_paint();
        match &command.kind {
            CommandKind::Move(cmd) => {
                let movement = cmd.to_movement(count);
                let raw = self.raw.get_untracked();
                let mut raw = raw.write();
                let term = &mut raw.term;
                match movement {
                    Movement::Left => {
                        term.vi_motion(ViMotion::Left);
                    },
                    Movement::Right => {
                        term.vi_motion(ViMotion::Right);
                    },
                    Movement::Up => {
                        term.vi_motion(ViMotion::Up);
                    },
                    Movement::Down => {
                        term.vi_motion(ViMotion::Down);
                    },
                    Movement::FirstNonBlank => {
                        term.vi_motion(ViMotion::FirstOccupied);
                    },
                    Movement::StartOfLine => {
                        term.vi_motion(ViMotion::First);
                    },
                    Movement::EndOfLine => {
                        term.vi_motion(ViMotion::Last);
                    },
                    Movement::WordForward => {
                        term.vi_motion(ViMotion::SemanticRight);
                    },
                    Movement::WordEndForward => {
                        term.vi_motion(ViMotion::SemanticRightEnd);
                    },
                    Movement::WordBackward => {
                        term.vi_motion(ViMotion::SemanticLeft);
                    },
                    Movement::Line(line) => {
                        match line {
                            LinePosition::First => {
                                term.scroll_display(Scroll::Top);
                                term.vi_mode_cursor.point.line = term.topmost_line();
                            },
                            LinePosition::Last => {
                                term.scroll_display(Scroll::Bottom);
                                term.vi_mode_cursor.point.line =
                                    term.bottommost_line();
                            },
                            LinePosition::Line(_) => {}
                        };
                    },
                    _ => ()
                };
            },
            CommandKind::Edit(cmd) => match cmd {
                EditCommand::NormalMode => {
                    if !self
                        .common
                        .config
                        .with_untracked(|config| config.core.modal)
                    {
                        return CommandExecuted::Yes;
                    }
                    self.mode.set(Mode::Normal);
                    let raw = self.raw.get_untracked();
                    let mut raw = raw.write();
                    let term = &mut raw.term;
                    if !term.mode().contains(TermMode::VI) {
                        term.toggle_vi_mode();
                    }
                    term.selection = None;
                },
                EditCommand::ToggleVisualMode => {
                    self.toggle_visual(VisualMode::Normal);
                },
                EditCommand::ToggleLinewiseVisualMode => {
                    self.toggle_visual(VisualMode::Linewise);
                },
                EditCommand::ToggleBlockwiseVisualMode => {
                    self.toggle_visual(VisualMode::Blockwise);
                },
                EditCommand::InsertMode => {
                    self.mode.set(Mode::Terminal);
                    let raw = self.raw.get_untracked();
                    let mut raw = raw.write();
                    let term = &mut raw.term;
                    if term.mode().contains(TermMode::VI) {
                        term.toggle_vi_mode();
                    }
                    let scroll = alacritty_terminal::grid::Scroll::Bottom;
                    term.scroll_display(scroll);
                    term.selection = None;
                },
                EditCommand::ClipboardCopy => {
                    let mut clipboard = SystemClipboard::new();
                    if matches!(self.mode.get_untracked(), Mode::Visual(_)) {
                        self.mode.set(Mode::Normal);
                    }
                    let raw = self.raw.get_untracked();
                    let mut raw = raw.write();
                    let term = &mut raw.term;
                    if let Some(content) = term.selection_to_string() {
                        clipboard.put_string(content);
                    }
                    if self.mode.get_untracked() != Mode::Terminal {
                        term.selection = None;
                    }
                },
                EditCommand::ClipboardPaste => {
                    let mut clipboard = SystemClipboard::new();
                    let mut check_bracketed_paste: bool = false;
                    if self.mode.get_untracked() == Mode::Terminal {
                        let raw = self.raw.get_untracked();
                        let mut raw = raw.write();
                        let term = &mut raw.term;
                        term.selection = None;
                        if term.mode().contains(TermMode::BRACKETED_PASTE) {
                            check_bracketed_paste = true;
                        }
                    }
                    if let Some(s) = clipboard.get_string() {
                        if check_bracketed_paste {
                            self.receive_char("\x1b[200~");
                            self.receive_char(&s.replace('\x1b', ""));
                            self.receive_char("\x1b[201~");
                        } else {
                            self.receive_char(&s);
                        }
                    }
                },
                _ => return CommandExecuted::No
            },
            CommandKind::Scroll(cmd) => match cmd {
                ScrollCommand::PageUp => {
                    let raw = self.raw.get_untracked();
                    let mut raw = raw.write();
                    let term = &mut raw.term;
                    let scroll_lines = term.screen_lines() as i32 / 2;
                    term.vi_mode_cursor =
                        term.vi_mode_cursor.scroll(term, scroll_lines);

                    term.scroll_display(alacritty_terminal::grid::Scroll::Delta(
                        scroll_lines
                    ));
                },
                ScrollCommand::PageDown => {
                    let raw = self.raw.get_untracked();
                    let mut raw = raw.write();
                    let term = &mut raw.term;
                    let scroll_lines = -(term.screen_lines() as i32 / 2);
                    term.vi_mode_cursor =
                        term.vi_mode_cursor.scroll(term, scroll_lines);

                    term.scroll_display(alacritty_terminal::grid::Scroll::Delta(
                        scroll_lines
                    ));
                },
                _ => return CommandExecuted::No
            },
            CommandKind::Focus(cmd) => match cmd {
                FocusCommand::SplitVertical
                | FocusCommand::SplitHorizontal
                | FocusCommand::SplitLeft
                | FocusCommand::SplitRight
                | FocusCommand::SplitExchange
                | FocusCommand::SearchForward => {
                    // if let Some(search_string) =
                    // self.find.search_string.as_ref() {
                    //     let mut raw = self.terminal.raw.lock();
                    //     let term = &mut raw.term;
                    //     self.terminal.search_next(
                    //         term,
                    //         search_string,
                    //         Direction::Right,
                    //     );
                    // }
                },
                FocusCommand::SearchBackward => {
                    // if let Some(search_string) =
                    // self.find.search_string.as_ref() {
                    //     let mut raw = self.terminal.raw.lock();
                    //     let term = &mut raw.term;
                    //     self.terminal.search_next(
                    //         term,
                    //         search_string,
                    //         Direction::Left,
                    //     );
                    // }
                },
                _ => return CommandExecuted::No
            },
            _ => return CommandExecuted::No
        };
        CommandExecuted::Yes
    }

    fn receive_char(&self, c: &str) {
        if self.mode.get_untracked() == Mode::Terminal {
            self.common.proxy.terminal_write(
                self.term_id,
                self.raw_id(),
                c.to_string()
            );
            self.raw
                .get_untracked()
                .write()
                .term
                .scroll_display(Scroll::Bottom);
        }
    }
}

impl TerminalData {
    pub fn new(
        cx: Scope,
        workspace: LapceWorkspace,
        profile: Option<TerminalProfile>,
        common: Rc<CommonData>
    ) -> Self {
        Self::new_run_debug(cx, workspace, None, profile, common)
    }

    pub fn new_run_debug(
        cx: Scope,
        workspace: LapceWorkspace,
        run_debug: Option<RunDebugProcess>,
        profile: Option<TerminalProfile>,
        common: Rc<CommonData>
    ) -> Self {
        let cx = cx.create_child();
        let term_id = TermId::next();

        let title = if let Some(profile) = &profile {
            cx.create_rw_signal(profile.name.to_owned())
        } else {
            cx.create_rw_signal(String::from("Default"))
        };

        let launch_error = cx.create_rw_signal(None);

        let (raw, raw_id) = Self::new_raw_terminal(
            &workspace,
            term_id,
            run_debug.as_ref(),
            profile,
            common.clone(),
            launch_error
        );

        let run_debug = cx.create_rw_signal(run_debug);
        let mode = cx.create_rw_signal(Mode::Terminal);
        let visual_mode = cx.create_rw_signal(VisualMode::Normal);
        let raw = cx.create_rw_signal(raw);
        let raw_id = cx.create_rw_signal(raw_id);
        Self {
            scope: cx,
            term_id,
            raw_id,
            workspace,
            raw,
            title,
            run_debug,
            mode,
            visual_mode,
            common,
            launch_error,
            view_id: cx.create_rw_signal(None)
        }
    }

    fn new_raw_terminal(
        workspace: &LapceWorkspace,
        term_id: TermId,
        run_debug: Option<&RunDebugProcess>,
        profile: Option<TerminalProfile>,
        common: Rc<CommonData>,
        launch_error: RwSignal<Option<String>>
    ) -> (Arc<RwLock<RawTerminal>>, u64) {
        log::debug!("term_id={term_id:?} new_raw_terminal");
        let raw_id = TermId::next().to_raw();
        let raw = Arc::new(RwLock::new(RawTerminal::new(
            term_id,
            raw_id,
            common.proxy.clone(),
            common.term_notification_tx.clone()
        )));

        let mut profile = profile.unwrap_or_default();

        if profile.workdir.is_none() {
            profile.workdir = if let Ok(path) = url::Url::from_file_path(
                workspace.path.as_ref().cloned().unwrap_or_default()
            ) {
                Some(path)
            } else {
                None
            };
        }

        let exp_run_debug = run_debug
            .as_ref()
            .map(|run_debug| {
                ExpandedRunDebug::expand(&run_debug.config, run_debug.is_prelaunch)
            })
            .transpose();

        let exp_run_debug = exp_run_debug.unwrap_or_else(|e| {
            let r_name = run_debug
                .as_ref()
                .map(|r| r.config.name.as_str())
                .unwrap_or("Unknown");
            launch_error.set(Some(format!(
                "Failed to expand variables in run debug definition {r_name}: {e}"
            )));
            None
        });

        if let Some(run_debug) = exp_run_debug {
            if let Some(work_dir) = run_debug.work_dir {
                profile.workdir = Some(work_dir);
            }

            profile.environment = run_debug.env;

            profile.command = Some(run_debug.program);
            profile.arguments = run_debug.args;
        }

        {
            let raw = raw.clone();
            if let Err(err) =
                common.term_tx.send((term_id, TermEvent::NewTerminal(raw)))
            {
                log::error!("{:?}", err);
            }
            common.proxy.new_terminal(term_id, raw_id, profile);
        }

        (raw, raw_id)
    }

    pub fn send_keypress(&self, key: &KeyEvent) -> bool {
        if let Some(command) = Self::resolve_key_event(key) {
            self.receive_char(command);
            true
        } else if key.modifiers == Modifiers::ALT
            && matches!(&key.key.logical_key, Key::Character(_))
        {
            if let Key::Character(c) = &key.key.logical_key {
                // In terminal emulators, when the Alt key is combined with another
                // character (such as Alt+a), a leading ESC (Escape,
                // ASCII code 0x1B) character is usually
                // sent followed by a sequence of that character. For example,
                // Alt+a sends \x1Ba.
                self.receive_char("\x1b");
                self.receive_char(c.as_str());
            }
            true
        } else {
            false
        }
    }

    pub fn raw_id(&self) -> u64 {
        self.raw_id.get_untracked()
    }

    pub fn resolve_key_event(key: &KeyEvent) -> Option<&str> {
        let key = key.clone();

        // Generates a `Modifiers` value to check against.
        macro_rules! modifiers {
            (ctrl) => {
                Modifiers::CONTROL
            };

            (alt) => {
                Modifiers::ALT
            };

            (shift) => {
                Modifiers::SHIFT
            };

            ($mod:ident $(| $($mods:ident)|+)?) => {
                modifiers!($mod) $(| modifiers!($($mods)|+) )?
            };
        }

        // Generates modifier values for ANSI sequences.
        macro_rules! modval {
            (shift) => {
                // 1
                "2"
            };
            (alt) => {
                // 2
                "3"
            };
            (alt | shift) => {
                // 1 + 2
                "4"
            };
            (ctrl) => {
                // 4
                "5"
            };
            (ctrl | shift) => {
                // 1 + 4
                "6"
            };
            (alt | ctrl) => {
                // 2 + 4
                "7"
            };
            (alt | ctrl | shift) => {
                // 1 + 2 + 4
                "8"
            };
        }

        // Generates ANSI sequences to move the cursor by one position.
        macro_rules! term_sequence {
            // Generate every modifier combination (except meta)
            ([all], $evt:ident, $no_mod:literal, $pre:literal, $post:literal) => {
                {
                    term_sequence!([], $evt, $no_mod);
                    term_sequence!([shift, alt, ctrl], $evt, $pre, $post);
                    term_sequence!([alt | shift, ctrl | shift, alt | ctrl], $evt, $pre, $post);
                    term_sequence!([alt | ctrl | shift], $evt, $pre, $post);
                    return None;
                }
            };
            // No modifiers
            ([], $evt:ident, $no_mod:literal) => {
                if $evt.modifiers.is_empty() {
                    return Some($no_mod);
                }
            };
            // A single modifier combination
            ([$($mod:ident)|+], $evt:ident, $pre:literal, $post:literal) => {
                if $evt.modifiers == modifiers!($($mod)|+) {
                    return Some(concat!($pre, modval!($($mod)|+), $post));
                }
            };
            // Break down multiple modifiers into a series of single combination branches
            ([$($($mod:ident)|+),+], $evt:ident, $pre:literal, $post:literal) => {
                $(
                    term_sequence!([$($mod)|+], $evt, $pre, $post);
                )+
            };
        }

        match key.key.logical_key {
            Key::Character(ref c) => {
                if key.modifiers == Modifiers::CONTROL {
                    // Convert the character into its index (into a control
                    // character). In essence, this turns
                    // `ctrl+h` into `^h`
                    let str = match c.as_str() {
                        "@" => "\x00",
                        "a" => "\x01",
                        "b" => "\x02",
                        "c" => "\x03",
                        "d" => "\x04",
                        "e" => "\x05",
                        "f" => "\x06",
                        "g" => "\x07",
                        "h" => "\x08",
                        "i" => "\x09",
                        "j" => "\x0a",
                        "k" => "\x0b",
                        "l" => "\x0c",
                        "m" => "\x0d",
                        "n" => "\x0e",
                        "o" => "\x0f",
                        "p" => "\x10",
                        "q" => "\x11",
                        "r" => "\x12",
                        "s" => "\x13",
                        "t" => "\x14",
                        "u" => "\x15",
                        "v" => "\x16",
                        "w" => "\x17",
                        "x" => "\x18",
                        "y" => "\x19",
                        "z" => "\x1a",
                        "[" => "\x1b",
                        "\\" => "\x1c",
                        "]" => "\x1d",
                        "^" => "\x1e",
                        "_" => "\x1f",
                        _ => return None
                    };

                    Some(str)
                } else {
                    None
                }
            },
            Key::Named(NamedKey::Backspace) => {
                Some(if key.modifiers.control() {
                    "\x08" // backspace
                } else if key.modifiers.alt() {
                    "\x1b\x7f"
                } else {
                    "\x7f"
                })
            },

            Key::Named(NamedKey::Tab) => Some("\x09"),
            Key::Named(NamedKey::Enter) => Some("\r"),
            Key::Named(NamedKey::Escape) => Some("\x1b"),

            // The following either expands to `\x1b[X` or `\x1b[1;NX` where N is a
            // modifier value
            Key::Named(NamedKey::ArrowUp) => {
                term_sequence!([all], key, "\x1b[A", "\x1b[1;", "A")
            },
            Key::Named(NamedKey::ArrowDown) => {
                term_sequence!([all], key, "\x1b[B", "\x1b[1;", "B")
            },
            Key::Named(NamedKey::ArrowRight) => {
                term_sequence!([all], key, "\x1b[C", "\x1b[1;", "C")
            },
            Key::Named(NamedKey::ArrowLeft) => {
                term_sequence!([all], key, "\x1b[D", "\x1b[1;", "D")
            },
            Key::Named(NamedKey::Home) => {
                term_sequence!([all], key, "\x1bOH", "\x1b[1;", "H")
            },
            Key::Named(NamedKey::End) => {
                term_sequence!([all], key, "\x1bOF", "\x1b[1;", "F")
            },
            Key::Named(NamedKey::Insert) => {
                term_sequence!([all], key, "\x1b[2~", "\x1b[2;", "~")
            },
            Key::Named(NamedKey::Delete) => {
                term_sequence!([all], key, "\x1b[3~", "\x1b[3;", "~")
            },
            Key::Named(NamedKey::PageUp) => {
                term_sequence!([all], key, "\x1b[5~", "\x1b[5;", "~")
            },
            Key::Named(NamedKey::PageDown) => {
                term_sequence!([all], key, "\x1b[6~", "\x1b[6;", "~")
            },
            _ => None
        }
    }

    pub fn wheel_scroll(&self, delta: f64) {
        let step = self
            .common
            .config
            .with_untracked(|config| config.terminal_line_height() as f64);
        let raw = self.raw.get_untracked();
        let mut raw = raw.write();
        raw.scroll_delta -= delta;
        let delta = (raw.scroll_delta / step) as i32;
        raw.scroll_delta -= delta as f64 * step;
        if delta != 0 {
            let scroll = alacritty_terminal::grid::Scroll::Delta(delta);
            raw.term.scroll_display(scroll);
        }
    }

    fn toggle_visual(&self, visual_mode: VisualMode) {
        if !self
            .common
            .config
            .with_untracked(|config| config.core.modal)
        {
            return;
        }

        match self.mode.get_untracked() {
            Mode::Normal => {
                self.mode.set(Mode::Visual(visual_mode));
                self.visual_mode.set(visual_mode);
            },
            Mode::Visual(_) => {
                if self.visual_mode.get_untracked() == visual_mode {
                    self.mode.set(Mode::Normal);
                } else {
                    self.visual_mode.set(visual_mode);
                }
            },
            _ => ()
        }

        let raw = self.raw.get_untracked();
        let mut raw = raw.write();
        let term = &mut raw.term;
        if !term.mode().contains(TermMode::VI) {
            term.toggle_vi_mode();
        }
        let ty = match visual_mode {
            VisualMode::Normal => SelectionType::Simple,
            VisualMode::Linewise => SelectionType::Lines,
            VisualMode::Blockwise => SelectionType::Block
        };
        let point = term.renderable_content().cursor.point;
        self.toggle_selection(
            term,
            ty,
            point,
            alacritty_terminal::index::Side::Left
        );
        if let Some(selection) = term.selection.as_mut() {
            selection.include_all();
        }
    }

    pub fn toggle_selection(
        &self,
        term: &mut Term<EventProxy>,
        ty: SelectionType,
        point: alacritty_terminal::index::Point,
        side: alacritty_terminal::index::Side
    ) {
        match &mut term.selection {
            Some(selection) if selection.ty == ty && !selection.is_empty() => {
                term.selection = None;
            },
            Some(selection) if !selection.is_empty() => {
                selection.ty = ty;
            },
            _ => self.start_selection(term, ty, point, side)
        }
    }

    fn start_selection(
        &self,
        term: &mut Term<EventProxy>,
        ty: SelectionType,
        point: alacritty_terminal::index::Point,
        side: alacritty_terminal::index::Side
    ) {
        term.selection = Some(Selection::new(ty, point, side));
    }

    pub fn new_process(&self, run_debug: Option<RunDebugProcess>) {
        let (width, height) = {
            let raw = self.raw.get_untracked();
            let raw = raw.read();
            let width = raw.term.columns();
            let height = raw.term.screen_lines();
            (width, height)
        };

        let (raw, raw_id) = Self::new_raw_terminal(
            &self.workspace,
            self.term_id,
            run_debug.as_ref(),
            None,
            self.common.clone(),
            self.launch_error
        );
        let term_size = TermSize::new(width, height);
        raw.write().term.resize(term_size);

        self.raw.set(raw);
        self.raw_id.set(raw_id);
        self.run_debug.set(run_debug);

        self.common
            .proxy
            .terminal_resize(self.term_id, width, height);
    }

    pub fn stop(&self) {
        if let Some(dap_id) = self.run_debug.with_untracked(|x| {
            if let Some(process) = x {
                if !process.is_prelaunch && process.mode == RunDebugMode::Debug {
                    return Some(process.config.dap_id);
                }
            }
            None
        }) {
            self.common.proxy.dap_stop(dap_id);
        }
        self.common
            .proxy
            .terminal_close(self.term_id, self.raw_id());
    }
}

/// [`RunDebugConfig`] with expanded out program/arguments/etc. Used for
/// creating the terminal.
#[derive(Debug, Clone)]
pub struct ExpandedRunDebug {
    pub work_dir: Option<Url>,
    pub env:      Option<HashMap<String, String>>,
    pub program:  String,
    pub args:     Option<Vec<String>>
}
impl ExpandedRunDebug {
    pub fn expand(
        run_debug: &RunDebugConfig,
        is_prelaunch: bool
    ) -> anyhow::Result<Self> {
        // Get the current working directory variable, which can container
        // ${workspace}

        let work_dir = run_debug
            .cwd
            .as_ref()
            .map(PathBuf::from)
            .and_then(|x| Url::from_file_path(x).ok());
        // let work_dir =
        //     Url::from_file_path(PathBuf::from(run_debug.cwd.as_ref().?)).ok();

        let prelaunch = is_prelaunch
            .then_some(run_debug.prelaunch.as_ref())
            .flatten();

        let env = run_debug.env.clone();

        // TODO: replace some variables in the args
        let (program, args) =
            if let Some(debug_command) = run_debug.debug_command.as_ref() {
                let mut args = debug_command.to_owned();
                let command = args.first().cloned().unwrap_or_default();
                if !args.is_empty() {
                    args.remove(0);
                }

                let args = if !args.is_empty() { Some(args) } else { None };
                (command, args)
            } else if let Some(prelaunch) = prelaunch {
                (prelaunch.program.clone(), prelaunch.args.clone())
            } else {
                (run_debug.program.clone(), run_debug.args.clone())
            };
        let program = if program == "${lapce}" {
            std::env::current_exe()
                .map_err(|e| {
                    anyhow!(
                        "Failed to get current exe for ${{lapce}} run and debug: \
                         {e}"
                    )
                })?
                .to_str()
                .ok_or_else(|| anyhow!("Failed to convert ${{lapce}} path to str"))?
                .to_string()
        } else {
            program
        };

        // if program.contains("${workspace}") {
        //     if let Some(workspace) = workspace.path.as_ref().and_then(|x|
        // x.to_str())     {
        //         program = program.replace("${workspace}", workspace);
        //     }
        // }

        // if let Some(args) = &mut args {
        //     for arg in args {
        //         // Replace all mentions of ${workspace} with the current workspace
        // path         if arg.contains("${workspace}") {
        //             if let Some(workspace) =
        //                 workspace.path.as_ref().and_then(|x| x.to_str())
        //             {
        //                 *arg = arg.replace("${workspace}", workspace);
        //             }
        //         }
        //     }
        // }

        Ok(ExpandedRunDebug {
            work_dir,
            env,
            program,
            args
        })
    }
}
